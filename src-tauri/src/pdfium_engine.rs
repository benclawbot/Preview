use std::path::{Path, PathBuf};

use image::{ExtendedColorType, ImageEncoder};
use pdfium_render::prelude::*;

use crate::error::{AppError, Result};
use crate::geometry::PageGeom;
use crate::model::{Rect, TextMatch};

/// Single owner of the pdfium library handle.
///
/// pdfium is not thread-safe. The crate's `thread_safe` feature serializes every
/// entry point, and the document service additionally routes rendering through
/// one queue, so all calls are serial.
#[derive(Clone)]
pub struct PdfEngine {
    pdfium: &'static Pdfium,
    library: PathBuf,
}

impl std::fmt::Debug for PdfEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfEngine")
            .field("library", &self.library)
            .finish()
    }
}

pub struct RenderOutput {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl PdfEngine {
    pub fn load(resource_dir: Option<&Path>) -> Result<Self> {
        let library = locate_library(resource_dir)?;
        let bindings = Pdfium::bind_to_library(&library).map_err(|error| {
            AppError::Other(format!(
                "could not load pdfium from {}: {error}. Run scripts/fetch-pdfium.sh",
                library.display()
            ))
        })?;
        let pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
        Ok(Self { pdfium, library })
    }

    pub fn load_document(
        &self,
        bytes: Vec<u8>,
        password: Option<&str>,
    ) -> Result<PdfDocument<'static>> {
        self.pdfium
            .load_pdf_from_byte_vec(bytes, password)
            .map_err(|error| match error {
                PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
                    AppError::Message("the password is incorrect or missing".into())
                }
                other => AppError::Other(format!("pdfium could not open the document: {other}")),
            })
    }

    /// Renders one page at `scale` CSS-pixel-sized points. The output size is
    /// derived from the CropBox and `/Rotate`, which matches what the overlay
    /// uses, so annotation hit-testing stays aligned at every zoom level.
    pub fn render_page(
        &self,
        document: &PdfDocument<'static>,
        index: u32,
        geom: PageGeom,
        scale: f32,
        render_annotations: bool,
    ) -> Result<RenderOutput> {
        let page = document.pages().get(index as i32).map_err(|error| {
            AppError::Other(format!("cannot render page {}: {error}", index + 1))
        })?;

        let width = ((geom.width() * scale).round() as u32).clamp(1, 16_384);
        let height = ((geom.height() * scale).round() as u32).clamp(1, 16_384);

        let config = PdfRenderConfig::new()
            .set_target_size(width as i32, height as i32)
            .render_annotations(render_annotations)
            .render_form_data(true)
            .clear_before_rendering(true);

        let bitmap = page.render_with_config(&config)?;
        let rgba = bitmap.as_rgba_bytes();
        let actual_width = bitmap.width().max(0) as u32;
        let actual_height = bitmap.height().max(0) as u32;

        let mut png = Vec::with_capacity(rgba.len() / 3);
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, actual_width, actual_height, ExtendedColorType::Rgba8)
            .map_err(|error| AppError::Other(format!("could not encode page as PNG: {error}")))?;

        Ok(RenderOutput {
            png,
            width: actual_width,
            height: actual_height,
        })
    }

    /// Finds every occurrence of `query` across all pages. PDFium's text search
    /// returns a run of text segments for each match; their union is the
    /// highlight rectangle.
    pub fn find_text(
        &self,
        document: &PdfDocument<'static>,
        geometries: &[PageGeom],
        query: &str,
    ) -> Result<Vec<TextMatch>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        let page_count = document.pages().len();
        for index in 0..page_count {
            let Ok(page) = document.pages().get(index) else {
                continue;
            };
            let Ok(text) = page.text() else {
                continue;
            };
            let options = PdfSearchOptions::new();
            let Ok(search) = text.search(query, &options) else {
                continue;
            };
            let geom = geometries.get(index as usize).copied().unwrap_or_default();

            let mut guard = 0;
            while let Some(segments) = search.find_next() {
                let mut text = String::new();
                let mut bounds: Option<PdfRect> = None;
                for segment in segments.iter() {
                    text.push_str(&segment.text());
                    let segment_bounds = segment.bounds();
                    bounds = Some(match bounds {
                        None => segment_bounds,
                        Some(current) => {
                            let left = current.left().value.min(segment_bounds.left().value);
                            let right = current.right().value.max(segment_bounds.right().value);
                            let bottom = current.bottom().value.min(segment_bounds.bottom().value);
                            let top = current.top().value.max(segment_bounds.top().value);
                            PdfRect::new_from_values(bottom, left, top, right)
                        }
                    });
                }
                if let Some(bounds) = bounds {
                    let a = geom.to_display(bounds.left().value, bounds.top().value);
                    let b = geom.to_display(bounds.right().value, bounds.bottom().value);
                    results.push(TextMatch {
                        page: index as u32,
                        rect: Rect::new(
                            a.x.min(b.x),
                            a.y.min(b.y),
                            (a.x - b.x).abs(),
                            (a.y - b.y).abs(),
                        ),
                        text,
                    });
                }
                guard += 1;
                if guard > 10_000 {
                    break;
                }
            }
        }
        Ok(results)
    }
}

fn library_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "pdfium.dll"
    } else if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    }
}

pub fn locate_library(resource_dir: Option<&Path>) -> Result<PathBuf> {
    let name = library_file_name();
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(path) = std::env::var("PDFIUM_LIB_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(dir) = resource_dir {
        candidates.push(dir.join("vendor/pdfium/lib").join(name));
        candidates.push(dir.join("lib").join(name));
        candidates.push(dir.join(name));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join(name));
            candidates.push(parent.join("lib").join(name));
            candidates.push(parent.join("..").join("Resources").join(name));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("vendor/pdfium/lib")
            .join(name),
    );
    candidates.push(PathBuf::from("/usr/lib").join(name));
    candidates.push(PathBuf::from("/usr/lib64").join(name));

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.clone()));
        }
    }

    Err(AppError::Other(format!(
        "pdfium was not found. Looked in: {}. Run scripts/fetch-pdfium.sh, or set PDFIUM_LIB_PATH.",
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}
