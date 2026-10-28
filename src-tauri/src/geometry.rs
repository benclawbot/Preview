use crate::error::{AppError, Result};
use crate::model::{PageInfo, Point, Rect};
use lopdf::{Dictionary, Document, Object, ObjectId};

/// Page geometry in two coordinate systems:
///
/// * PDF user space, in points, with the CropBox origin at the bottom-left.
/// * Display space, in points, with the top-left origin, after `/Rotate`.
///
/// The UI only sees display space. Annotation dictionaries accept PDF user
/// space, so every annotation crossing the boundary goes through here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeom {
    pub crop_x: f32,
    pub crop_y: f32,
    pub raw_width: f32,
    pub raw_height: f32,
    pub rotation: i32,
}

impl Default for PageGeom {
    fn default() -> Self {
        Self {
            crop_x: 0.0,
            crop_y: 0.0,
            raw_width: 612.0,
            raw_height: 792.0,
            rotation: 0,
        }
    }
}

impl PageGeom {
    pub fn width(&self) -> f32 {
        if self.is_quarter_turn() {
            self.raw_height
        } else {
            self.raw_width
        }
    }

    pub fn height(&self) -> f32 {
        if self.is_quarter_turn() {
            self.raw_width
        } else {
            self.raw_height
        }
    }

    pub fn is_quarter_turn(&self) -> bool {
        self.rotation == 90 || self.rotation == 270
    }

    /// PDF user space -> display space (top-left origin).
    pub fn to_display(&self, x: f32, y: f32) -> Point {
        let px = x - self.crop_x;
        let py = y - self.crop_y;
        match self.rotation {
            90 => Point::new(py, px),
            180 => Point::new(self.raw_width - px, py),
            270 => Point::new(self.raw_height - py, self.raw_width - px),
            _ => Point::new(px, self.raw_height - py),
        }
    }

    /// Display space (top-left origin) -> PDF user space.
    pub fn to_user(&self, x: f32, y: f32) -> Point {
        let (px, py) = match self.rotation {
            90 => (y, x),
            180 => (self.raw_width - x, y),
            270 => (self.raw_width - y, self.raw_height - x),
            _ => (x, self.raw_height - y),
        };
        Point::new(px + self.crop_x, py + self.crop_y)
    }

    pub fn rect_to_user(&self, rect: Rect) -> Rect {
        let rect = rect.normalized();
        let a = self.to_user(rect.x, rect.y);
        let b = self.to_user(rect.right(), rect.bottom());
        Rect::new(
            a.x.min(b.x),
            a.y.min(b.y),
            (a.x - b.x).abs(),
            (a.y - b.y).abs(),
        )
    }

    pub fn rect_to_display(&self, rect: Rect) -> Rect {
        let rect = rect.normalized();
        let a = self.to_display(rect.x, rect.y);
        let b = self.to_display(rect.right(), rect.bottom());
        Rect::new(
            a.x.min(b.x),
            a.y.min(b.y),
            (a.x - b.x).abs(),
            (a.y - b.y).abs(),
        )
    }

    pub fn info(&self, index: u32) -> PageInfo {
        PageInfo {
            index,
            width: self.width(),
            height: self.height(),
            rotation: self.rotation,
            crop_x: self.crop_x,
            crop_y: self.crop_y,
            raw_width: self.raw_width,
            raw_height: self.raw_height,
        }
    }
}

fn resolve<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

/// Reads an inheritable page attribute, following `/Parent` links.
fn inherited<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut current = dict;
    // Guard against malformed parent cycles.
    for _ in 0..64 {
        if let Ok(object) = current.get(key) {
            return resolve(doc, object);
        }
        let parent = current
            .get(b"Parent")
            .ok()
            .and_then(|o| o.as_reference().ok())?;
        current = doc.get_dictionary(parent).ok()?;
    }
    None
}

fn numbers(object: &Object) -> Option<Vec<f32>> {
    object
        .as_array()
        .ok()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_float()
                        .ok()
                        .or_else(|| item.as_i64().ok().map(|v| v as f32))
                })
                .collect::<Vec<_>>()
        })
        .filter(|values| values.len() >= 4)
}

fn box_dimensions(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<(f32, f32, f32, f32)> {
    let object = inherited(doc, dict, key)?;
    let values = numbers(object)?;
    let left = values[0];
    let bottom = values[1];
    let right = values[2];
    let top = values[3];
    if !left.is_finite()
        || !bottom.is_finite()
        || !right.is_finite()
        || !top.is_finite()
        || right <= left
        || top <= bottom
    {
        return None;
    }
    Some((left, bottom, right - left, top - bottom))
}

pub fn page_geometry(doc: &Document, page_id: ObjectId) -> Result<PageGeom> {
    let dict = doc
        .get_dictionary(page_id)
        .map_err(|_| AppError::Other(format!("page object {:?} is not a dictionary", page_id)))?;

    let (crop_x, crop_y, raw_width, raw_height) = box_dimensions(doc, dict, b"CropBox")
        .or_else(|| box_dimensions(doc, dict, b"MediaBox"))
        .unwrap_or((0.0, 0.0, 612.0, 792.0));

    let rotation = inherited(doc, dict, b"Rotate")
        .and_then(|object| object.as_i64().ok())
        .map(|value| (value as i32).rem_euclid(360))
        .map(|value| ((value as f32 / 90.0).round() as i32 * 90).rem_euclid(360))
        .unwrap_or(0);

    Ok(PageGeom {
        crop_x,
        crop_y,
        raw_width,
        raw_height,
        rotation,
    })
}

pub fn all_page_geometries(doc: &Document) -> Vec<PageGeom> {
    doc.get_pages()
        .values()
        .enumerate()
        .map(|(index, id)| {
            page_geometry(doc, *id).unwrap_or_else(|_| {
                let _ = index;
                PageGeom::default()
            })
        })
        .collect()
}

/// Resolved page entries in page order, with their parent page-tree node.
#[derive(Debug, Clone)]
pub struct PageRef {
    pub id: ObjectId,
    pub parent: Option<ObjectId>,
    pub index_in_parent: usize,
}

/// Walks the page tree and returns every leaf page in document order.
pub fn page_refs(doc: &Document) -> Vec<PageRef> {
    let mut out = Vec::new();
    let Ok(root) = doc
        .catalog()
        .and_then(|c| c.get(b"Pages").and_then(Object::as_reference))
    else {
        return out;
    };
    walk_page_tree(doc, root, &mut out);
    out
}

fn walk_page_tree(doc: &Document, node: ObjectId, out: &mut Vec<PageRef>) {
    let Ok(dict) = doc.get_dictionary(node) else {
        return;
    };
    let kids = dict.get(b"Kids").ok().and_then(|o| o.as_array().ok());
    let Some(kids) = kids else {
        // A leaf without Kids is malformed, but its page object is still usable.
        out.push(PageRef {
            id: node,
            parent: dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok()),
            index_in_parent: 0,
        });
        return;
    };

    for (index, kid) in kids.iter().enumerate() {
        let Ok(id) = kid.as_reference() else {
            continue;
        };
        let Ok(child) = doc.get_dictionary(id) else {
            continue;
        };
        let child_type = child.get(b"Type").ok().and_then(|o| o.as_name().ok());
        let is_pages = child_type == Some(b"Pages".as_slice()) || child.has(b"Kids");
        if is_pages {
            walk_page_tree(doc, id, out);
        } else {
            out.push(PageRef {
                id,
                parent: Some(node),
                index_in_parent: index,
            });
        }
    }
}

/// Recomputes `/Count` on every page-tree node. Called after any structural
/// change; it is cheap and keeps readers happy even when we only moved one kid.
pub fn recompute_page_counts(doc: &mut Document) -> Result<()> {
    let root = doc
        .catalog()
        .and_then(|c| c.get(b"Pages").and_then(Object::as_reference))?;
    recompute_node(doc, root)?;
    Ok(())
}

fn recompute_node(doc: &mut Document, node: ObjectId) -> Result<u32> {
    let dict = doc.get_dictionary(node)?.clone();
    let Some(kids) = dict
        .get(b"Kids")
        .ok()
        .and_then(|o| o.as_array().ok())
        .cloned()
    else {
        return Ok(1);
    };
    let mut count = 0u32;
    for kid in kids {
        if let Ok(id) = kid.as_reference() {
            count += recompute_node(doc, id)?;
        }
    }
    doc.get_dictionary_mut(node)?.set("Count", count as i64);
    Ok(count)
}

/// Removes a page reference from its parent's Kids array without deleting the
/// page object, so undo can put it back byte-for-byte.
pub fn detach_page(doc: &mut Document, page: &PageRef) -> Result<()> {
    let Some(parent) = page.parent else {
        return Err(AppError::Other("page has no parent page-tree node".into()));
    };
    let parent_dict = doc.get_dictionary_mut(parent)?;
    let kids = parent_dict.get_mut(b"Kids")?.as_array_mut()?;
    let position = kids
        .iter()
        .position(|kid| kid.as_reference().ok() == Some(page.id));
    match position {
        Some(position) => {
            kids.remove(position);
            Ok(())
        }
        None => Err(AppError::Other("page is missing from its parent".into())),
    }
}

pub fn attach_page(
    doc: &mut Document,
    object: Object,
    id: ObjectId,
    parent: Option<ObjectId>,
    index: usize,
) -> Result<()> {
    doc.objects.insert(id, object);
    let Some(parent) = parent else {
        return Err(AppError::Other(
            "cannot insert page without a parent".into(),
        ));
    };
    let parent_dict = doc.get_dictionary_mut(parent)?;
    let kids = parent_dict.get_mut(b"Kids")?.as_array_mut()?;
    let position = index.min(kids.len());
    kids.insert(position, Object::Reference(id));
    Ok(())
}

/// Moves a page within the existing page tree. This preserves inherited
/// attributes by keeping the page in its original parent whenever possible.
pub fn reorder_page(doc: &mut Document, from: u32, to: u32) -> Result<()> {
    let refs = page_refs(doc);
    let from = from as usize;
    let to = to as usize;
    if from >= refs.len() {
        return Err(AppError::Other("page index out of bounds".into()));
    }
    let target = to.min(refs.len().saturating_sub(1));
    if from == target {
        return Ok(());
    }
    let moving = refs[from].clone();
    detach_page(doc, &moving)?;

    // Find the destination *after* detaching so indexes refer to the final list.
    let refs_after = page_refs(doc);
    let (parent, index) = if target >= refs_after.len() {
        match refs_after.last() {
            Some(last) => (last.parent, last.index_in_parent + 1),
            None => (moving.parent, 0),
        }
    } else {
        let destination = &refs_after[target];
        (destination.parent, destination.index_in_parent)
    };

    let parent_dict = match parent {
        Some(id) => doc
            .get_dictionary_mut(id)
            .map_err(|error| AppError::Other(error.to_string()))?,
        None => return Err(AppError::Other("destination page has no parent".into())),
    };
    let kids = parent_dict.get_mut(b"Kids")?.as_array_mut()?;
    let index = index.min(kids.len());
    kids.insert(index, Object::Reference(moving.id));
    recompute_page_counts(doc)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_maps_round_trip() {
        for rotation in [0, 90, 180, 270] {
            let geom = PageGeom {
                crop_x: 10.0,
                crop_y: 20.0,
                raw_width: 300.0,
                raw_height: 500.0,
                rotation,
            };
            for (x, y) in [(0.0, 0.0), (12.0, 34.0), (300.0, 500.0)] {
                let display = geom.to_display(x, y);
                let user = geom.to_user(display.x, display.y);
                assert!((user.x - x).abs() < 0.001, "rot {rotation} x");
                assert!((user.y - y).abs() < 0.001, "rot {rotation} y");
            }
        }
    }

    #[test]
    fn quarter_turn_swaps_dimensions() {
        let geom = PageGeom {
            rotation: 90,
            raw_width: 300.0,
            raw_height: 500.0,
            ..Default::default()
        };
        assert_eq!(geom.width(), 500.0);
        assert_eq!(geom.height(), 300.0);
    }
}
