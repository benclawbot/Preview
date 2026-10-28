use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{Document, EncryptionState, EncryptionVersion, Object, ObjectId, Permissions};
use pdfium_render::prelude::{
    PdfColor, PdfPageContentRegenerationStrategy, PdfPageTextRenderMode, PdfPoints, PdfRect,
};
use pdfium_render::prelude::{PdfDocument, PdfPageObjectCommon, PdfPageObjectsCommon};
use rand::Rng as _;

use crate::annotations::{self, has_signatures};
use crate::error::{AppError, Result};
use crate::geometry::{self, PageGeom};
use crate::model::{
    Annotation, BookmarkInfo, DocumentInfo, DocumentKind, FormFieldInfo, Inverse, MetadataEntry,
    MetadataReport, Operation, PageInfo, SaveOptions, TextMatch,
};
use crate::pdfium_engine::PdfEngine;

pub struct PdfState {
    pub lopdf: Document,
    pub pdfium: PdfDocument<'static>,
    pub geometries: Vec<PageGeom>,
    pub annotations: Vec<Annotation>,
    pub annotation_objects: HashMap<String, ObjectId>,
    pub labels: Vec<Option<String>>,
    pub password: Option<String>,
}

impl PdfState {
    pub fn open(engine: &PdfEngine, bytes: &[u8], password: Option<String>) -> Result<Self> {
        let parsed = Document::load_mem(bytes)
            .map_err(|error| AppError::Message(format!("could not read PDF: {error}")))?;
        let encrypted = parsed.is_encrypted();
        let (lopdf, effective_password) = if encrypted {
            let Some(password) = password.as_deref() else {
                return Err(AppError::Message(
                    "password required: this PDF is encrypted".into(),
                ));
            };
            (
                Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password(password))
                    .map_err(|_| {
                        AppError::Message("the password is incorrect or missing".into())
                    })?,
                Some(password.to_string()),
            )
        } else {
            (parsed, None)
        };

        let pdfium = engine.load_document(bytes.to_vec(), effective_password.as_deref())?;
        if pdfium.pages().len() as usize != lopdf.get_pages().len() {
            // PDFium repairs some malformed page trees; prefer its page count
            // only when lopdf cannot see any pages.
            if lopdf.get_pages().is_empty() {
                return Err(AppError::Message(
                    "the PDF page tree could not be read".into(),
                ));
            }
        }

        let geometries = geometry::all_page_geometries(&lopdf);
        let parsed = annotations::read_annotations(&lopdf, &geometries);
        let labels = page_labels(&pdfium);

        Ok(Self {
            lopdf,
            pdfium,
            geometries,
            annotations: parsed.annotations,
            annotation_objects: parsed.objects,
            labels,
            password: effective_password,
        })
    }

    pub fn page_count(&self) -> u32 {
        self.geometries.len() as u32
    }

    pub fn to_bytes(&mut self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.lopdf.save_to(&mut bytes)?;
        Ok(bytes)
    }

    /// Reopens the pdfium side from the edited lopdf document. Structural
    /// operations (rotate/delete/reorder) need this; annotation edits do not,
    /// because the overlay draws them until save.
    pub fn reload(&mut self, engine: &PdfEngine) -> Result<()> {
        let bytes = self.to_bytes()?;
        let password = self
            .lopdf
            .is_encrypted()
            .then_some(self.password.as_deref())
            .flatten();
        self.pdfium = engine.load_document(bytes, password)?;
        self.geometries = geometry::all_page_geometries(&self.lopdf);
        self.labels = page_labels(&self.pdfium);
        Ok(())
    }

    pub fn render(&self, engine: &PdfEngine, page: u32, dpi: f32) -> Result<(Vec<u8>, u32, u32)> {
        let geom = *self
            .geometries
            .get(page as usize)
            .ok_or_else(|| AppError::Other("page is out of bounds".into()))?;
        let scale = (dpi.max(9.0) / 72.0).clamp(0.02, 12.0);
        let output = engine.render_page(&self.pdfium, page, geom, scale, false)?;
        Ok((output.png, output.width, output.height))
    }

    /// Renders the current PDF state for image export, including annotation
    /// edits that have not yet been saved to the source file.
    pub fn render_for_export(
        &self,
        engine: &PdfEngine,
        page: u32,
        dpi: f32,
    ) -> Result<(Vec<u8>, u32, u32)> {
        let geom = *self
            .geometries
            .get(page as usize)
            .ok_or_else(|| AppError::Other("page is out of bounds".into()))?;
        let scale = (dpi.max(9.0) / 72.0).clamp(0.02, 12.0);

        let mut annotated = self.lopdf.clone();
        annotations::write_annotations(
            &mut annotated,
            &self.annotations,
            &self.annotation_objects,
            &[],
        )?;
        let mut bytes = Vec::new();
        annotated.save_to(&mut bytes)?;
        let pdfium = engine.load_document(bytes, None)?;
        let output = engine.render_page(&pdfium, page, geom, scale, true)?;
        Ok((output.png, output.width, output.height))
    }

    pub fn find_text(&self, engine: &PdfEngine, query: &str) -> Result<Vec<TextMatch>> {
        engine.find_text(&self.pdfium, &self.geometries, query)
    }

    pub fn bookmarks(&self) -> Vec<BookmarkInfo> {
        fn flatten(
            doc: &Document,
            outlines: &[lopdf::Outline],
            pages: &[ObjectId],
            depth: u32,
            result: &mut Vec<BookmarkInfo>,
        ) {
            for outline in outlines {
                match outline {
                    lopdf::Outline::Destination(destination) => {
                        let title = destination
                            .title()
                            .ok()
                            .and_then(|object| lopdf::decode_text_string(object).ok())
                            .unwrap_or_default();
                        let target = destination.page().ok().and_then(|object| match object {
                            Object::Reference(id) => Some(*id),
                            Object::Integer(index) => pages.get((*index).max(0) as usize).copied(),
                            _ => None,
                        });
                        if let Some(target) = target {
                            if let Some(page) = pages.iter().position(|id| *id == target) {
                                result.push(BookmarkInfo {
                                    title,
                                    page: page as u32,
                                    depth,
                                });
                            }
                        }
                    }
                    lopdf::Outline::SubOutlines(children) => {
                        flatten(doc, children, pages, depth + 1, result)
                    }
                }
            }
            let _ = doc;
        }
        let mut names = Default::default();
        let Ok(Some(outlines)) = self.lopdf.get_outlines(None, None, &mut names) else {
            return Vec::new();
        };
        let pages: Vec<ObjectId> = self.lopdf.get_pages().values().copied().collect();
        let mut result = Vec::new();
        flatten(&self.lopdf, &outlines, &pages, 0, &mut result);
        result
    }

    pub fn form_fields(&self) -> Vec<FormFieldInfo> {
        fn resolve<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
            match object {
                Object::Reference(id) => doc.get_object(*id).unwrap_or(object),
                other => other,
            }
        }
        fn text_value(object: &Object) -> String {
            match object {
                Object::Name(value) => String::from_utf8_lossy(value).to_string(),
                _ => lopdf::decode_text_string(object).unwrap_or_default(),
            }
        }
        fn walk(
            doc: &Document,
            fields: &[Object],
            prefix: &str,
            inherited_type: Option<String>,
            inherited_flags: u32,
            output: &mut Vec<FormFieldInfo>,
            depth: usize,
        ) {
            if depth > 64 {
                return;
            }
            for field in fields {
                let dict = match resolve(doc, field) {
                    Object::Dictionary(dict) => dict,
                    _ => continue,
                };
                let partial = dict.get(b"T").ok().map(text_value).unwrap_or_default();
                let name = if prefix.is_empty() {
                    partial.clone()
                } else if partial.is_empty() {
                    prefix.to_string()
                } else {
                    format!("{prefix}.{partial}")
                };
                let field_type = dict
                    .get(b"FT")
                    .ok()
                    .map(|o| {
                        String::from_utf8_lossy(resolve(doc, o).as_name().unwrap_or_default())
                            .to_string()
                    })
                    .or(inherited_type.clone());
                let field_flags = dict
                    .get(b"Ff")
                    .ok()
                    .map(|value| resolve(doc, value))
                    .and_then(|value| value.as_i64().ok())
                    .map(|value| value as u32)
                    .unwrap_or(inherited_flags);
                let kids = dict
                    .get(b"Kids")
                    .ok()
                    .map(|o| resolve(doc, o))
                    .and_then(|o| o.as_array().ok());
                if let Some(kids) = kids {
                    let children: Vec<Object> = kids
                        .iter()
                        .filter(|child| {
                            let dict = match resolve(doc, child) {
                                Object::Dictionary(dict) => dict,
                                _ => return false,
                            };
                            // Widget kids may carry their own appearances but are not independent fields.
                            !dict.has(b"Subtype")
                                || dict.get(b"T").is_ok()
                                || dict.get(b"FT").is_ok()
                        })
                        .cloned()
                        .collect();
                    if !children.is_empty() {
                        walk(
                            doc,
                            &children,
                            &name,
                            field_type.clone(),
                            field_flags,
                            output,
                            depth + 1,
                        );
                    }
                }
                if name.is_empty() {
                    continue;
                }
                let Some(field_type) = field_type else {
                    continue;
                };
                let value = dict
                    .get(b"V")
                    .ok()
                    .map(|o| text_value(resolve(doc, o)))
                    .unwrap_or_default();
                let options = dict
                    .get(b"Opt")
                    .ok()
                    .map(|o| resolve(doc, o))
                    .and_then(|o| o.as_array().ok())
                    .map(|values| {
                        values
                            .iter()
                            .map(|value| match resolve(doc, value) {
                                Object::Array(pair) => {
                                    pair.first().map(text_value).unwrap_or_default()
                                }
                                value => text_value(value),
                            })
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        if field_type != "Btn" {
                            return Vec::new();
                        }
                        let Some(kids) = dict
                            .get(b"Kids")
                            .ok()
                            .map(|value| resolve(doc, value))
                            .and_then(|value| value.as_array().ok())
                        else {
                            return Vec::new();
                        };
                        let mut states = Vec::new();
                        for child in kids {
                            let widget = resolve(doc, child);
                            let Ok(widget) = widget.as_dict() else {
                                continue;
                            };
                            let Some(ap) = widget
                                .get(b"AP")
                                .ok()
                                .map(|value| resolve(doc, value))
                                .and_then(|value| value.as_dict().ok())
                            else {
                                continue;
                            };
                            let Some(normal) = ap
                                .get(b"N")
                                .ok()
                                .map(|value| resolve(doc, value))
                                .and_then(|value| value.as_dict().ok())
                            else {
                                continue;
                            };
                            for (key, _) in normal.iter() {
                                if key.as_slice() != b"Off" {
                                    let value = String::from_utf8_lossy(key).to_string();
                                    if !states.contains(&value) {
                                        states.push(value);
                                    }
                                }
                            }
                        }
                        states
                    });
                let kind = match field_type.as_str() {
                    "Tx" => "text",
                    "Btn" if field_flags & (1 << 16) != 0 => "pushbutton",
                    "Btn" if field_flags & (1 << 15) != 0 => "radio",
                    "Btn" => "button",
                    "Ch" => "choice",
                    "Sig" => "signature",
                    other => other,
                };
                let checked = field_type == "Btn" && !value.is_empty() && value != "Off";
                output.push(FormFieldInfo {
                    name,
                    kind: kind.into(),
                    value,
                    options,
                    checked,
                });
            }
        }

        let Ok(catalog) = self.lopdf.catalog() else {
            return Vec::new();
        };
        let Some(form) = catalog
            .get(b"AcroForm")
            .ok()
            .map(|o| resolve(&self.lopdf, o))
        else {
            return Vec::new();
        };
        let Some(fields) = form
            .as_dict()
            .ok()
            .and_then(|dict| dict.get(b"Fields").ok())
            .map(|o| resolve(&self.lopdf, o))
            .and_then(|o| o.as_array().ok())
        else {
            return Vec::new();
        };
        let mut result = Vec::new();
        walk(&self.lopdf, fields, "", None, 0, &mut result, 0);
        result
    }

    pub fn fill_form_field(&mut self, engine: &PdfEngine, name: &str, value: &str) -> Result<()> {
        fn find(
            doc: &Document,
            fields: &[Object],
            prefix: &str,
            inherited: Option<String>,
            inherited_flags: u32,
            target: &str,
            depth: usize,
        ) -> Result<Option<(ObjectId, String, u32)>> {
            if depth > 64 {
                return Ok(None);
            }
            for field in fields {
                let Ok(id) = field.as_reference() else {
                    continue;
                };
                let dict = doc.get_dictionary(id)?;
                let partial = dict
                    .get(b"T")
                    .ok()
                    .map(lopdf::decode_text_string)
                    .and_then(|value| value.ok())
                    .unwrap_or_default();
                let full = if prefix.is_empty() {
                    partial.clone()
                } else if partial.is_empty() {
                    prefix.to_string()
                } else {
                    format!("{prefix}.{partial}")
                };
                let field_type = dict
                    .get(b"FT")
                    .ok()
                    .and_then(|o| match o {
                        Object::Name(v) => Some(String::from_utf8_lossy(v).to_string()),
                        Object::Reference(r) => doc
                            .get_object(*r)
                            .ok()
                            .and_then(|x| x.as_name().ok())
                            .map(|v| String::from_utf8_lossy(v).to_string()),
                        _ => None,
                    })
                    .or(inherited.clone());
                let field_flags = dict
                    .get(b"Ff")
                    .ok()
                    .and_then(|object| match object {
                        Object::Reference(id) => doc.get_object(*id).ok(),
                        other => Some(other),
                    })
                    .and_then(|object| object.as_i64().ok())
                    .map(|value| value as u32)
                    .unwrap_or(inherited_flags);
                if full == target {
                    return Ok(Some((
                        id,
                        field_type.unwrap_or_else(|| "Tx".into()),
                        field_flags,
                    )));
                }
                if let Some(kids) = dict
                    .get(b"Kids")
                    .ok()
                    .and_then(|o| match o {
                        Object::Reference(r) => doc.get_object(*r).ok(),
                        _ => Some(o),
                    })
                    .and_then(|o| o.as_array().ok())
                {
                    if let Some(found) =
                        find(doc, kids, &full, field_type, field_flags, target, depth + 1)?
                    {
                        return Ok(Some(found));
                    }
                }
            }
            Ok(None)
        }
        let catalog = self.lopdf.catalog()?.clone();
        let acroform_ref = catalog
            .get(b"AcroForm")
            .and_then(Object::as_reference)
            .map_err(|_| AppError::Message("PDF contains no AcroForm".into()))?;
        let fields_ref = self
            .lopdf
            .get_dictionary(acroform_ref)?
            .get(b"Fields")?
            .as_reference()?;
        let fields = self.lopdf.get_object(fields_ref)?.as_array()?.clone();
        let Some((field_id, field_type, field_flags)) =
            find(&self.lopdf, &fields, "", None, 0, name, 0)?
        else {
            return Err(AppError::Message(format!("form field not found: {name}")));
        };
        if field_type == "Sig" {
            return Err(AppError::Unsupported(
                "digital signature fields require a certificate-backed signing workflow".into(),
            ));
        }
        if field_type == "Btn" && field_flags & (1 << 16) != 0 {
            return Err(AppError::Unsupported(
                "push-button form widgets cannot be filled as data fields".into(),
            ));
        }
        let is_radio = field_type == "Btn" && field_flags & (1 << 15) != 0;
        let field_dict = self.lopdf.get_dictionary(field_id)?;
        let mut widget_ids: Vec<ObjectId> = field_dict
            .get(b"Kids")
            .ok()
            .and_then(|object| match object {
                Object::Reference(id) => self.lopdf.get_object(*id).ok(),
                other => Some(other),
            })
            .and_then(|object| object.as_array().ok())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_reference().ok())
                    .filter(|id| {
                        self.lopdf.get_dictionary(*id).is_ok_and(|dict| {
                            dict.get(b"Subtype")
                                .and_then(Object::as_name)
                                .is_ok_and(|name| name == b"Widget")
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if field_dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|name| name == b"Widget")
            && !widget_ids.contains(&field_id)
        {
            widget_ids.push(field_id);
        }
        let mut selected_button = value.to_string();
        if field_type == "Btn"
            && !is_radio
            && matches!(
                value.to_ascii_lowercase().as_str(),
                "true" | "yes" | "on" | "1"
            )
            && !widget_ids.is_empty()
        {
            let widget = self.lopdf.get_dictionary(widget_ids[0])?;
            let on_value = widget
                .get(b"AP")
                .ok()
                .and_then(|object| match object {
                    Object::Reference(id) => self.lopdf.get_object(*id).ok(),
                    other => Some(other),
                })
                .and_then(|object| object.as_dict().ok())
                .and_then(|ap| ap.get(b"N").ok())
                .and_then(|object| match object {
                    Object::Reference(id) => self.lopdf.get_object(*id).ok(),
                    other => Some(other),
                })
                .and_then(|object| object.as_dict().ok())
                .and_then(|normal| {
                    normal
                        .iter()
                        .find(|(key, _)| key.as_slice() != b"Off")
                        .map(|(key, _)| String::from_utf8_lossy(key).to_string())
                })
                .unwrap_or_else(|| "Yes".into());
            selected_button = on_value;
        }
        if is_radio
            && !matches!(value.to_ascii_lowercase().as_str(), "off" | "false" | "0")
            && !widget_ids.iter().any(|widget_id| {
                self.lopdf
                    .get_dictionary(*widget_id)
                    .ok()
                    .and_then(|widget| widget.get(b"AP").ok())
                    .and_then(|object| match object {
                        Object::Reference(id) => self.lopdf.get_object(*id).ok(),
                        other => Some(other),
                    })
                    .and_then(|object| object.as_dict().ok())
                    .and_then(|ap| ap.get(b"N").ok())
                    .and_then(|object| match object {
                        Object::Reference(id) => self.lopdf.get_object(*id).ok(),
                        other => Some(other),
                    })
                    .and_then(|object| object.as_dict().ok())
                    .is_some_and(|normal| normal.has(value.as_bytes()))
            })
        {
            return Err(AppError::Message(format!(
                "radio value is not one of the field's choices: {value}"
            )));
        }
        let field_value = if field_type == "Btn" {
            if value.eq_ignore_ascii_case("off")
                || value.eq_ignore_ascii_case("false")
                || value == "0"
            {
                Object::Name(b"Off".to_vec())
            } else {
                Object::Name(selected_button.as_bytes().to_vec())
            }
        } else if field_type == "Ch" {
            Object::string_literal(value)
        } else {
            lopdf::text_string(value)
        };
        self.lopdf
            .get_dictionary_mut(field_id)?
            .set("V", field_value.clone());
        if field_type == "Btn" {
            for widget_id in widget_ids {
                let state = if let Ok(widget) = self.lopdf.get_dictionary(widget_id) {
                    let states = widget
                        .get(b"AP")
                        .ok()
                        .and_then(|object| match object {
                            Object::Reference(id) => self.lopdf.get_object(*id).ok(),
                            other => Some(other),
                        })
                        .and_then(|object| object.as_dict().ok())
                        .and_then(|ap| ap.get(b"N").ok())
                        .and_then(|object| match object {
                            Object::Reference(id) => self.lopdf.get_object(*id).ok(),
                            other => Some(other),
                        })
                        .and_then(|object| object.as_dict().ok());
                    if states.is_some_and(|states| states.has(selected_button.as_bytes()))
                        && field_value.as_name().ok() == Some(selected_button.as_bytes())
                    {
                        selected_button.as_bytes().to_vec()
                    } else {
                        b"Off".to_vec()
                    }
                } else {
                    b"Off".to_vec()
                };
                self.lopdf
                    .get_dictionary_mut(widget_id)?
                    .set("AS", Object::Name(state));
            }
        }
        self.lopdf
            .get_dictionary_mut(acroform_ref)?
            .set("NeedAppearances", true);
        self.reload(engine)?;
        Ok(())
    }

    pub fn page_info(&self) -> Vec<PageInfo> {
        self.geometries
            .iter()
            .enumerate()
            .map(|(index, geom)| geom.info(index as u32))
            .collect()
    }

    /// Permanently removes page objects intersecting `rect` and paints an
    /// opaque black replacement over the requested region. The operation is
    /// fail-closed: interactive pages, encrypted files, signatures, and
    /// objects whose bounds cannot be inspected are rejected.
    fn redact_page(
        &mut self,
        engine: &PdfEngine,
        page_index: u32,
        requested: crate::model::Rect,
    ) -> Result<(
        Vec<u32>,
        Vec<u8>,
        Vec<Annotation>,
        HashMap<String, ObjectId>,
    )> {
        if self.lopdf.is_encrypted() {
            return Err(AppError::Unsupported(
                "redaction is disabled on encrypted PDFs; export an unlocked copy, reopen it, then redact so the rewritten bytes can be verified".into(),
            ));
        }
        if annotations::has_signatures(&self.lopdf) {
            return Err(AppError::Unsupported(
                "redaction is disabled on digitally signed PDFs because rewriting invalidates their signatures".into(),
            ));
        }
        let geom = *self
            .geometries
            .get(page_index as usize)
            .ok_or_else(|| AppError::Other("redaction page is out of bounds".into()))?;
        let rect = requested.normalized();
        if ![rect.x, rect.y, rect.width, rect.height]
            .iter()
            .all(|value| value.is_finite())
            || rect.width < 1.0
            || rect.height < 1.0
        {
            return Err(AppError::Message(
                "redaction area must be a non-empty rectangle".into(),
            ));
        }
        let right = rect.right().min(geom.width());
        let bottom = rect.bottom().min(geom.height());
        let left = rect.x.max(0.0);
        let top = rect.y.max(0.0);
        if right - left < 1.0 || bottom - top < 1.0 {
            return Err(AppError::Message(
                "redaction area is outside the page".into(),
            ));
        }
        let rect = crate::model::Rect::new(left, top, right - left, bottom - top);
        let page_id = page_id_at(&self.lopdf, page_index)?;
        if !self
            .lopdf
            .get_page_annotations(page_id)
            .map_err(|error| {
                AppError::Other(format!("could not inspect page annotations: {error}"))
            })?
            .is_empty()
        {
            return Err(AppError::Unsupported(
                "redaction is disabled on pages with annotations or form widgets; remove the interactive data first".into(),
            ));
        }
        if !self.form_fields().is_empty() {
            return Err(AppError::Unsupported(
                "redaction is disabled in PDFs with form fields to avoid changing form data unexpectedly".into(),
            ));
        }

        let pdf_rect = geom.rect_to_user(rect);
        let pdf_rect =
            PdfRect::new_from_values(pdf_rect.y, pdf_rect.x, pdf_rect.bottom(), pdf_rect.right());

        let mut snapshot = self.lopdf.clone();
        let snapshot_objects = annotations::write_annotations(
            &mut snapshot,
            &self.annotations,
            &self.annotation_objects,
            &[],
        )?;
        let mut snapshot_bytes = Vec::new();
        snapshot.save_to(&mut snapshot_bytes)?;
        let original_annotations = self.annotations.clone();

        let mut pdfium = engine.load_document(snapshot_bytes.clone(), None)?;
        {
            let mut page = pdfium.pages_mut().get(page_index as i32).map_err(|error| {
                AppError::Other(format!("could not open redaction page: {error}"))
            })?;
            let object_count = page.objects().len();
            let mut remove = Vec::new();
            for index in 0..object_count {
                let object = page.objects().get(index).map_err(|error| {
                    AppError::Unsupported(format!(
                        "redaction cannot safely inspect page object {index}: {error}"
                    ))
                })?;
                let bounds = object.bounds().map_err(|error| {
                    AppError::Unsupported(format!(
                        "redaction cannot safely determine page object {index} bounds: {error}"
                    ))
                })?;
                let intersects = bounds.right().value > pdf_rect.left().value
                    && bounds.left().value < pdf_rect.right().value
                    && bounds.top().value > pdf_rect.bottom().value
                    && bounds.bottom().value < pdf_rect.top().value;
                if intersects {
                    remove.push(index);
                }
            }
            let text_before = page
                .text()
                .map_err(|error| AppError::Other(format!("could not inspect page text: {error}")))?
                .inside_rect(pdf_rect);
            for index in remove.into_iter().rev() {
                page.objects_mut()
                    .remove_object_at_index(index)
                    .map_err(|error| {
                        AppError::Other(format!("could not remove redacted page object: {error}"))
                    })?;
            }
            page.objects_mut()
                .create_path_object_rect(pdf_rect, None, None, Some(PdfColor::BLACK))
                .map_err(|error| AppError::Other(format!("could not paint redaction: {error}")))?;
            page.regenerate_content().map_err(|error| {
                AppError::Other(format!(
                    "could not commit redaction to page content: {error}"
                ))
            })?;
            if !text_before.is_empty()
                && !page
                    .text()
                    .map_err(|error| {
                        AppError::Other(format!(
                            "could not verify page text after redaction: {error}"
                        ))
                    })?
                    .inside_rect(pdf_rect)
                    .is_empty()
            {
                return Err(AppError::Unsupported(
                    "redaction verification found text remaining in the selected area; no changes were accepted".into(),
                ));
            }
        }

        let pdfium_bytes = pdfium
            .save_to_bytes()
            .map_err(|error| AppError::Other(format!("could not write redacted PDF: {error}")))?;
        let mut cleaned = Document::load_mem(&pdfium_bytes).map_err(|error| {
            AppError::Message(format!("could not verify redacted PDF: {error}"))
        })?;
        cleaned.prune_objects();
        let mut clean_bytes = Vec::new();
        cleaned.save_to(&mut clean_bytes)?;

        // Re-open the exact bytes that will be saved and verify that PDFium no
        // longer extracts text from the redaction rectangle.
        let verified = engine.load_document(clean_bytes.clone(), None)?;
        let page = verified.pages().get(page_index as i32).map_err(|error| {
            AppError::Other(format!(
                "could not reopen redacted page for verification: {error}"
            ))
        })?;
        if !page
            .text()
            .map_err(|error| AppError::Other(format!("could not verify redacted text: {error}")))?
            .inside_rect(pdf_rect)
            .is_empty()
        {
            return Err(AppError::Unsupported(
                "redaction verification found searchable text remaining in the selected area; no changes were accepted".into(),
            ));
        }

        self.lopdf = cleaned;
        self.pdfium = verified;
        self.geometries = geometry::all_page_geometries(&self.lopdf);
        self.labels = page_labels(&self.pdfium);
        let parsed = annotations::read_annotations(&self.lopdf, &self.geometries);
        self.annotations = parsed.annotations;
        self.annotation_objects = parsed.objects;

        Ok((
            vec![page_index],
            snapshot_bytes,
            original_annotations,
            snapshot_objects,
        ))
    }

    fn ocr_document(
        &mut self,
        engine: &PdfEngine,
    ) -> Result<(
        Vec<u32>,
        Vec<u8>,
        Vec<Annotation>,
        HashMap<String, ObjectId>,
    )> {
        if self.lopdf.is_encrypted() {
            return Err(AppError::Unsupported(
                "OCR is disabled on encrypted PDFs; export an unlocked copy first".into(),
            ));
        }
        if has_signatures(&self.lopdf) {
            return Err(AppError::Unsupported(
                "OCR is disabled on digitally signed PDFs because rewriting invalidates signatures"
                    .into(),
            ));
        }
        if !self.annotations.is_empty() || !self.form_fields().is_empty() {
            return Err(AppError::Unsupported(
                "OCR is disabled on PDFs with markup or form fields to avoid changing interactive content".into(),
            ));
        }
        for page_id in self.lopdf.get_pages().values().copied() {
            if !self
                .lopdf
                .get_page_annotations(page_id)
                .map_err(|error| {
                    AppError::Other(format!(
                        "could not inspect page annotations before OCR: {error}"
                    ))
                })?
                .is_empty()
            {
                return Err(AppError::Unsupported(
                    "OCR is disabled on PDFs with annotations or widgets to avoid changing interactive content".into(),
                ));
            }
        }

        let mut results = Vec::with_capacity(self.page_count() as usize);
        let mut changed = Vec::new();
        let mut total_words = 0usize;
        for page_index in 0..self.page_count() {
            let existing_text = {
                let page = self
                    .pdfium
                    .pages()
                    .get(page_index as i32)
                    .map_err(|error| {
                        AppError::Other(format!("could not inspect page text: {error}"))
                    })?;
                page.text()
                    .map_err(|error| {
                        AppError::Other(format!("could not inspect page text: {error}"))
                    })?
                    .all()
            };
            if !existing_text.trim().is_empty() {
                results.push(Vec::new());
                continue;
            }
            let geom = self.geometries[page_index as usize];
            if geom.rotation != 0 {
                return Err(AppError::Unsupported(
                    "OCR currently requires unrotated scanned pages; rotate and save the page before OCR".into(),
                ));
            }
            let (png, width, height) = self.render(engine, page_index, 300.0)?;
            let words = run_tesseract_tsv(&png)?;
            total_words = total_words.saturating_add(words.len());
            if total_words > 100_000 {
                return Err(AppError::Unsupported(
                    "OCR output exceeds the 100,000-word safety limit".into(),
                ));
            }
            if !words.is_empty() {
                changed.push(page_index);
            }
            results.push(
                words
                    .into_iter()
                    .map(|word| (word, width, height))
                    .collect(),
            );
        }
        if changed.is_empty() {
            return Err(AppError::Message(
                "no scanned-page text was recognized; pages with existing text were left unchanged"
                    .into(),
            ));
        }

        let mut snapshot = self.lopdf.clone();
        let snapshot_objects = annotations::write_annotations(
            &mut snapshot,
            &self.annotations,
            &self.annotation_objects,
            &[],
        )?;
        let mut snapshot_bytes = Vec::new();
        snapshot.save_to(&mut snapshot_bytes)?;
        let original_annotations = self.annotations.clone();
        let mut pdfium = engine.load_document(snapshot_bytes.clone(), None)?;
        let font = pdfium.fonts_mut().helvetica();
        let expected_words: Vec<Vec<String>> = results
            .iter()
            .map(|words| {
                words
                    .iter()
                    .map(|(word, _, _)| word.text.to_lowercase())
                    .collect()
            })
            .collect();
        for (page_index, words) in results.into_iter().enumerate() {
            if words.is_empty() {
                continue;
            }
            let geom = self.geometries[page_index];
            let mut page = pdfium
                .pages_mut()
                .get(page_index as i32)
                .map_err(|error| AppError::Other(format!("could not open OCR page: {error}")))?;
            page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
            for (word, width, height) in words {
                let display_x = word.left as f32 * geom.width() / width.max(1) as f32;
                let display_baseline = (word.top as f32 + word.height as f32 * 0.82)
                    * geom.height()
                    / height.max(1) as f32;
                let user = geom.to_user(display_x, display_baseline);
                let font_size = (word.height as f32 * geom.height() / height.max(1) as f32 * 0.78)
                    .clamp(4.0, 72.0);
                let mut object = page
                    .objects_mut()
                    .create_text_object(
                        PdfPoints::new(user.x),
                        PdfPoints::new(user.y),
                        format!("{} ", word.text),
                        font,
                        PdfPoints::new(font_size),
                    )
                    .map_err(|error| AppError::Other(format!("could not add OCR text: {error}")))?;
                object
                    .as_text_object_mut()
                    .ok_or_else(|| {
                        AppError::Other("OCR text object has an unexpected type".into())
                    })?
                    .set_render_mode(PdfPageTextRenderMode::Invisible)
                    .map_err(|error| {
                        AppError::Other(format!("could not hide OCR text: {error}"))
                    })?;
            }
            page.regenerate_content()
                .map_err(|error| AppError::Other(format!("could not commit OCR text: {error}")))?;
        }
        let pdfium_bytes = pdfium
            .save_to_bytes()
            .map_err(|error| AppError::Other(format!("could not write OCR PDF: {error}")))?;
        let mut cleaned = Document::load_mem(&pdfium_bytes)
            .map_err(|error| AppError::Message(format!("could not verify OCR PDF: {error}")))?;
        cleaned.prune_objects();
        let mut clean_bytes = Vec::new();
        cleaned.save_to(&mut clean_bytes)?;
        let verified = engine.load_document(clean_bytes.clone(), None)?;
        for page_index in &changed {
            let page = verified.pages().get(*page_index as i32).map_err(|error| {
                AppError::Other(format!(
                    "could not reopen OCR page for verification: {error}"
                ))
            })?;
            let extracted = page
                .text()
                .map_err(|error| AppError::Other(format!("could not verify OCR text: {error}")))?
                .all()
                .to_lowercase();
            // At least one recognized token from each changed page must be
            // searchable in the exact bytes that will be committed.
            let expected = expected_words
                .get(*page_index as usize)
                .cloned()
                .unwrap_or_default();
            if expected.is_empty() || !expected.iter().any(|word| extracted.contains(word)) {
                return Err(AppError::Unsupported(
                    "OCR verification could not find the recognized text in the saved PDF; no changes were accepted".into(),
                ));
            }
        }

        self.lopdf = cleaned;
        self.pdfium = verified;
        self.geometries = geometry::all_page_geometries(&self.lopdf);
        self.labels = page_labels(&self.pdfium);
        let parsed = annotations::read_annotations(&self.lopdf, &self.geometries);
        self.annotations = parsed.annotations;
        self.annotation_objects = parsed.objects;
        Ok((
            changed,
            snapshot_bytes,
            original_annotations,
            snapshot_objects,
        ))
    }

    pub fn apply_structural(
        &mut self,
        engine: &PdfEngine,
        operation: &Operation,
    ) -> Result<(Vec<u32>, Inverse)> {
        match operation {
            Operation::OcrDocument => {
                let (changed, bytes, annotations, objects) = self.ocr_document(engine)?;
                Ok((
                    changed,
                    Inverse::RestorePdf {
                        bytes,
                        annotations,
                        annotation_objects: objects.into_iter().collect(),
                    },
                ))
            }
            Operation::FillFormFields { fields } => {
                if self.lopdf.is_encrypted() {
                    return Err(AppError::Unsupported(
                        "undoable form editing is unavailable on encrypted PDFs; export an unlocked copy first".into(),
                    ));
                }
                if fields.is_empty() || fields.len() > 256 {
                    return Err(AppError::Message(
                        "fill one to 256 form fields at a time".into(),
                    ));
                }
                if fields.iter().any(|update| {
                    update.field.trim().is_empty()
                        || update.field.len() > 1024
                        || update.value.len() > 10_000
                }) {
                    return Err(AppError::Message(
                        "form field names and values exceed supported limits".into(),
                    ));
                }
                let original_annotations = self.annotations.clone();
                let mut snapshot = self.lopdf.clone();
                let snapshot_objects = annotations::write_annotations(
                    &mut snapshot,
                    &self.annotations,
                    &self.annotation_objects,
                    &[],
                )?;
                let mut bytes = Vec::new();
                snapshot.save_to(&mut bytes)?;
                for update in fields {
                    if let Err(error) = self.fill_form_field(engine, &update.field, &update.value) {
                        self.lopdf = Document::load_mem(&bytes).map_err(|restore_error| {
                            AppError::Other(format!(
                                "form update failed ({error}) and could not restore the original PDF ({restore_error})"
                            ))
                        })?;
                        self.annotations = original_annotations;
                        self.annotation_objects = snapshot_objects;
                        self.reload(engine)?;
                        return Err(error);
                    }
                }
                self.annotations = original_annotations.clone();
                self.annotation_objects = snapshot_objects.clone();
                Ok((
                    (0..self.page_count()).collect(),
                    Inverse::RestorePdf {
                        bytes,
                        annotations: original_annotations,
                        annotation_objects: snapshot_objects.into_iter().collect(),
                    },
                ))
            }
            Operation::Redact { page, rect } => {
                let (changed, bytes, annotations, objects) =
                    self.redact_page(engine, *page, *rect)?;
                Ok((
                    changed,
                    Inverse::RestorePdf {
                        bytes,
                        annotations,
                        annotation_objects: objects.into_iter().collect(),
                    },
                ))
            }
            Operation::InsertPages { source_path, at } => {
                let (changed, bytes, annotations, objects) =
                    self.insert_pdf_pages(engine, Path::new(source_path), *at)?;
                Ok((
                    changed,
                    Inverse::RestorePdf {
                        bytes,
                        annotations,
                        annotation_objects: objects.into_iter().collect(),
                    },
                ))
            }
            Operation::TransferPage {
                source_page,
                at,
                source_pdf_bytes,
                ..
            } => {
                let source_bytes = source_pdf_bytes.as_deref().ok_or_else(|| {
                    AppError::Message("page transfer has no retained source snapshot".into())
                })?;
                let (changed, bytes, annotations, objects) =
                    self.insert_pdf_page_bytes(engine, source_bytes, *source_page, *at)?;
                Ok((
                    changed,
                    Inverse::RestorePdf {
                        bytes,
                        annotations,
                        annotation_objects: objects.into_iter().collect(),
                    },
                ))
            }
            Operation::RotatePage { page, degrees } => {
                let page_count = self.page_count();
                if *page >= page_count {
                    return Err(AppError::Other("page is out of bounds".into()));
                }
                if degrees % 90 != 0 {
                    return Err(AppError::Message(
                        "page rotation must be a multiple of 90°".into(),
                    ));
                }
                let page_id = page_id_at(&self.lopdf, *page)?;
                let geom = geometry::page_geometry(&self.lopdf, page_id).unwrap_or_default();
                let rotation = (geom.rotation + degrees).rem_euclid(360);
                self.lopdf
                    .get_dictionary_mut(page_id)?
                    .set("Rotate", rotation as i64);
                self.reload(engine)?;
                Ok((
                    vec![*page],
                    Inverse::RotatePage {
                        page: *page,
                        degrees: -degrees,
                    },
                ))
            }
            Operation::DeletePage { page } => {
                let page_count = self.page_count();
                if *page >= page_count {
                    return Err(AppError::Other("page is out of bounds".into()));
                }
                if page_count <= 1 {
                    return Err(AppError::Message(
                        "a PDF must keep at least one page".into(),
                    ));
                }
                let refs = geometry::page_refs(&self.lopdf);
                let page_ref = refs
                    .get(*page as usize)
                    .ok_or_else(|| AppError::Other("page is out of bounds".into()))?
                    .clone();
                let object = self
                    .lopdf
                    .get_object(page_ref.id)
                    .cloned()
                    .map_err(|error| AppError::Other(error.to_string()))?;

                let removed_annotations: Vec<Annotation> = self
                    .annotations
                    .iter()
                    .filter(|annotation| annotation.page == *page)
                    .cloned()
                    .collect();
                let removed_ids: Vec<String> = removed_annotations
                    .iter()
                    .map(|annotation| annotation.id.clone())
                    .collect();
                self.annotations
                    .retain(|annotation| annotation.page != *page);
                for annotation in &mut self.annotations {
                    if annotation.page > *page {
                        annotation.page -= 1;
                    }
                }
                let removed_objects: Vec<(String, ObjectId)> = removed_ids
                    .iter()
                    .filter_map(|id| {
                        self.annotation_objects
                            .get(id)
                            .copied()
                            .map(|object| (id.clone(), object))
                    })
                    .collect();

                geometry::detach_page(&mut self.lopdf, &page_ref)?;
                self.lopdf.objects.remove(&page_ref.id);
                geometry::recompute_page_counts(&mut self.lopdf)?;
                self.reload(engine)?;

                Ok((
                    (0..self.page_count()).collect(),
                    Inverse::InsertPage {
                        page: *page,
                        id: page_ref.id,
                        object,
                        parent: page_ref.parent,
                        index: page_ref.index_in_parent,
                        annotations: removed_annotations,
                        annotation_objects: removed_objects,
                    },
                ))
            }
            Operation::ReorderPages { from, to } => {
                let page_count = self.page_count();
                if *from >= page_count || *to >= page_count {
                    return Err(AppError::Other("page is out of bounds".into()));
                }
                if from == to {
                    return Ok((
                        vec![*from],
                        Inverse::ReorderPages {
                            from: *from,
                            to: *to,
                        },
                    ));
                }
                geometry::reorder_page(&mut self.lopdf, *from, *to)?;
                remap_annotation_pages(&mut self.annotations, *from, *to);
                self.reload(engine)?;
                Ok((
                    (0..self.page_count()).collect(),
                    Inverse::ReorderPages {
                        from: *to,
                        to: *from,
                    },
                ))
            }
            _ => Err(AppError::Message("not a structural page operation".into())),
        }
    }

    pub fn apply_annotation(&mut self, operation: &Operation) -> Result<(Vec<u32>, Inverse)> {
        match operation {
            Operation::AddAnnotation { page, annotation } => {
                if *page >= self.page_count() {
                    return Err(AppError::Other("annotation page is out of bounds".into()));
                }
                let mut annotation = annotation.clone();
                if annotation.id.trim().is_empty() {
                    annotation.id = uuid::Uuid::new_v4().to_string();
                }
                while self
                    .annotations
                    .iter()
                    .any(|existing| existing.id == annotation.id)
                {
                    annotation.id = uuid::Uuid::new_v4().to_string();
                }
                annotation.page = *page;
                annotation.existing = false;
                annotation.dirty = false;
                let id = annotation.id.clone();
                self.annotations.push(annotation);
                Ok((vec![*page], Inverse::RemoveAnnotation(id)))
            }
            Operation::EditAnnotation { id, after, .. } => {
                let position = self
                    .annotations
                    .iter()
                    .position(|annotation| annotation.id == *id)
                    .ok_or_else(|| AppError::Other("annotation not found".into()))?;
                let before = self.annotations[position].clone();
                let mut updated = after.clone();
                updated.id = before.id.clone();
                updated.page = before.page;
                updated.existing = before.existing;
                updated.dirty = true;
                self.annotations[position] = updated;
                Ok((
                    vec![before.page],
                    Inverse::EditAnnotation {
                        id: id.clone(),
                        before,
                    },
                ))
            }
            Operation::RemoveAnnotation { id } => {
                let position = self
                    .annotations
                    .iter()
                    .position(|annotation| annotation.id == *id)
                    .ok_or_else(|| AppError::Other("annotation not found".into()))?;
                let removed = self.annotations.remove(position);
                let page = removed.page;
                Ok((vec![page], Inverse::AddAnnotation(removed)))
            }
            _ => Err(AppError::Message("not an annotation operation".into())),
        }
    }

    pub fn apply_inverse(&mut self, engine: &PdfEngine, inverse: &Inverse) -> Result<Vec<u32>> {
        match inverse {
            Inverse::RotatePage { page, degrees } => {
                let page_id = page_id_at(&self.lopdf, *page)?;
                let geom = geometry::page_geometry(&self.lopdf, page_id).unwrap_or_default();
                let rotation = (geom.rotation + degrees).rem_euclid(360);
                self.lopdf
                    .get_dictionary_mut(page_id)?
                    .set("Rotate", rotation as i64);
                self.reload(engine)?;
                Ok(vec![*page])
            }
            Inverse::InsertPage {
                page,
                id,
                object,
                parent,
                index,
                annotations: restored_annotations,
                annotation_objects,
            } => {
                geometry::attach_page(&mut self.lopdf, object.clone(), *id, *parent, *index)?;
                for annotation in &mut self.annotations {
                    if annotation.page >= *page {
                        annotation.page += 1;
                    }
                }
                for annotation in restored_annotations {
                    self.annotations.push(annotation.clone());
                }
                for (annotation_id, object_id) in annotation_objects {
                    self.annotation_objects
                        .insert(annotation_id.clone(), *object_id);
                }
                geometry::recompute_page_counts(&mut self.lopdf)?;
                self.reload(engine)?;
                Ok((0..self.page_count()).collect())
            }
            Inverse::ReorderPages { from, to } => {
                geometry::reorder_page(&mut self.lopdf, *from, *to)?;
                remap_annotation_pages(&mut self.annotations, *from, *to);
                self.reload(engine)?;
                Ok((0..self.page_count()).collect())
            }
            Inverse::RestorePdf {
                bytes,
                annotations,
                annotation_objects,
            } => {
                self.lopdf = Document::load_mem(bytes).map_err(|error| {
                    AppError::Message(format!("could not restore PDF history: {error}"))
                })?;
                self.annotations = annotations.clone();
                self.annotation_objects = annotation_objects.iter().cloned().collect();
                self.reload(engine)?;
                Ok((0..self.page_count()).collect())
            }
            Inverse::AddAnnotation(annotation) => {
                if !self
                    .annotations
                    .iter()
                    .any(|existing| existing.id == annotation.id)
                {
                    self.annotations.push(annotation.clone());
                }
                Ok(vec![annotation.page])
            }
            Inverse::RemoveAnnotation(id) => {
                let page = self
                    .annotations
                    .iter()
                    .find(|annotation| annotation.id == *id)
                    .map(|annotation| annotation.page)
                    .unwrap_or(0);
                self.annotations.retain(|annotation| annotation.id != *id);
                Ok(vec![page])
            }
            Inverse::EditAnnotation { id, before } => {
                if let Some(annotation) = self
                    .annotations
                    .iter_mut()
                    .find(|annotation| annotation.id == *id)
                {
                    *annotation = before.clone();
                }
                Ok(vec![before.page])
            }
        }
    }

    fn insert_pdf_pages(
        &mut self,
        engine: &PdfEngine,
        source_path: &Path,
        at: u32,
    ) -> Result<(
        Vec<u32>,
        Vec<u8>,
        Vec<Annotation>,
        HashMap<String, ObjectId>,
    )> {
        if self.password.is_some() {
            return Err(AppError::Unsupported(
                "page insertion into encrypted PDFs is unavailable; export an unlocked copy first"
                    .into(),
            ));
        }
        if !source_path.is_file() {
            return Err(AppError::Message(format!(
                "source PDF not found: {}",
                source_path.display()
            )));
        }
        if !self.annotations.is_empty() || has_acroform_or_page_annotations(&self.lopdf) {
            return Err(AppError::Unsupported(
                "page insertion is disabled for PDFs with annotations or interactive forms; save those documents separately first".into(),
            ));
        }
        let source_bytes = std::fs::read(source_path)?;
        let source_lopdf = Document::load_mem(&source_bytes)
            .map_err(|error| AppError::Message(format!("could not read source PDF: {error}")))?;
        if source_lopdf.is_encrypted() {
            return Err(AppError::Message(
                "encrypted source PDFs are not supported for page insertion".into(),
            ));
        }
        if has_signatures(&source_lopdf) || has_acroform_or_page_annotations(&source_lopdf) {
            return Err(AppError::Unsupported(
                "page insertion is disabled for source PDFs with annotations, forms, or signatures to avoid dropping interactive data".into(),
            ));
        }
        let source_page_count = source_lopdf.get_pages().len() as u32;
        if source_page_count == 0 {
            return Err(AppError::Message("source PDF contains no pages".into()));
        }
        if at > self.page_count() {
            return Err(AppError::Message(
                "page insertion position is out of bounds".into(),
            ));
        }

        let before = self.pdfium.save_to_bytes()?;
        let source_pdfium = engine.load_document(source_bytes, None)?;
        let mut destination = engine.load_document(before.clone(), None)?;
        destination
            .pages_mut()
            .copy_page_range_from_document(
                &source_pdfium,
                0..=(source_page_count - 1) as i32,
                at as i32,
            )
            .map_err(|error| {
                AppError::Message(format!("could not import source pages: {error}"))
            })?;
        let merged = destination.save_to_bytes()?;
        let merged_lopdf = Document::load_mem(&merged).map_err(|error| {
            AppError::Message(format!("PDFium produced an invalid merged PDF: {error}"))
        })?;
        let expected = self.page_count() + source_page_count;
        if merged_lopdf.get_pages().len() as u32 != expected {
            return Err(AppError::Message(
                "page import did not produce the expected page count".into(),
            ));
        }
        self.lopdf = merged_lopdf;
        self.pdfium = destination;
        self.annotations.clear();
        self.annotation_objects.clear();
        self.geometries = geometry::all_page_geometries(&self.lopdf);
        self.labels = page_labels(&self.pdfium);
        Ok((
            (at..at + source_page_count).collect(),
            before,
            Vec::new(),
            HashMap::new(),
        ))
    }

    pub(crate) fn transfer_snapshot(&self, page: u32) -> Result<Vec<u8>> {
        if self.password.is_some() || self.lopdf.is_encrypted() {
            return Err(AppError::Unsupported(
                "dragging pages from encrypted PDFs is disabled; export an unlocked copy first"
                    .into(),
            ));
        }
        if has_signatures(&self.lopdf) {
            return Err(AppError::Unsupported(
                "dragging pages from digitally signed PDFs is disabled because copying rewrites the document".into(),
            ));
        }
        if !self.annotations.is_empty() || has_acroform_or_page_annotations(&self.lopdf) {
            return Err(AppError::Unsupported(
                "dragging pages from PDFs with annotations or form fields is disabled to avoid losing interactive data".into(),
            ));
        }
        if page >= self.page_count() {
            return Err(AppError::Message("source page is out of bounds".into()));
        }
        let mut snapshot = self.lopdf.clone();
        let mut bytes = Vec::new();
        snapshot.save_to(&mut bytes)?;
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(AppError::Unsupported(
                "page transfer is limited to source PDFs smaller than 128 MiB".into(),
            ));
        }
        Ok(bytes)
    }

    fn insert_pdf_page_bytes(
        &mut self,
        engine: &PdfEngine,
        source_bytes: &[u8],
        source_page: u32,
        at: u32,
    ) -> Result<(
        Vec<u32>,
        Vec<u8>,
        Vec<Annotation>,
        HashMap<String, ObjectId>,
    )> {
        if self.password.is_some() || self.lopdf.is_encrypted() {
            return Err(AppError::Unsupported(
                "page transfer into encrypted PDFs is disabled; export an unlocked copy first"
                    .into(),
            ));
        }
        if has_signatures(&self.lopdf) {
            return Err(AppError::Unsupported(
                "page transfer into digitally signed PDFs is disabled because copying rewrites the document".into(),
            ));
        }
        if !self.annotations.is_empty() || has_acroform_or_page_annotations(&self.lopdf) {
            return Err(AppError::Unsupported(
                "page transfer into PDFs with annotations or form fields is disabled to avoid losing interactive data".into(),
            ));
        }
        if at > self.page_count() {
            return Err(AppError::Message(
                "page transfer position is out of bounds".into(),
            ));
        }
        let source_lopdf = Document::load_mem(source_bytes)
            .map_err(|error| AppError::Message(format!("could not read source PDF: {error}")))?;
        if source_lopdf.is_encrypted() {
            return Err(AppError::Unsupported(
                "encrypted source PDFs cannot be used for page transfer".into(),
            ));
        }
        if has_signatures(&source_lopdf) || has_acroform_or_page_annotations(&source_lopdf) {
            return Err(AppError::Unsupported(
                "source PDF has signatures, annotations, or forms and cannot be transferred without losing interactive data".into(),
            ));
        }
        let source_page_count = source_lopdf.get_pages().len() as u32;
        if source_page >= source_page_count {
            return Err(AppError::Message("source page is out of bounds".into()));
        }

        let before = self.pdfium.save_to_bytes()?;
        let source_pdfium = engine.load_document(source_bytes.to_vec(), None)?;
        let mut destination = engine.load_document(before.clone(), None)?;
        destination
            .pages_mut()
            .copy_page_range_from_document(
                &source_pdfium,
                source_page as i32..=source_page as i32,
                at as i32,
            )
            .map_err(|error| AppError::Message(format!("could not transfer PDF page: {error}")))?;
        let merged = destination.save_to_bytes()?;
        let merged_lopdf = Document::load_mem(&merged).map_err(|error| {
            AppError::Message(format!(
                "PDFium produced an invalid transferred PDF: {error}"
            ))
        })?;
        if merged_lopdf.get_pages().len() as u32 != self.page_count() + 1 {
            return Err(AppError::Message(
                "page transfer did not produce the expected page count".into(),
            ));
        }
        self.lopdf = merged_lopdf;
        self.pdfium = destination;
        self.annotations.clear();
        self.annotation_objects.clear();
        self.geometries = geometry::all_page_geometries(&self.lopdf);
        self.labels = page_labels(&self.pdfium);
        Ok((vec![at], before, Vec::new(), HashMap::new()))
    }

    pub fn save(&mut self, path: &Path, options: &SaveOptions) -> Result<PathBuf> {
        if has_signatures(&self.lopdf) && !options.force {
            return Err(AppError::Message(
                "This PDF contains a digital signature. Saving rewrites the file and will invalidate it. Save again with force to continue."
                    .into(),
            ));
        }

        if options.remove_password && options.password.is_some() {
            return Err(AppError::Message(
                "cannot remove encryption and set a new password in the same export".into(),
            ));
        }
        let output_password = options.password.as_deref().or_else(|| {
            (!options.remove_password)
                .then_some(self.password.as_deref())
                .flatten()
        });
        let mut annotated = self.lopdf.clone();
        let mut output_annotations = self.annotations.clone();
        let mut output_objects = self.annotation_objects.clone();
        if let Some(selected) = &options.pages {
            if selected.is_empty() {
                return Err(AppError::Message(
                    "select at least one page to extract".into(),
                ));
            }
            let mut unique = std::collections::HashSet::new();
            for page in selected {
                if *page >= self.page_count() || !unique.insert(*page) {
                    return Err(AppError::Message(
                        "extracted pages must be valid, unique page numbers".into(),
                    ));
                }
            }
            if !selected.windows(2).all(|pair| pair[0] < pair[1]) {
                return Err(AppError::Message(
                    "extracted page numbers must be in ascending order".into(),
                ));
            }
            let keep: std::collections::HashSet<u32> = selected.iter().copied().collect();
            let page_numbers: Vec<u32> = annotated.get_pages().keys().copied().collect();
            let removed: Vec<u32> = page_numbers
                .into_iter()
                .enumerate()
                .filter_map(|(index, key)| (!keep.contains(&(index as u32))).then_some(key))
                .collect();
            annotated.delete_pages(&removed);
            annotated.prune_objects();
            let mut remap = HashMap::new();
            for (new_index, old_index) in selected.iter().enumerate() {
                remap.insert(*old_index, new_index as u32);
            }
            output_annotations.retain_mut(|annotation| {
                if let Some(new_index) = remap.get(&annotation.page) {
                    annotation.page = *new_index;
                    true
                } else {
                    false
                }
            });
            output_objects.retain(|id, _| {
                output_annotations
                    .iter()
                    .any(|annotation| &annotation.id == id)
            });
        }
        let annotation_objects = annotations::write_annotations(
            &mut annotated,
            &output_annotations,
            &output_objects,
            &[],
        )?;
        let mut written_document = annotated.clone();
        if let Some(password) = output_password {
            if password.is_empty() {
                return Err(AppError::Message("password must not be empty".into()));
            }
            let mut file_key = [0u8; 32];
            rand::rng().fill(&mut file_key);
            let filters: std::collections::BTreeMap<Vec<u8>, Arc<dyn CryptFilter>> =
                std::collections::BTreeMap::from([(
                    b"StdCF".to_vec(),
                    Arc::new(Aes256CryptFilter) as Arc<dyn CryptFilter>,
                )]);
            let encryption = EncryptionState::try_from(EncryptionVersion::V5 {
                encrypt_metadata: true,
                crypt_filters: filters,
                file_encryption_key: &file_key,
                stream_filter: b"StdCF".to_vec(),
                string_filter: b"StdCF".to_vec(),
                owner_password: password,
                user_password: password,
                permissions: Permissions::all(),
            })?;
            written_document.encrypt(&encryption)?;
        }
        let mut bytes = Vec::new();
        written_document.save_to(&mut bytes)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut temporary, &bytes)?;
        temporary
            .persist(path)
            .map_err(|error| AppError::Other(format!("could not replace PDF: {}", error.error)))?;

        if options.pages.is_none() {
            self.password = output_password.map(str::to_owned);
            for annotation in &mut self.annotations {
                annotation.existing = true;
                annotation.dirty = false;
            }
            let live: std::collections::HashSet<&str> =
                self.annotations.iter().map(|a| a.id.as_str()).collect();
            let annotation_objects: HashMap<String, ObjectId> = annotation_objects
                .into_iter()
                .filter(|(id, _)| live.contains(id.as_str()))
                .collect();
            self.annotation_objects = annotation_objects;
            self.lopdf = annotated;
        }
        Ok(path.to_path_buf())
    }

    pub fn metadata(&self) -> MetadataReport {
        let mut entries = Vec::new();
        let mut push = |key: &str, value: String| {
            if !value.trim().is_empty() {
                entries.push(MetadataEntry {
                    group: "PDF".into(),
                    key: key.into(),
                    value,
                });
            }
        };

        if let Ok(info_ref) = self
            .lopdf
            .trailer
            .get(b"Info")
            .and_then(Object::as_reference)
        {
            if let Ok(info) = self.lopdf.get_dictionary(info_ref) {
                for (key, label) in [
                    (b"Title".as_slice(), "Title"),
                    (b"Author".as_slice(), "Author"),
                    (b"Subject".as_slice(), "Subject"),
                    (b"Keywords".as_slice(), "Keywords"),
                    (b"Creator".as_slice(), "Creator"),
                    (b"Producer".as_slice(), "Producer"),
                    (b"CreationDate".as_slice(), "Created"),
                    (b"ModDate".as_slice(), "Modified"),
                ] {
                    if let Ok(object) = info.get(key) {
                        if let Ok(value) = lopdf::decode_text_string(object) {
                            push(label, value);
                        }
                    }
                }
            }
        }

        push("Pages", self.page_count().to_string());
        push(
            "Page size",
            self.geometries
                .get(0)
                .map(|geom| format!("{:.0} × {:.0} pt", geom.width(), geom.height()))
                .unwrap_or_default(),
        );
        push(
            "Encryption",
            if self.password.is_some() {
                "AES/RC4"
            } else {
                "none"
            }
            .into(),
        );
        push(
            "Digital signature",
            if has_signatures(&self.lopdf) {
                "present"
            } else {
                "none"
            }
            .into(),
        );
        push("Annotations", self.annotations.len().to_string());

        MetadataReport {
            entries,
            can_strip_gps: false,
            gps_present: false,
        }
    }

    pub fn info(&self, id: &str, path: &Path, version: u64, dirty: bool) -> DocumentInfo {
        DocumentInfo {
            id: id.to_string(),
            kind: DocumentKind::Pdf,
            name: file_name(path),
            path: Some(path.display().to_string()),
            page_count: self.page_count(),
            version,
            dirty,
            can_undo: false,
            can_redo: false,
            encrypted: self.password.is_some(),
            read_only: false,
            signed: has_signatures(&self.lopdf),
            pages: self.page_info(),
            annotations: self.annotations.clone(),
            image: None,
            mime: "image/png".into(),
        }
    }
}

fn page_id_at(doc: &Document, index: u32) -> Result<ObjectId> {
    doc.get_pages()
        .values()
        .nth(index as usize)
        .copied()
        .ok_or_else(|| AppError::Other("page is out of bounds".into()))
}

fn remap_annotation_pages(annotations: &mut [Annotation], from: u32, to: u32) {
    for annotation in annotations {
        if annotation.page == from {
            annotation.page = to;
        } else if from < to && annotation.page > from && annotation.page <= to {
            annotation.page -= 1;
        } else if from > to && annotation.page >= to && annotation.page < from {
            annotation.page += 1;
        }
    }
}

fn has_acroform_or_page_annotations(doc: &Document) -> bool {
    if doc.catalog().is_ok_and(|catalog| catalog.has(b"AcroForm")) {
        return true;
    }
    doc.get_pages().values().any(|page_id| {
        let Ok(page) = doc.get_dictionary(*page_id) else {
            return false;
        };
        let Ok(annots) = page.get(b"Annots") else {
            return false;
        };
        match annots {
            Object::Array(items) => !items.is_empty(),
            Object::Reference(id) => doc
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_array().ok())
                .is_some_and(|items| !items.is_empty()),
            _ => true,
        }
    })
}

fn page_labels(document: &PdfDocument<'static>) -> Vec<Option<String>> {
    document
        .pages()
        .iter()
        .map(|page| page.label().map(str::to_string))
        .collect()
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

#[derive(Debug)]
struct OcrWord {
    text: String,
    left: u32,
    top: u32,
    height: u32,
}

fn run_tesseract_tsv(png: &[u8]) -> Result<Vec<OcrWord>> {
    let mut input = tempfile::NamedTempFile::new()?;
    input.write_all(png)?;
    input.flush()?;
    let output = tempfile::NamedTempFile::new()?;
    let process = Command::new("tesseract")
        .arg(input.path())
        .arg("stdout")
        .args(["--dpi", "300", "-l", "eng", "--psm", "3", "tsv"])
        .stdout(Stdio::from(output.reopen()?))
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| {
            AppError::Unsupported(format!(
                "OCR requires Tesseract 5 with English language data installed and available on PATH: {error}"
            ))
        })?;
    if !process.status.success() {
        let details = String::from_utf8_lossy(&process.stderr);
        let details: String = details.chars().take(500).collect();
        return Err(AppError::Unsupported(format!(
            "Tesseract OCR failed: {}",
            details.trim()
        )));
    }
    let output_len = std::fs::metadata(output.path())?.len();
    if output_len > 64 * 1024 * 1024 {
        return Err(AppError::Unsupported(
            "Tesseract output exceeds the 64 MiB safety limit".into(),
        ));
    }
    let tsv = std::fs::read_to_string(output.path())?;
    let mut words = Vec::new();
    for line in tsv.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 12 || fields[0] != "5" {
            continue;
        }
        let confidence = fields[10].parse::<f32>().unwrap_or(-1.0);
        let text = fields[11..].join(" ").trim().to_string();
        if confidence < 15.0 || text.is_empty() {
            continue;
        }
        let Ok(left) = fields[6].parse::<u32>() else {
            continue;
        };
        let Ok(top) = fields[7].parse::<u32>() else {
            continue;
        };
        let Ok(height) = fields[9].parse::<u32>() else {
            continue;
        };
        if fields[8].parse::<u32>().unwrap_or(0) == 0 || height == 0 {
            continue;
        }
        words.push(OcrWord {
            text,
            left,
            top,
            height,
        });
    }
    Ok(words)
}

pub fn raster_export(
    state: &PdfState,
    engine: &PdfEngine,
    path: &Path,
    options: &SaveOptions,
) -> Result<()> {
    let format = options
        .format
        .clone()
        .unwrap_or_else(|| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("png")
                .to_ascii_lowercase()
        })
        .to_ascii_lowercase();
    let page = options.page.unwrap_or(0);
    let dpi = options.dpi.unwrap_or(150.0);
    let (png, _, _) = state.render_for_export(engine, page, dpi)?;
    let image = image::load_from_memory(&png)?;
    match format.as_str() {
        "png" => {
            std::fs::write(path, &png)?;
        }
        "jpg" | "jpeg" => {
            let rgb = image.to_rgb8();
            let file = std::fs::File::create(path)?;
            let mut writer = std::io::BufWriter::new(file);
            let quality = options.quality.unwrap_or(90);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, quality).encode(
                &rgb,
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )?;
        }
        other => {
            return Err(AppError::Message(format!(
                "PDF export supports PDF, PNG and JPEG, not {other}"
            )));
        }
    }
    Ok(())
}

pub fn export_path_format(path: &Path, options: &SaveOptions) -> String {
    options
        .format
        .clone()
        .unwrap_or_else(|| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("pdf")
                .to_ascii_lowercase()
        })
        .to_ascii_lowercase()
}
