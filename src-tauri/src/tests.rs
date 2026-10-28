use std::path::Path;

use base64::Engine as _;
use image::{ImageDecoder as _, ImageEncoder as _};
use lopdf::dictionary;

use crate::error::Result;
use crate::geometry::PageGeom;
use crate::model::{Annotation, AnnotationKind, Operation, Point, Rect, SaveOptions};
use crate::pdfium_engine::PdfEngine;
use crate::service::{AppState, DocumentState};

fn test_pdf(path: &Path) -> Result<()> {
    let mut doc = lopdf::Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"
    });
    let resources = dictionary! {
        "Font" => dictionary! { "F1" => font_id }
    };
    let mut page_ids = Vec::new();
    for (index, text) in ["Page one", "Page two", "Page three"].iter().enumerate() {
        let content_id = doc.add_object(lopdf::Stream::new(
            dictionary! {},
            format!("BT /F1 28 Tf 60 700 Td ({text}) Tj ET").into_bytes(),
        ));
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => resources.clone(),
            "Contents" => content_id,
        };
        if index == 2 {
            page.set("Rotate", 90i64);
        }
        page_ids.push(doc.add_object(page));
    }
    doc.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().map(|id| lopdf::Object::Reference(*id)).collect::<Vec<_>>(),
            "Count" => 3i64,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    doc.save(path)?;
    Ok(())
}

fn test_form_pdf(path: &Path) -> Result<()> {
    test_pdf(path)?;
    let mut doc = lopdf::Document::load(path)?;
    let page_id = doc.get_pages().values().next().copied().unwrap();
    let field_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => lopdf::Object::string_literal("FirstName"),
        "V" => lopdf::Object::string_literal("Before"),
        "Rect" => vec![60.into(), 600.into(), 220.into(), 640.into()],
        "P" => page_id,
    });
    doc.get_dictionary_mut(page_id)?
        .set("Annots", vec![lopdf::Object::Reference(field_id)]);
    let fields_id = doc.add_object(lopdf::Object::Array(vec![lopdf::Object::Reference(
        field_id,
    )]));
    let form_id = doc.add_object(dictionary! {
        "Fields" => fields_id,
        "NeedAppearances" => true,
    });
    let catalog_id = doc.trailer.get(b"Root")?.as_reference()?;
    doc.get_dictionary_mut(catalog_id)?.set("AcroForm", form_id);
    doc.save(path)?;
    Ok(())
}

fn test_radio_form_pdf(path: &Path) -> Result<()> {
    test_pdf(path)?;
    let mut doc = lopdf::Document::load(path)?;
    let page_id = doc.get_pages().values().next().copied().unwrap();
    let field_id = doc.new_object_id();
    let off = doc.add_object(lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 16.into(), 16.into()],
            "Resources" => dictionary! {},
        },
        b"q Q".to_vec(),
    ));
    let choice_a = doc.add_object(lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 16.into(), 16.into()],
            "Resources" => dictionary! {},
        },
        b"q Q".to_vec(),
    ));
    let choice_b = doc.add_object(lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 16.into(), 16.into()],
            "Resources" => dictionary! {},
        },
        b"q Q".to_vec(),
    ));
    let widget_a = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "Parent" => field_id,
        "Rect" => vec![40.into(), 560.into(), 60.into(), 580.into()], "P" => page_id,
        "AS" => "ChoiceA",
        "AP" => dictionary! { "N" => dictionary! { "Off" => off, "ChoiceA" => choice_a } },
    });
    let widget_b = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "Parent" => field_id,
        "Rect" => vec![80.into(), 560.into(), 100.into(), 580.into()], "P" => page_id,
        "AS" => "Off",
        "AP" => dictionary! { "N" => dictionary! { "Off" => off, "ChoiceB" => choice_b } },
    });
    doc.objects.insert(
        field_id,
        lopdf::Object::Dictionary(dictionary! {
            "FT" => "Btn", "T" => lopdf::Object::string_literal("RadioChoice"),
            "Ff" => 32768, "V" => "ChoiceA",
            "Kids" => vec![lopdf::Object::Reference(widget_a), lopdf::Object::Reference(widget_b)],
        }),
    );
    doc.get_dictionary_mut(page_id)?.set(
        "Annots",
        vec![
            lopdf::Object::Reference(widget_a),
            lopdf::Object::Reference(widget_b),
        ],
    );
    let fields_id = doc.add_object(lopdf::Object::Array(vec![lopdf::Object::Reference(
        field_id,
    )]));
    let form_id = doc.add_object(dictionary! { "Fields" => fields_id, "NeedAppearances" => true });
    let catalog_id = doc.trailer.get(b"Root")?.as_reference()?;
    doc.get_dictionary_mut(catalog_id)?.set("AcroForm", form_id);
    doc.save(path)?;
    Ok(())
}

fn test_scanned_pdf(path: &Path) -> Result<()> {
    use std::io::Write as _;

    let width = 640u32;
    let height = 200u32;
    let mut pixels = vec![255u8; (width * height * 3) as usize];
    let glyphs: [[u8; 7]; 4] = [
        [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ], // H
        [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ], // E
        [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ], // L
        [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ], // O
    ];
    let scale = 6u32;
    let start_x = 60u32;
    let start_y = 60u32;
    let glyph_indices = [0usize, 1, 2, 2, 3];
    for (character, glyph_index) in glyph_indices.into_iter().enumerate() {
        for (row, bits) in glyphs[glyph_index].iter().enumerate() {
            for column in 0..5u32 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = start_x + character as u32 * scale * 6 + column * scale + dx;
                        let y = start_y + row as u32 * scale + dy;
                        let offset = ((y * width + x) * 3) as usize;
                        pixels[offset..offset + 3].fill(0);
                    }
                }
            }
        }
    }
    let mut compressor = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    compressor.write_all(&pixels)?;
    let image_stream = lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width as i64,
            "Height" => height as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
        },
        compressor.finish()?,
    );
    let mut doc = lopdf::Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let image_id = doc.add_object(image_stream);
    let content_id = doc.add_object(lopdf::Stream::new(
        dictionary! {},
        b"q 612 0 0 192 0 300 cm /Im0 Do Q".to_vec(),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image_id } },
        "Contents" => content_id,
    });
    doc.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![lopdf::Object::Reference(page_id)],
            "Count" => 1i64,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    doc.save(path)?;
    Ok(())
}

/// pdfium-render binds the shared library into a process-wide static; loading it
/// more than once in one process fails. Tests share a single engine.
fn engine() -> &'static PdfEngine {
    use std::sync::OnceLock;
    static ENGINE: OnceLock<PdfEngine> = OnceLock::new();
    ENGINE.get_or_init(|| PdfEngine::load(None).expect("pdfium"))
}

fn open_test_pdf() -> (tempfile::TempDir, DocumentState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.pdf");
    test_pdf(&path).unwrap();
    let engine = engine();
    let document = DocumentState::open(&engine, "doc".into(), &path, None).expect("open fixture");
    (dir, document)
}

#[test]
fn opens_and_renders_pdf() {
    let (_dir, document) = open_test_pdf();
    let engine = engine();
    assert_eq!(document.page_count(), 3);
    let info = document.info();
    assert_eq!(info.page_count, 3);
    assert_eq!(info.pages[0].width, 612.0);
    assert_eq!(info.pages[2].width, 792.0);
    assert_eq!(info.pages[2].height, 612.0);
    assert_eq!(info.pages[2].rotation, 90);
    let (png, mime) = document.render(&engine, 0, 96.0).unwrap();
    assert_eq!(mime, "image/png");
    let image = image::load_from_memory(&png).unwrap();
    assert!(image.width() > 700);
    assert!(image.height() > 900);
}

#[test]
fn finds_text_with_display_rects() {
    let (_dir, document) = open_test_pdf();
    let engine = engine();
    let matches = document.find_text(&engine, "Page two").unwrap();
    assert!(!matches.is_empty());
    assert!(matches.iter().all(|m| m.page == 1));
    let first = &matches[0];
    assert!(first.rect.x >= 50.0 && first.rect.x < 100.0);
    assert!(first.rect.width > 20.0);
}

#[test]
fn redaction_removes_searchable_text_and_is_undoable() {
    let (dir, mut document) = open_test_pdf();
    let engine = engine();
    assert!(!document.find_text(&engine, "Page one").unwrap().is_empty());

    document
        .apply(
            &engine,
            Operation::Redact {
                page: 0,
                rect: Rect::new(52.0, 64.0, 160.0, 48.0),
            },
        )
        .unwrap();
    assert!(document.find_text(&engine, "Page one").unwrap().is_empty());

    let redacted = dir.path().join("redacted.pdf");
    document
        .save(&redacted, &SaveOptions::default(), true)
        .unwrap();
    let reopened =
        crate::pdfdoc::PdfState::open(&engine, &std::fs::read(&redacted).unwrap(), None).unwrap();
    assert!(reopened.find_text(&engine, "Page one").unwrap().is_empty());

    document.undo(&engine).unwrap();
    assert!(!document.find_text(&engine, "Page one").unwrap().is_empty());
    document.redo(&engine).unwrap();
    assert!(document.find_text(&engine, "Page one").unwrap().is_empty());
}

#[test]
fn form_updates_are_atomic_and_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("form.pdf");
    test_form_pdf(&path).unwrap();
    let engine = engine();
    let mut document = DocumentState::open(engine, "form-doc".into(), &path, None).unwrap();
    let initial = document.pdf.as_ref().unwrap().form_fields();
    assert_eq!(initial[0].name, "FirstName");
    assert_eq!(initial[0].value, "Before");

    assert!(
        document
            .apply(
                engine,
                Operation::FillFormFields {
                    fields: vec![
                        crate::model::FormFieldUpdate {
                            field: "FirstName".into(),
                            value: "Partial".into(),
                        },
                        crate::model::FormFieldUpdate {
                            field: "MissingField".into(),
                            value: "value".into(),
                        },
                    ],
                },
            )
            .is_err()
    );
    assert_eq!(
        document.pdf.as_ref().unwrap().form_fields()[0].value,
        "Before"
    );

    document
        .apply(
            engine,
            Operation::FillFormFields {
                fields: vec![crate::model::FormFieldUpdate {
                    field: "FirstName".into(),
                    value: "Ada".into(),
                }],
            },
        )
        .unwrap();
    assert_eq!(document.pdf.as_ref().unwrap().form_fields()[0].value, "Ada");
    document.undo(engine).unwrap();
    assert_eq!(
        document.pdf.as_ref().unwrap().form_fields()[0].value,
        "Before"
    );
    document.redo(engine).unwrap();
    assert_eq!(document.pdf.as_ref().unwrap().form_fields()[0].value, "Ada");
}

#[test]
fn radio_form_fields_expose_choices_and_select_the_matching_widget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("radio-form.pdf");
    test_radio_form_pdf(&path).unwrap();
    let engine = engine();
    let mut document = DocumentState::open(engine, "radio-form-doc".into(), &path, None).unwrap();
    let initial = document.pdf.as_ref().unwrap().form_fields();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].kind, "radio");
    assert_eq!(initial[0].value, "ChoiceA");
    assert!(initial[0].options.contains(&"ChoiceB".to_string()));

    document
        .apply(
            engine,
            Operation::FillFormFields {
                fields: vec![crate::model::FormFieldUpdate {
                    field: "RadioChoice".into(),
                    value: "ChoiceB".into(),
                }],
            },
        )
        .unwrap();
    let pdf = document.pdf.as_ref().unwrap();
    let fields = pdf.form_fields();
    assert_eq!(fields[0].value, "ChoiceB");
    let widgets: Vec<_> = pdf
        .lopdf
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| {
            dict.get(b"Subtype")
                .and_then(lopdf::Object::as_name)
                .is_ok_and(|name| name == b"Widget")
        })
        .map(|dict| {
            dict.get(b"AS")
                .and_then(lopdf::Object::as_name)
                .unwrap()
                .to_vec()
        })
        .collect();
    assert!(widgets.contains(&b"Off".to_vec()));
    assert!(widgets.contains(&b"ChoiceB".to_vec()));
    assert!(!widgets.contains(&b"ChoiceA".to_vec()));
}

#[test]
fn ocr_adds_searchable_invisible_text_and_is_undoable() {
    let available = std::process::Command::new("tesseract")
        .args(["--list-langs"])
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("eng")
        });
    if !available {
        eprintln!("skipping Tesseract OCR test because Tesseract English data is unavailable");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scanned.pdf");
    test_scanned_pdf(&path).unwrap();
    let engine = engine();
    let mut document = DocumentState::open(engine, "scanned-doc".into(), &path, None).unwrap();
    assert!(document.find_text(engine, "HELL").unwrap().is_empty());
    document.apply(engine, Operation::OcrDocument).unwrap();
    assert!(!document.find_text(engine, "HELL").unwrap().is_empty());
    document.undo(engine).unwrap();
    assert!(document.find_text(engine, "HELL").unwrap().is_empty());
    document.redo(engine).unwrap();
    assert!(!document.find_text(engine, "HELL").unwrap().is_empty());
}

#[test]
fn rotate_undo_and_redo() {
    let (_dir, mut document) = open_test_pdf();
    let engine = engine();
    let result = document
        .apply(
            &engine,
            Operation::RotatePage {
                page: 0,
                degrees: 90,
            },
        )
        .unwrap();
    assert_eq!(result.changed_pages, vec![0]);
    assert_eq!(document.info().pages[0].rotation, 90);
    assert_eq!(document.info().pages[0].width, 792.0);

    let undo = document.undo(&engine).unwrap();
    assert_eq!(undo.version, 3);
    assert_eq!(document.info().pages[0].rotation, 0);

    document.redo(&engine).unwrap();
    assert_eq!(document.info().pages[0].rotation, 90);
}

#[test]
fn delete_page_and_undo_restores_annotations() {
    let (_dir, mut document) = open_test_pdf();
    let engine = engine();
    let annotation = Annotation::new(
        "note-1".into(),
        1,
        AnnotationKind::Highlight,
        Rect::new(60.0, 60.0, 200.0, 20.0),
    );
    document
        .apply(
            &engine,
            Operation::AddAnnotation {
                page: 1,
                annotation,
            },
        )
        .unwrap();

    document
        .apply(&engine, Operation::DeletePage { page: 1 })
        .unwrap();
    assert_eq!(document.page_count(), 2);
    assert!(document.info().annotations.is_empty());

    document.undo(&engine).unwrap();
    assert_eq!(document.page_count(), 3);
    assert_eq!(document.info().annotations.len(), 1);
    assert_eq!(document.info().annotations[0].page, 1);
}

#[test]
fn reorder_pages_keeps_annotations_aligned() {
    let (_dir, mut document) = open_test_pdf();
    let engine = engine();
    let annotation = Annotation::new(
        "note-2".into(),
        0,
        AnnotationKind::Square,
        Rect::new(50.0, 50.0, 100.0, 80.0),
    );
    document
        .apply(
            &engine,
            Operation::AddAnnotation {
                page: 0,
                annotation,
            },
        )
        .unwrap();
    document
        .apply(&engine, Operation::ReorderPages { from: 0, to: 2 })
        .unwrap();
    assert_eq!(document.info().annotations[0].page, 2);
    document.undo(&engine).unwrap();
    assert_eq!(document.info().annotations[0].page, 0);
}

#[test]
fn save_writes_annotations_with_appearance_streams() {
    let (dir, mut document) = open_test_pdf();
    let engine = engine();
    let mut highlight = Annotation::new(
        "hl".into(),
        0,
        AnnotationKind::Highlight,
        Rect::new(60.0, 700.0, 180.0, 24.0),
    );
    highlight.color = "#ffd43b".into();
    document
        .apply(
            &engine,
            Operation::AddAnnotation {
                page: 0,
                annotation: highlight,
            },
        )
        .unwrap();

    let mut text = Annotation::new(
        "txt".into(),
        0,
        AnnotationKind::FreeText,
        Rect::new(60.0, 600.0, 220.0, 60.0),
    );
    text.contents = Some("Review this paragraph".into());
    document
        .apply(
            &engine,
            Operation::AddAnnotation {
                page: 0,
                annotation: text,
            },
        )
        .unwrap();

    let output = dir.path().join("saved.pdf");
    document
        .save(&output, &SaveOptions::default(), true)
        .unwrap();

    let saved = lopdf::Document::load(&output).unwrap();
    let page_id = *saved.get_pages().values().next().unwrap();
    let annotations = saved.get_page_annotations(page_id).unwrap();
    assert_eq!(annotations.len(), 2);
    for annotation in annotations {
        let subtype = annotation.get(b"Subtype").unwrap().as_name().unwrap();
        assert!(subtype == b"Highlight" || subtype == b"FreeText");
        assert!(annotation.get(b"AP").is_ok());
        assert!(annotation.get(b"NM").is_ok());
        let appearance = annotation
            .get(b"AP")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"N")
            .unwrap()
            .as_reference()
            .unwrap();
        let stream = saved.get_object(appearance).unwrap().as_stream().unwrap();
        assert!(!stream.content.is_empty());
    }

    // The saved file must still render through pdfium.
    let pdfium = engine
        .load_document(std::fs::read(&output).unwrap(), None)
        .unwrap();
    assert_eq!(pdfium.pages().len(), 3);

    // Editing an existing annotation and saving again should not duplicate it.
    let annotation = document.info().annotations[0].clone();
    let mut edited = annotation.clone();
    edited.color = "#40c057".into();
    document
        .apply(
            &engine,
            Operation::EditAnnotation {
                id: annotation.id,
                before: None,
                after: edited,
            },
        )
        .unwrap();
    document
        .save(&output, &SaveOptions::default(), true)
        .unwrap();
    let saved = lopdf::Document::load(&output).unwrap();
    let page_id = *saved.get_pages().values().next().unwrap();
    assert_eq!(saved.get_page_annotations(page_id).unwrap().len(), 2);
}

#[test]
fn signature_appearance_keeps_transparent_pixels() {
    let (dir, mut document) = open_test_pdf();
    let signature_pixels =
        image::RgbaImage::from_raw(2, 1, vec![20, 30, 40, 0, 20, 30, 40, 255]).unwrap();
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(signature_pixels)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let mut signature = Annotation::new(
        "signature".into(),
        0,
        AnnotationKind::Signature,
        Rect::new(60.0, 500.0, 200.0, 70.0),
    );
    signature.image_data = Some(base64::engine::general_purpose::STANDARD.encode(png));
    document
        .apply(
            &engine(),
            Operation::AddAnnotation {
                page: 0,
                annotation: signature,
            },
        )
        .unwrap();
    let path = dir.path().join("signed-image.pdf");
    document.save(&path, &SaveOptions::default(), true).unwrap();

    let saved = lopdf::Document::load(&path).unwrap();
    let page_id = *saved.get_pages().values().next().unwrap();
    let annotation = saved.get_page_annotations(page_id).unwrap().remove(0);
    assert_eq!(
        annotation.get(b"Subtype").unwrap().as_name().unwrap(),
        b"Stamp"
    );
    let appearance_id = annotation
        .get(b"AP")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"N")
        .unwrap()
        .as_reference()
        .unwrap();
    let appearance = saved
        .get_object(appearance_id)
        .unwrap()
        .as_stream()
        .unwrap();
    let image_id = appearance
        .dict
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"XObject")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Im0")
        .unwrap()
        .as_reference()
        .unwrap();
    let image = saved.get_object(image_id).unwrap().as_stream().unwrap();
    let mask_id = image.dict.get(b"SMask").unwrap().as_reference().unwrap();
    let mask = saved.get_object(mask_id).unwrap().as_stream().unwrap();
    assert_eq!(mask.content, vec![0, 255]);
}

#[test]
fn raster_export_includes_unsaved_annotations() {
    let (dir, mut document) = open_test_pdf();
    let engine = engine();
    let mut square = Annotation::new(
        "export-square".into(),
        0,
        AnnotationKind::Square,
        Rect::new(120.0, 120.0, 110.0, 90.0),
    );
    square.color = "#ff0000".into();
    square.fill = Some("#ff0000".into());
    document
        .apply(
            &engine,
            Operation::AddAnnotation {
                page: 0,
                annotation: square,
            },
        )
        .unwrap();

    let output = dir.path().join("annotated-export.png");
    crate::pdfdoc::raster_export(
        document.pdf.as_ref().unwrap(),
        &engine,
        &output,
        &SaveOptions {
            format: Some("png".into()),
            page: Some(0),
            dpi: Some(72.0),
            ..SaveOptions::default()
        },
    )
    .unwrap();
    let image = image::open(output).unwrap().to_rgba8();
    let pixel = image.get_pixel(160, 160);
    assert!(
        pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100,
        "pixel: {pixel:?}"
    );
}

#[test]
fn geometry_rect_mapping_is_stable() {
    let geom = PageGeom {
        crop_x: 10.0,
        crop_y: 20.0,
        raw_width: 300.0,
        raw_height: 500.0,
        rotation: 270,
    };
    let rect = Rect::new(12.0, 15.0, 100.0, 50.0);
    let user = geom.rect_to_user(rect);
    let back = geom.rect_to_display(user);
    assert!((back.x - rect.x).abs() < 0.01);
    assert!((back.y - rect.y).abs() < 0.01);
    assert!((back.width - rect.width).abs() < 0.01);
    assert!((back.height - rect.height).abs() < 0.01);
}

fn write_png(path: &Path, width: u32, height: u32) {
    let mut image = image::RgbaImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x % 255) as u8, (y % 255) as u8, 128, 200]);
    }
    image.save(path).unwrap();
}

#[test]
fn image_crop_resize_undo_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.png");
    write_png(&path, 120, 80);
    let engine = engine();
    let mut document = DocumentState::open(&engine, "img".into(), &path, None).unwrap();
    assert_eq!(document.info().image.as_ref().unwrap().width, 120);
    assert_eq!(document.info().mime, "image/png");

    document
        .apply(
            &engine,
            Operation::CropImage {
                x: 10,
                y: 10,
                width: 50,
                height: 40,
            },
        )
        .unwrap();
    assert_eq!(document.info().image.as_ref().unwrap().width, 50);
    assert_eq!(document.info().image.as_ref().unwrap().height, 40);

    document.undo(&engine).unwrap();
    assert_eq!(document.info().image.as_ref().unwrap().width, 120);

    document
        .apply(
            &engine,
            Operation::ResizeImage {
                width: 60,
                height: 40,
            },
        )
        .unwrap();
    let (png, _) = document.render(&engine, 0, 72.0).unwrap();
    let rendered = image::load_from_memory(&png).unwrap();
    assert_eq!(rendered.width(), 60);

    let exported = dir.path().join("out.png");
    document
        .save(&exported, &SaveOptions::default(), true)
        .unwrap();
    let saved = image::open(&exported).unwrap();
    assert_eq!(saved.width(), 60);
    assert_eq!(saved.height(), 40);
    assert_eq!(saved.color().has_alpha(), true);
}

#[test]
fn jpeg2000_export_roundtrips_and_curves_are_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("curve-source.png");
    let pixels = image::RgbaImage::from_raw(
        4,
        2,
        vec![
            0, 0, 0, 255, 64, 64, 64, 255, 128, 128, 128, 255, 255, 255, 255, 255, 10, 20, 30, 128,
            40, 50, 60, 128, 70, 80, 90, 128, 100, 110, 120, 128,
        ],
    )
    .unwrap();
    image::DynamicImage::ImageRgba8(pixels.clone())
        .save(&path)
        .unwrap();
    let mut state =
        crate::image_doc::ImageState::open(std::fs::read(&path).unwrap(), &path).unwrap();
    let original = state.current.clone();
    state
        .apply(Operation::ImageCurves {
            points: vec![
                Point::new(0.0, 0.0),
                Point::new(0.5, 0.8),
                Point::new(1.0, 1.0),
            ],
        })
        .unwrap();
    assert!(state.current.get_pixel(1, 0)[0] > original.get_pixel(1, 0)[0]);
    assert!(state.undo());
    assert_eq!(state.current, original);

    let encoded = state
        .encode(crate::image_doc::ImageFormatKind::Jpeg2000, 90)
        .unwrap();
    assert!(encoded.starts_with(b"\0\0\0\x0cjP  \r\n\x87\n"));
    let decoded = jpeg2k::Image::from_bytes(&encoded).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (4, 2));
    assert!(decoded.num_components() >= 3);
}

#[test]
fn png_and_jpeg_exports_retain_embedded_icc_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile-source.png");
    let mut profile = vec![0u8; 132];
    profile[0..4].copy_from_slice(&(132u32).to_be_bytes());
    profile[12..16].copy_from_slice(b"mntr");
    profile[16..20].copy_from_slice(b"RGB ");
    profile[20..24].copy_from_slice(b"XYZ ");
    profile[36..40].copy_from_slice(b"acsp");
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([40, 80, 120, 255]));
    let mut source = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut source);
    encoder.set_icc_profile(profile.clone()).unwrap();
    encoder
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    std::fs::write(&path, source).unwrap();
    let state = crate::image_doc::ImageState::open(std::fs::read(&path).unwrap(), &path).unwrap();
    assert_eq!(state.icc_profile, Some(profile.clone()));

    for (format, image_format) in [
        (
            crate::image_doc::ImageFormatKind::Png,
            image::ImageFormat::Png,
        ),
        (
            crate::image_doc::ImageFormatKind::Jpeg,
            image::ImageFormat::Jpeg,
        ),
        (
            crate::image_doc::ImageFormatKind::WebP,
            image::ImageFormat::WebP,
        ),
    ] {
        let bytes = state.encode(format, 92).unwrap();
        let reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image_format);
        let mut decoder = reader.into_decoder().unwrap();
        assert_eq!(
            decoder.icc_profile().unwrap(),
            Some(profile.clone()),
            "profile lost in {format:?}"
        );
    }

    let tiff = state
        .encode(crate::image_doc::ImageFormatKind::Tiff, 92)
        .unwrap();
    assert!(tiff.windows(profile.len()).any(|window| window == profile));
    let tiff_path = dir.path().join("profile-output.tiff");
    std::fs::write(&tiff_path, &tiff).unwrap();
    let reopened_tiff = crate::image_doc::ImageState::open(tiff, &tiff_path).unwrap();
    assert_eq!(reopened_tiff.icc_profile, Some(profile.clone()));

    let jp2 = state
        .encode(crate::image_doc::ImageFormatKind::Jpeg2000, 92)
        .unwrap();
    assert!(jp2.windows(profile.len()).any(|window| window == profile));
    let jp2_path = dir.path().join("profile-output.jp2");
    std::fs::write(&jp2_path, &jp2).unwrap();
    let reopened = crate::image_doc::ImageState::open(jp2, &jp2_path).unwrap();
    assert_eq!(reopened.icc_profile, Some(profile));
}

#[test]
fn instant_alpha_makes_matching_pixels_transparent_and_is_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alpha.png");
    let image = image::RgbaImage::from_raw(
        3,
        1,
        vec![255, 255, 255, 255, 244, 244, 244, 255, 12, 34, 56, 255],
    )
    .unwrap();
    image.save(&path).unwrap();
    let mut state =
        crate::image_doc::ImageState::open(std::fs::read(&path).unwrap(), &path).unwrap();
    state
        .apply(Operation::InstantAlpha {
            color: "#ffffff".into(),
            tolerance: 12,
        })
        .unwrap();
    assert_eq!(state.current.get_pixel(0, 0)[3], 0);
    assert_eq!(state.current.get_pixel(1, 0)[3], 0);
    assert_eq!(state.current.get_pixel(2, 0)[3], 255);
    assert!(state.undo());
    assert_eq!(state.current.get_pixel(0, 0)[3], 255);
}

#[test]
fn image_exports_to_pdf() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.png");
    write_png(&path, 64, 48);
    let engine = engine();
    let mut document = DocumentState::open(&engine, "img".into(), &path, None).unwrap();
    let pdf_path = dir.path().join("photo.pdf");
    document
        .save(&pdf_path, &SaveOptions::default(), false)
        .unwrap();
    let pdfium = engine
        .load_document(std::fs::read(&pdf_path).unwrap(), None)
        .unwrap();
    assert_eq!(pdfium.pages().len(), 1);
    let page = pdfium.pages().get(0).unwrap();
    assert_eq!(page.width().value, 64.0);
    assert_eq!(page.height().value, 48.0);
}

#[test]
fn extracted_pdf_contains_only_selected_pages_and_does_not_mutate_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.pdf");
    test_pdf(&source).unwrap();
    let mut pdf =
        crate::pdfdoc::PdfState::open(&engine(), &std::fs::read(&source).unwrap(), None).unwrap();
    let output = dir.path().join("extract.pdf");
    pdf.save(
        &output,
        &SaveOptions {
            pages: Some(vec![0, 2]),
            force: true,
            ..SaveOptions::default()
        },
    )
    .unwrap();
    let extracted = lopdf::Document::load(&output).unwrap();
    assert_eq!(extracted.get_pages().len(), 2);
    assert_eq!(pdf.page_count(), 3);
    let extracted_page_ids: Vec<_> = extracted.get_pages().values().copied().collect();
    let extracted_page_three_content = extracted.get_page_content(extracted_page_ids[1]);
    assert!(String::from_utf8_lossy(&extracted_page_three_content).contains("Page three"));
    assert_eq!(
        extracted
            .get_dictionary(extracted_page_ids[1])
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        90
    );
    let extracted_pdfium = engine()
        .load_document(std::fs::read(&output).unwrap(), None)
        .unwrap();
    let pages: Vec<String> = (0..extracted_pdfium.pages().len())
        .map(|index| {
            extracted_pdfium
                .pages()
                .get(index)
                .unwrap()
                .text()
                .unwrap()
                .all()
        })
        .collect();
    assert!(pages[0].contains("Page one"));
    assert_eq!(pages.len(), 2);
}

#[test]
fn password_protection_uses_aes256_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.pdf");
    test_pdf(&source).unwrap();
    let mut pdf =
        crate::pdfdoc::PdfState::open(&engine(), &std::fs::read(&source).unwrap(), None).unwrap();
    let protected = dir.path().join("protected.pdf");
    pdf.save(
        &protected,
        &SaveOptions {
            password: Some("local-secret".into()),
            force: true,
            ..SaveOptions::default()
        },
    )
    .unwrap();
    let bytes = std::fs::read(&protected).unwrap();
    let encrypted_header = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(encrypted_header.is_encrypted());
    let encryption = encrypted_header
        .get_dictionary(
            encrypted_header
                .trailer
                .get(b"Encrypt")
                .unwrap()
                .as_reference()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(encryption.get(b"V").unwrap().as_i64().unwrap(), 5);
    assert_eq!(encryption.get(b"R").unwrap().as_i64().unwrap(), 6);
    let encrypted = lopdf::Document::load_mem_with_options(
        &bytes,
        lopdf::LoadOptions::with_password("local-secret"),
    )
    .unwrap();
    assert_eq!(encrypted.get_pages().len(), 3);
    assert!(
        lopdf::Document::load_mem_with_options(
            &bytes,
            lopdf::LoadOptions::with_password("wrong-password"),
        )
        .is_err()
    );
    let mut opened = DocumentState::open(
        &engine(),
        "protected".into(),
        &protected,
        Some("local-secret".into()),
    )
    .unwrap();
    assert!(!opened.info().read_only);
    opened
        .apply(
            &engine(),
            Operation::RotatePage {
                page: 0,
                degrees: 90,
            },
        )
        .unwrap();
    let protected_extract = dir.path().join("protected-extract.pdf");
    opened
        .save(
            &protected_extract,
            &SaveOptions {
                pages: Some(vec![0, 2]),
                ..SaveOptions::default()
            },
            false,
        )
        .unwrap();
    assert!(
        lopdf::Document::load(&protected_extract)
            .unwrap()
            .is_encrypted()
    );
    assert_eq!(
        lopdf::Document::load_mem_with_options(
            &std::fs::read(&protected_extract).unwrap(),
            lopdf::LoadOptions::with_password("local-secret"),
        )
        .unwrap()
        .get_pages()
        .len(),
        2
    );
    let preserved = dir.path().join("preserved.pdf");
    opened
        .save(&preserved, &SaveOptions::default(), false)
        .unwrap();
    assert!(lopdf::Document::load(&preserved).unwrap().is_encrypted());
    let unlocked = dir.path().join("unlocked.pdf");
    opened
        .save(
            &unlocked,
            &SaveOptions {
                remove_password: true,
                ..SaveOptions::default()
            },
            false,
        )
        .unwrap();
    let unlocked_pdf = lopdf::Document::load(&unlocked).unwrap();
    assert!(!unlocked_pdf.is_encrypted());
    assert_eq!(unlocked_pdf.get_pages().len(), 3);
}

#[test]
fn inserting_pages_is_undoable_and_redoable() {
    let dir = tempfile::tempdir().unwrap();
    let (source_dir, mut document) = open_test_pdf();
    let source = dir.path().join("source.pdf");
    test_pdf(&source).unwrap();
    document
        .apply(
            &engine(),
            Operation::InsertPages {
                source_path: source.display().to_string(),
                at: 1,
            },
        )
        .unwrap();
    assert_eq!(document.info().page_count, 6);
    document.undo(&engine()).unwrap();
    assert_eq!(document.info().page_count, 3);
    document.redo(&engine()).unwrap();
    assert_eq!(document.info().page_count, 6);
    drop(source_dir);
}

#[test]
fn cross_pdf_page_transfer_is_copied_and_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.pdf");
    let destination_path = dir.path().join("destination.pdf");
    test_pdf(&source_path).unwrap();
    test_pdf(&destination_path).unwrap();
    let app = AppState::new(engine().clone());
    let source = app.open(source_path.display().to_string(), None).unwrap();
    let mut destination = app
        .open(destination_path.display().to_string(), None)
        .unwrap();

    destination = app
        .transfer_page(&source.id, &destination.id, 1, 1)
        .unwrap();
    assert_eq!(destination.page_count, 4);
    assert!(destination.can_undo);
    let matches = app.find_text(&destination.id, "Page two").unwrap();
    assert_eq!(matches.iter().filter(|m| m.page == 1).count(), 1);
    assert_eq!(matches.iter().filter(|m| m.page == 2).count(), 1);

    app.close(&source.id).unwrap();
    app.undo(&destination.id).unwrap();
    let undone = app.info(&destination.id).unwrap();
    assert_eq!(undone.page_count, 3);
    app.redo(&destination.id).unwrap();
    assert_eq!(app.info(&destination.id).unwrap().page_count, 4);
}

#[test]
fn cross_pdf_page_transfer_rejects_interactive_sources() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("form-source.pdf");
    let destination_path = dir.path().join("destination.pdf");
    test_form_pdf(&source_path).unwrap();
    test_pdf(&destination_path).unwrap();
    let app = AppState::new(engine().clone());
    let source = app.open(source_path.display().to_string(), None).unwrap();
    let destination = app
        .open(destination_path.display().to_string(), None)
        .unwrap();
    assert!(
        app.transfer_page(&source.id, &destination.id, 0, 1)
            .is_err()
    );
    assert_eq!(app.info(&destination.id).unwrap().page_count, 3);
}

#[test]
fn gif_stays_animated_while_pristine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anim.gif");
    let frame = image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 255]));
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        encoder
            .encode(&frame, 10, 10, image::ExtendedColorType::Rgba8)
            .unwrap();
    }
    let engine = engine();
    let mut document = DocumentState::open(&engine, "gif".into(), &path, None).unwrap();
    assert_eq!(document.info().mime, "image/gif");
    document
        .apply(&engine, Operation::RotateImage { degrees: 90 })
        .unwrap();
    assert_eq!(document.info().mime, "image/png");
}
