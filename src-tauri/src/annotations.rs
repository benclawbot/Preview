use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};

use crate::error::{AppError, Result};
use crate::geometry::PageGeom;
use crate::model::{Annotation, AnnotationKind, Point, Rect};

pub fn parse_hex_color(value: &str) -> [f32; 3] {
    let value = value.trim_start_matches('#');
    if value.len() >= 6 {
        let r = u8::from_str_radix(&value[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&value[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&value[4..6], 16).unwrap_or(0);
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
    } else {
        [1.0, 0.0, 0.0]
    }
}

fn color_hex(values: &[f32]) -> String {
    let component = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    match values.len() {
        1 => {
            let gray = component(values[0]);
            format!("#{gray:02x}{gray:02x}{gray:02x}")
        }
        3 | 4 => {
            let (r, g, b) = if values.len() == 4 {
                let k = values[3];
                (
                    (1.0 - values[0]) * (1.0 - k),
                    (1.0 - values[1]) * (1.0 - k),
                    (1.0 - values[2]) * (1.0 - k),
                )
            } else {
                (values[0], values[1], values[2])
            };
            format!(
                "#{:02x}{:02x}{:02x}",
                component(r),
                component(g),
                component(b)
            )
        }
        _ => "#e03131".to_string(),
    }
}

fn object_f32(doc: &Document, object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| object_f32(doc, o)),
        _ => None,
    }
}

fn dict_f32(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<f32> {
    dict.get(key).ok().and_then(|o| object_f32(doc, o))
}

fn numbers_of(doc: &Document, object: &Object) -> Vec<f32> {
    let resolved = match object {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(object),
        other => other,
    };
    resolved
        .as_array()
        .map(|array| array.iter().filter_map(|o| object_f32(doc, o)).collect())
        .unwrap_or_default()
}

fn name_of(object: &Object) -> Option<String> {
    object
        .as_name()
        .ok()
        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
}

fn text_of(object: &Object) -> Option<String> {
    lopdf::decode_text_string(object).ok().or_else(|| {
        object
            .as_str()
            .ok()
            .map(|s| String::from_utf8_lossy(s).to_string())
    })
}

fn utf16_hex(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xfe, 0xff];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

fn pdf_literal(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.extend_from_slice(b"\\\\"),
            '(' => out.extend_from_slice(b"\\("),
            ')' => out.extend_from_slice(b"\\)"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) <= 0xff => out.push(c as u8),
            _ => out.push(b'?'),
        }
    }
    out
}

pub struct ExistingAnnotations {
    pub annotations: Vec<Annotation>,
    pub objects: HashMap<String, ObjectId>,
}

/// Reads supported markup annotations from the PDF. Unsupported types (links,
/// widgets, stamps, ...) are left untouched in the file and simply not shown
/// in the editing overlay yet.
pub fn read_annotations(doc: &Document, geometries: &[PageGeom]) -> ExistingAnnotations {
    let mut annotations = Vec::new();
    let mut objects = HashMap::new();
    let mut used_ids: HashSet<String> = HashSet::new();

    for (page_index, page_id) in doc.get_pages().values().enumerate() {
        let geom = geometries.get(page_index).copied().unwrap_or_default();
        let Ok(page_dict) = doc.get_dictionary(*page_id) else {
            continue;
        };
        let Ok(annots_object) = page_dict.get(b"Annots") else {
            continue;
        };
        let annots = match annots_object {
            Object::Reference(id) => doc
                .get_object(*id)
                .ok()
                .and_then(|o| o.as_array().ok())
                .cloned()
                .unwrap_or_default(),
            other => other.as_array().cloned().unwrap_or_default(),
        };

        for entry in annots {
            let object_id = entry.as_reference().ok();
            let dict = match &entry {
                Object::Reference(id) => doc.get_dictionary(*id).ok(),
                Object::Dictionary(dict) => Some(dict),
                _ => None,
            };
            let Some(dict) = dict else {
                continue;
            };
            let Some(subtype) = dict.get(b"Subtype").ok().and_then(name_of) else {
                continue;
            };
            let kind = match subtype.as_str() {
                "Highlight" => AnnotationKind::Highlight,
                "Underline" => AnnotationKind::Underline,
                "StrikeOut" => AnnotationKind::Strikeout,
                "Square" => AnnotationKind::Square,
                "Circle" => AnnotationKind::Circle,
                "Line" => {
                    let arrow = dict
                        .get(b"LE")
                        .ok()
                        .and_then(|o| o.as_array().ok())
                        .map(|array| {
                            array.iter().any(|item| {
                                name_of(item)
                                    .map(|name| name.contains("Arrow"))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false);
                    if arrow {
                        AnnotationKind::Arrow
                    } else {
                        AnnotationKind::Line
                    }
                }
                "FreeText" => AnnotationKind::FreeText,
                "Text" => AnnotationKind::Note,
                "Ink" => AnnotationKind::Ink,
                "Stamp" if dict.get(b"PreviewSignature").is_ok() => AnnotationKind::Signature,
                _ => continue,
            };

            let Ok(rect_object) = dict.get(b"Rect") else {
                continue;
            };
            let rect_values = numbers_of(doc, rect_object);
            if rect_values.len() < 4 {
                continue;
            }
            let rect = geom.rect_to_display(Rect::new(
                rect_values[0],
                rect_values[1],
                rect_values[2] - rect_values[0],
                rect_values[3] - rect_values[1],
            ));

            let id = dict
                .get(b"NM")
                .ok()
                .and_then(text_of)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    object_id.map(|(number, generation)| format!("ann-{number}-{generation}"))
                })
                .unwrap_or_else(|| format!("ann-{page_index}-{}", annotations.len()));
            let mut id = id;
            let mut suffix = 1;
            while used_ids.contains(&id) {
                id = format!("{id}-{suffix}");
                suffix += 1;
            }
            used_ids.insert(id.clone());

            let color = dict
                .get(b"C")
                .ok()
                .map(|o| numbers_of(doc, o))
                .filter(|values| !values.is_empty())
                .map(|values| color_hex(&values))
                .unwrap_or_else(|| match kind {
                    AnnotationKind::Highlight => "#ffe066".into(),
                    AnnotationKind::Note => "#ffd43b".into(),
                    _ => "#e03131".into(),
                });

            let fill = dict
                .get(b"IC")
                .ok()
                .map(|o| numbers_of(doc, o))
                .filter(|values| !values.is_empty())
                .map(|values| color_hex(&values));

            let width = dict
                .get(b"BS")
                .ok()
                .and_then(|o| match o {
                    Object::Reference(id) => doc.get_object(*id).ok(),
                    other => Some(other),
                })
                .and_then(|o| o.as_dict().ok())
                .and_then(|bs| dict_f32(doc, bs, b"W"))
                .or_else(|| {
                    dict.get(b"Border")
                        .ok()
                        .map(|o| numbers_of(doc, o))
                        .and_then(|values| values.get(2).copied())
                })
                .unwrap_or(2.0);

            let opacity = dict_f32(doc, dict, b"CA").unwrap_or(if kind.is_text_markup() {
                0.42
            } else {
                1.0
            });

            let contents = dict.get(b"Contents").ok().and_then(text_of);
            let font_size = dict
                .get(b"DA")
                .ok()
                .and_then(text_of)
                .and_then(|da| {
                    let tokens: Vec<&str> = da.split_whitespace().collect();
                    tokens
                        .windows(2)
                        .find(|pair| pair[1] == "Tf")
                        .and_then(|pair| pair[0].parse::<f32>().ok())
                })
                .unwrap_or(12.0);

            let mut points = Vec::new();
            if matches!(kind, AnnotationKind::Line | AnnotationKind::Arrow) {
                if let Ok(line) = dict.get(b"L") {
                    let values = numbers_of(doc, line);
                    if values.len() >= 4 {
                        points.push(geom.to_display(values[0], values[1]));
                        points.push(geom.to_display(values[2], values[3]));
                    }
                }
            } else if kind == AnnotationKind::Ink {
                if let Ok(ink_list) = dict.get(b"InkList") {
                    if let Ok(strokes) = ink_list.as_array() {
                        if let Some(stroke) = strokes.first() {
                            let values = numbers_of(doc, stroke);
                            for pair in values.chunks_exact(2) {
                                points.push(geom.to_display(pair[0], pair[1]));
                            }
                        }
                    }
                }
            }

            let mut annotation = Annotation::new(id.clone(), page_index as u32, kind, rect);
            annotation.color = color;
            annotation.fill = fill;
            annotation.width = width;
            annotation.opacity = opacity;
            annotation.contents = contents;
            annotation.font_size = font_size;
            annotation.points = points;
            annotation.image_data = dict
                .get(b"PreviewSignature")
                .ok()
                .and_then(|value| value.as_str().ok())
                .map(|value| String::from_utf8_lossy(value).to_string());
            annotation.existing = true;

            if let Some(object_id) = object_id {
                objects.insert(id, object_id);
            }
            annotations.push(annotation);
        }
    }

    ExistingAnnotations {
        annotations,
        objects,
    }
}

fn page_annots_mut<'a>(doc: &'a mut Document, page_id: ObjectId) -> Result<&'a mut Vec<Object>> {
    let annots_reference = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|dict| dict.get(b"Annots").ok())
        .and_then(|object| object.as_reference().ok());

    if let Some(id) = annots_reference {
        let object = doc.get_object_mut(id)?;
        return if object.as_array_mut().is_ok() {
            Ok(object.as_array_mut()?)
        } else {
            Err(AppError::Other("page /Annots is not an array".into()))
        };
    }

    let page = doc.get_dictionary_mut(page_id)?;
    if !page.has(b"Annots") {
        page.set("Annots", Vec::<Object>::new());
    }
    let object = page.get_mut(b"Annots")?;
    Ok(object.as_array_mut()?)
}

fn remove_annots_reference(
    doc: &mut Document,
    page_id: ObjectId,
    object_id: ObjectId,
) -> Result<()> {
    let annots = page_annots_mut(doc, page_id)?;
    annots.retain(|entry| entry.as_reference().ok() != Some(object_id));
    Ok(())
}

fn add_annots_reference(doc: &mut Document, page_id: ObjectId, object_id: ObjectId) -> Result<()> {
    let annots = page_annots_mut(doc, page_id)?;
    if !annots
        .iter()
        .any(|entry| entry.as_reference().ok() == Some(object_id))
    {
        annots.push(Object::Reference(object_id));
    }
    Ok(())
}

fn font_resource(bold: bool, italic: bool) -> (&'static str, &'static str) {
    match (bold, italic) {
        (true, true) => ("HelvB", "Helvetica-BoldOblique"),
        (true, false) => ("HelvB", "Helvetica-Bold"),
        (false, true) => ("HelvI", "Helvetica-Oblique"),
        (false, false) => ("Helv", "Helvetica"),
    }
}

fn circle_path(x: f32, y: f32, w: f32, h: f32) -> String {
    let rx = w / 2.0;
    let ry = h / 2.0;
    let cx = x + rx;
    let cy = y + ry;
    let k = 0.552_284_8;
    format!(
        "{cx} {cy_plus} m \
         {cx_plus} {cy_plus} {cx_plus} {cy_plus_k} {cx_plus} {cy} c \
         {cx_plus} {cy_minus_k} {cx_plus} {cy_minus} {cx} {cy_minus} c \
         {cx_minus} {cy_minus} {cx_minus} {cy_minus_k} {cx_minus} {cy} c \
         {cx_minus} {cy_plus_k} {cx_minus} {cy_plus} {cx} {cy_plus} c h",
        cx_plus = cx + rx,
        cx_minus = cx - rx,
        cy_plus = cy + ry,
        cy_minus = cy - ry,
        cy_plus_k = cy + ry * k,
        cy_minus_k = cy - ry * k,
    )
}

fn wrap_text(text: &str, width: f32, font_size: f32) -> Vec<String> {
    let max_chars = ((width / (font_size * 0.52)).floor() as usize).max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + word.chars().count() <= max_chars {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(line);
                line = word.to_string();
            }
        }
        lines.push(line);
    }
    lines
}

/// Builds the `/AP` stream for an annotation. Coordinates inside the stream are
/// local to the annotation rectangle; viewers map the form onto `/Rect`.
fn make_appearance(
    doc: &mut Document,
    annotation: &Annotation,
    geom: PageGeom,
) -> Result<ObjectId> {
    let user_rect = geom.rect_to_user(annotation.rect.normalized());
    let user_rect = Rect {
        x: user_rect.x,
        y: user_rect.y,
        width: user_rect.width.max(1.0),
        height: user_rect.height.max(1.0),
    };
    let w = user_rect.width;
    let h = user_rect.height;
    let (r, g, b) = {
        let [r, g, b] = parse_hex_color(&annotation.color);
        (r, g, b)
    };
    let (fill_r, fill_g, fill_b) = annotation
        .fill
        .as_deref()
        .map(parse_hex_color)
        .map(|[r, g, b]| (r, g, b))
        .unwrap_or((1.0, 1.0, 1.0));
    let stroke = annotation.width.max(0.1);

    let mut content = String::new();
    let mut resources = dictionary! {};
    let mut ext_gstates = dictionary! {};
    let mut fonts = dictionary! {};
    let mut has_transparency = false;

    let to_local = |point: Point| -> Point {
        let user = geom.to_user(point.x, point.y);
        Point::new(user.x - user_rect.x, user.y - user_rect.y)
    };

    if annotation.kind == AnnotationKind::Signature {
        let encoded = annotation
            .image_data
            .as_deref()
            .ok_or_else(|| AppError::Message("signature image is missing".into()))?;
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
            .map_err(|error| {
                AppError::Message(format!("signature image data is invalid: {error}"))
            })?;
        let signature = image::load_from_memory(&png)?.to_rgba8();
        let mut rgb = Vec::with_capacity((signature.width() * signature.height() * 3) as usize);
        let mut alpha = Vec::with_capacity((signature.width() * signature.height()) as usize);
        for pixel in signature.pixels() {
            rgb.extend_from_slice(&pixel.0[..3]);
            alpha.push(pixel[3]);
        }
        let alpha_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => signature.width() as i64,
                "Height" => signature.height() as i64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8i64,
            },
            alpha,
        ));
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => signature.width() as i64,
                "Height" => signature.height() as i64,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8i64,
                "SMask" => alpha_id,
            },
            rgb,
        ));
        resources.set("XObject", dictionary! { "Im0" => image_id });
        content.push_str(&format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q\n"));
    }

    match annotation.kind {
        AnnotationKind::Highlight => {
            if annotation.opacity < 1.0 {
                ext_gstates.set(
                    "GS0",
                    dictionary! {
                        "Type" => "ExtGState",
                        "ca" => annotation.opacity,
                        "CA" => 1.0f32,
                    },
                );
                has_transparency = true;
            }
            content.push_str(&format!(
                "q {} gs {r} {g} {b} rg 0 0 {w} {h} re f Q\n",
                if has_transparency { "/GS0" } else { "" }
            ));
            if !has_transparency {
                content = format!("q {r} {g} {b} rg 0 0 {w} {h} re f Q\n");
            }
        }
        AnnotationKind::Underline | AnnotationKind::Strikeout => {
            let y = if annotation.kind == AnnotationKind::Underline {
                1.0
            } else {
                h / 2.0
            };
            content.push_str(&format!(
                "q {r} {g} {b} RG {stroke} w 0 {y} m {w} {y} l S Q\n"
            ));
        }
        AnnotationKind::Square => {
            if annotation.fill.is_some() {
                content.push_str(&format!(
                    "q {r} {g} {b} RG {fill_r} {fill_g} {fill_b} rg {stroke} w \
                     {half} {half} {w2} {h2} re B Q\n",
                    half = stroke / 2.0,
                    w2 = (w - stroke).max(0.1),
                    h2 = (h - stroke).max(0.1),
                ));
            } else {
                content.push_str(&format!(
                    "q {r} {g} {b} RG {stroke} w {half} {half} {w2} {h2} re S Q\n",
                    half = stroke / 2.0,
                    w2 = (w - stroke).max(0.1),
                    h2 = (h - stroke).max(0.1),
                ));
            }
        }
        AnnotationKind::Circle => {
            let path = circle_path(
                stroke / 2.0,
                stroke / 2.0,
                (w - stroke).max(0.1),
                (h - stroke).max(0.1),
            );
            let paint = if annotation.fill.is_some() {
                format!("q {r} {g} {b} RG {fill_r} {fill_g} {fill_b} rg {stroke} w {path} B Q\n")
            } else {
                format!("q {r} {g} {b} RG {stroke} w {path} S Q\n")
            };
            content.push_str(&paint);
        }
        AnnotationKind::Line | AnnotationKind::Arrow => {
            let points: Vec<Point> = if annotation.points.len() >= 2 {
                annotation.points.iter().copied().map(to_local).collect()
            } else {
                vec![Point::new(0.0, 0.0), Point::new(w, h)]
            };
            let start = points[0];
            let end = *points.last().unwrap();
            content.push_str(&format!(
                "q {r} {g} {b} RG {stroke} w 1 J {sx} {sy} m {ex} {ey} l S Q\n",
                sx = start.x,
                sy = start.y,
                ex = end.x,
                ey = end.y,
            ));
            if annotation.kind == AnnotationKind::Arrow {
                let dx = end.x - start.x;
                let dy = end.y - start.y;
                let length = (dx * dx + dy * dy).sqrt().max(0.001);
                let ux = dx / length;
                let uy = dy / length;
                let head = (stroke * 4.0).max(6.0).min(length);
                let left = Point::new(
                    end.x - ux * head - uy * head * 0.5,
                    end.y - uy * head + ux * head * 0.5,
                );
                let right = Point::new(
                    end.x - ux * head + uy * head * 0.5,
                    end.y - uy * head - ux * head * 0.5,
                );
                content.push_str(&format!(
                    "q {r} {g} {b} rg {} {} m {} {} l {} {} l h f Q\n",
                    end.x, end.y, left.x, left.y, right.x, right.y
                ));
            }
        }
        AnnotationKind::FreeText => {
            let (font_key, base_font) = font_resource(annotation.bold, annotation.italic);
            fonts.set(
                font_key,
                dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => base_font,
                    "Encoding" => "WinAnsiEncoding",
                },
            );
            if annotation.fill.is_some() {
                content.push_str(&format!(
                    "q {fill_r} {fill_g} {fill_b} rg 0 0 {w} {h} re f Q\n"
                ));
            }
            let text = annotation.contents.clone().unwrap_or_default();
            let font_size = annotation.font_size.max(6.0);
            let lines = wrap_text(&text, w - 8.0, font_size);
            content.push_str(&format!(
                "BT /{font_key} {font_size} Tf {r} {g} {b} rg 4 {} Td {leading} TL\n",
                (h - font_size - 4.0).max(2.0),
                leading = font_size * 1.25,
            ));
            for (index, line) in lines.iter().enumerate() {
                if index > 0 {
                    content.push_str("T* ");
                }
                content.push('(');
                content.push_str(&String::from_utf8_lossy(&pdf_literal(line)));
                content.push_str(") Tj ");
            }
            content.push_str("ET\n");
        }
        AnnotationKind::Note => {
            let fold = (w.min(h) * 0.32).max(4.0);
            content.push_str(&format!(
                "q {r} {g} {b} RG 0.9 w {fill_r} {fill_g} {fill_b} rg \
                 0.5 0.5 {body_w} {body_h} re B\n",
                body_w = (w - 1.0).max(1.0),
                body_h = (h - 1.0).max(1.0),
            ));
            content.push_str(&format!(
                "q {r} {g} {b} rg {fx} {fy} m {fx2} {fy} l {fx2} {fy2} l f Q\n",
                fx = w - fold,
                fy = h - 0.5,
                fx2 = w - 0.5,
                fy2 = h - fold,
            ));
            content.push_str(&format!(
                "q 0.25 0.25 0.25 RG 0.8 w {x1} {y1} m {x2} {y1} l S \
                 {x1} {y2} m {x3} {y2} l S Q\n",
                x1 = w * 0.18,
                x2 = w * 0.82,
                x3 = w * 0.62,
                y1 = h * 0.38,
                y2 = h * 0.62,
            ));
        }
        AnnotationKind::Ink => {
            if annotation.points.len() >= 2 {
                let points: Vec<Point> = annotation.points.iter().copied().map(to_local).collect();
                content.push_str(&format!("q {r} {g} {b} RG {stroke} w 1 J 1 j "));
                let first = points[0];
                content.push_str(&format!("{} {} m ", first.x, first.y));
                for point in points.iter().skip(1) {
                    content.push_str(&format!("{} {} l ", point.x, point.y));
                }
                content.push_str("S Q\n");
            }
        }
        AnnotationKind::Signature => {}
    }

    if !ext_gstates.is_empty() {
        resources.set("ExtGState", ext_gstates);
    }
    if !fonts.is_empty() {
        resources.set("Font", fonts);
    }

    let stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "FormType" => 1,
            "BBox" => vec![0.into(), 0.into(), w.into(), h.into()],
            "Resources" => resources,
        },
        content.into_bytes(),
    );
    Ok(doc.add_object(stream))
}

fn text_object(text: &str) -> Object {
    Object::String(utf16_hex(text), StringFormat::Hexadecimal)
}

fn build_annotation_dict(
    doc: &mut Document,
    annotation: &Annotation,
    geom: PageGeom,
    page_id: ObjectId,
    appearance: ObjectId,
) -> Result<Dictionary> {
    let user_rect = geom.rect_to_user(annotation.rect.normalized());
    let rect_values = vec![
        user_rect.x.into(),
        user_rect.y.into(),
        (user_rect.x + user_rect.width).into(),
        (user_rect.y + user_rect.height).into(),
    ];
    let [r, g, b] = parse_hex_color(&annotation.color);
    let color = vec![r.into(), g.into(), b.into()];

    let mut dict = dictionary! {
        "Type" => "Annot",
        "Subtype" => match annotation.kind {
            AnnotationKind::Highlight => "Highlight",
            AnnotationKind::Underline => "Underline",
            AnnotationKind::Strikeout => "StrikeOut",
            AnnotationKind::Square => "Square",
            AnnotationKind::Circle => "Circle",
            AnnotationKind::Line | AnnotationKind::Arrow => "Line",
            AnnotationKind::FreeText => "FreeText",
            AnnotationKind::Note => "Text",
            AnnotationKind::Ink => "Ink",
            AnnotationKind::Signature => "Stamp",
        },
        "Rect" => rect_values,
        "P" => page_id,
        "F" => 4i64,
        "C" => color,
        "CA" => annotation.opacity,
        "NM" => Object::string_literal(annotation.id.as_bytes().to_vec()),
        "AP" => dictionary! { "N" => appearance },
    };

    if let Some(fill) = annotation.fill.as_deref() {
        let [fr, fg, fb] = parse_hex_color(fill);
        dict.set("IC", vec![fr.into(), fg.into(), fb.into()]);
    }

    if annotation.kind != AnnotationKind::FreeText && annotation.kind != AnnotationKind::Highlight {
        dict.set(
            "BS",
            dictionary! { "Type" => "Border", "W" => annotation.width.max(0.1), "S" => "S" },
        );
    }

    if let Some(contents) = &annotation.contents {
        dict.set("Contents", text_object(contents));
    }
    if let Some(image_data) = &annotation.image_data {
        dict.set(
            "PreviewSignature",
            Object::string_literal(image_data.clone()),
        );
    }

    match annotation.kind {
        AnnotationKind::Highlight | AnnotationKind::Underline | AnnotationKind::Strikeout => {
            // QuadPoints corners: upper-left, upper-right, lower-left, lower-right.
            let corners = [
                (annotation.rect.x, annotation.rect.y),
                (annotation.rect.right(), annotation.rect.y),
                (annotation.rect.x, annotation.rect.bottom()),
                (annotation.rect.right(), annotation.rect.bottom()),
            ];
            let mut quads = Vec::with_capacity(8);
            for (x, y) in corners {
                let point = geom.to_user(x, y);
                quads.push(point.x.into());
                quads.push(point.y.into());
            }
            dict.set("QuadPoints", quads);
        }
        AnnotationKind::Line | AnnotationKind::Arrow => {
            let points: Vec<Point> = if annotation.points.len() >= 2 {
                annotation.points.clone()
            } else {
                vec![
                    Point::new(annotation.rect.x, annotation.rect.bottom()),
                    Point::new(annotation.rect.right(), annotation.rect.y),
                ]
            };
            let a = geom.to_user(points[0].x, points[0].y);
            let b = geom.to_user(points[1].x, points[1].y);
            dict.set("L", vec![a.x.into(), a.y.into(), b.x.into(), b.y.into()]);
            if annotation.kind == AnnotationKind::Arrow {
                dict.set(
                    "LE",
                    vec![
                        Object::Name(b"None".to_vec()),
                        Object::Name(b"ClosedArrow".to_vec()),
                    ],
                );
            }
        }
        AnnotationKind::Ink => {
            let points: Vec<Point> = annotation
                .points
                .iter()
                .map(|point| geom.to_user(point.x, point.y))
                .collect();
            let mut stroke = Vec::with_capacity(points.len() * 2);
            for point in points {
                stroke.push(point.x.into());
                stroke.push(point.y.into());
            }
            dict.set("InkList", vec![Object::Array(stroke)]);
        }
        AnnotationKind::FreeText => {
            let (font_key, _) = font_resource(annotation.bold, annotation.italic);
            dict.set(
                "DA",
                Object::string_literal(format!(
                    "{r} {g} {b} rg /{font_key} {} Tf",
                    annotation.font_size.max(6.0)
                )),
            );
            dict.set("Q", 0i64);
        }
        AnnotationKind::Note => {
            dict.set("Name", "Comment");
            dict.set("Open", false);
        }
        AnnotationKind::Signature => {}
        _ => {}
    }

    let _ = doc;
    Ok(dict)
}

/// Writes annotations into the PDF. New annotations become indirect objects,
/// edited existing annotations are rewritten, and annotations removed from the
/// model have their references detached. Everything else is left alone.
pub fn write_annotations(
    doc: &mut Document,
    annotations: &[Annotation],
    existing_objects: &HashMap<String, ObjectId>,
    removed: &[(u32, ObjectId)],
) -> Result<HashMap<String, ObjectId>> {
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let geometries: Vec<PageGeom> = page_ids
        .iter()
        .map(|id| crate::geometry::page_geometry(doc, *id).unwrap_or_default())
        .collect();

    for (page_index, object_id) in removed {
        if let Some(page_id) = page_ids.get(*page_index as usize) {
            remove_annots_reference(doc, *page_id, *object_id)?;
        }
    }

    let mut objects = existing_objects.clone();
    let live_ids: HashSet<&str> = annotations.iter().map(|a| a.id.as_str()).collect();
    for (id, object_id) in existing_objects {
        if !live_ids.contains(id.as_str()) {
            for page_id in &page_ids {
                remove_annots_reference(doc, *page_id, *object_id)?;
            }
        }
    }

    for annotation in annotations {
        let page_id = *page_ids
            .get(annotation.page as usize)
            .ok_or_else(|| AppError::Other("annotation page is out of bounds".into()))?;
        let geom = geometries
            .get(annotation.page as usize)
            .copied()
            .unwrap_or_default();

        if annotation.existing && !annotation.dirty {
            continue;
        }

        let appearance = make_appearance(doc, annotation, geom)?;
        let dict = build_annotation_dict(doc, annotation, geom, page_id, appearance)?;

        if let Some(object_id) = objects.get(&annotation.id).copied() {
            match doc.get_object_mut(object_id) {
                Ok(Object::Dictionary(existing)) => {
                    *existing = dict;
                }
                _ => {
                    doc.objects.insert(object_id, Object::Dictionary(dict));
                }
            }
            add_annots_reference(doc, page_id, object_id)?;
        } else {
            let object_id = doc.add_object(Object::Dictionary(dict));
            objects.insert(annotation.id.clone(), object_id);
            add_annots_reference(doc, page_id, object_id)?;
        }
    }

    Ok(objects)
}

/// Returns true when the PDF contains an AcroForm signature field or a
/// signature dictionary. A full rewrite would invalidate those signatures, so
/// the UI warns before saving.
pub fn has_signatures(doc: &Document) -> bool {
    if let Ok(catalog) = doc.catalog() {
        if let Ok(acroform) = catalog.get(b"AcroForm").and_then(Object::as_reference) {
            if let Ok(form) = doc.get_dictionary(acroform) {
                if let Ok(fields) = form.get(b"Fields").and_then(Object::as_reference) {
                    if let Ok(array) = doc.get_object(fields).and_then(Object::as_array) {
                        if array.iter().any(|field| {
                            field
                                .as_reference()
                                .ok()
                                .and_then(|id| doc.get_dictionary(id).ok())
                                .and_then(|d| d.get(b"FT").ok())
                                .and_then(|o| o.as_name().ok())
                                == Some(b"Sig".as_slice())
                        }) {
                            return true;
                        }
                    }
                }
            }
        }
    }

    doc.objects.values().any(|object| match object {
        Object::Dictionary(dict) => {
            dict.get(b"Type").ok().and_then(name_of).as_deref() == Some("Sig")
                || dict.has(b"ByteRange")
        }
        Object::Stream(stream) => {
            stream.dict.get(b"Type").ok().and_then(name_of).as_deref() == Some("Sig")
                || stream.dict.has(b"ByteRange")
        }
        _ => false,
    })
}
