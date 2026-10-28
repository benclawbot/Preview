use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::{AppError, Result};
use crate::image_doc::ImageState;
use crate::model::{
    ApplyResult, BookmarkInfo, DocumentInfo, DocumentKind, FormFieldInfo, Inverse, MetadataReport,
    Operation, PageInfo, SaveOptions, TextMatch,
};
use crate::pdfdoc::{self, PdfState};
use crate::pdfium_engine::PdfEngine;

const MAX_EPUB_BYTES: u64 = 256 * 1024 * 1024;

pub struct PdfHistory {
    forward: Operation,
    inverse: Inverse,
}

pub struct DocumentState {
    pub id: String,
    pub kind: DocumentKind,
    pub name: String,
    pub path: PathBuf,
    pub version: u64,
    pub dirty: bool,
    pub password: Option<String>,
    pub pdf: Option<PdfState>,
    pub image: Option<ImageState>,
    pub epub: Option<Vec<u8>>,
    pub undo: Vec<PdfHistory>,
    pub redo: Vec<PdfHistory>,
}

impl DocumentState {
    pub fn open(
        engine: &PdfEngine,
        id: String,
        path: &Path,
        password: Option<String>,
    ) -> Result<Self> {
        let is_epub = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("epub"));
        if is_epub && std::fs::metadata(path)?.len() > MAX_EPUB_BYTES {
            return Err(AppError::Message(
                "EPUB files larger than 256 MiB are not supported".into(),
            ));
        }
        let mut bytes = std::fs::read(path)?;
        if is_epub {
            if !bytes.starts_with(b"PK\x03\x04") {
                return Err(AppError::Message(
                    "this file is not a valid EPUB archive".into(),
                ));
            }
            return Ok(Self {
                id,
                kind: DocumentKind::Epub,
                name: pdfdoc::file_name(path),
                path: path.to_path_buf(),
                version: 1,
                dirty: false,
                password: None,
                pdf: None,
                image: None,
                epub: Some(bytes),
                undo: Vec::new(),
                redo: Vec::new(),
            });
        }
        let is_svg = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
            || std::str::from_utf8(&bytes).ok().is_some_and(|text| {
                text.trim_start().starts_with("<svg") || text.contains("<svg ")
            });
        if is_svg {
            bytes = rasterize_svg(&bytes)?;
        }
        let is_pdf = bytes.starts_with(b"%PDF")
            || path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
        let name = pdfdoc::file_name(path);

        if is_pdf {
            let pdf = PdfState::open(engine, &bytes, password.clone())?;
            Ok(Self {
                id,
                kind: DocumentKind::Pdf,
                name,
                path: path.to_path_buf(),
                version: 1,
                dirty: false,
                password,
                pdf: Some(pdf),
                image: None,
                epub: None,
                undo: Vec::new(),
                redo: Vec::new(),
            })
        } else {
            let image = ImageState::open(bytes, path)?;
            Ok(Self {
                id,
                kind: DocumentKind::Image,
                name,
                path: path.to_path_buf(),
                version: 1,
                dirty: false,
                password: None,
                pdf: None,
                image: Some(image),
                epub: None,
                undo: Vec::new(),
                redo: Vec::new(),
            })
        }
    }

    pub fn reload(&mut self, engine: &PdfEngine) -> Result<()> {
        let password = self.password.clone();
        let mut fresh = Self::open(engine, self.id.clone(), &self.path, password)?;
        fresh.version = self.version + 1;
        fresh.dirty = false;
        *self = fresh;
        Ok(())
    }

    #[cfg(test)]
    pub fn page_count(&self) -> u32 {
        match self.kind {
            DocumentKind::Pdf => self.pdf.as_ref().map(PdfState::page_count).unwrap_or(0),
            DocumentKind::Image => 1,
            DocumentKind::Epub => 1,
        }
    }

    pub fn render(&self, engine: &PdfEngine, page: u32, dpi: f32) -> Result<(Vec<u8>, String)> {
        match self.kind {
            DocumentKind::Pdf => {
                let pdf = self.pdf.as_ref().expect("pdf state");
                let (bytes, _, _) = pdf.render(engine, page, dpi)?;
                Ok((bytes, "image/png".into()))
            }
            DocumentKind::Image => {
                if page != 0 {
                    return Err(AppError::Other("image has one page".into()));
                }
                let image = self.image.as_ref().expect("image state");
                let (bytes, mime) = image.view_bytes()?;
                Ok((bytes, mime.into()))
            }
            DocumentKind::Epub => Err(AppError::Unsupported(
                "EPUB books are rendered by the reading view".into(),
            )),
        }
    }

    pub fn find_text(&self, engine: &PdfEngine, query: &str) -> Result<Vec<TextMatch>> {
        match self.kind {
            DocumentKind::Pdf => self
                .pdf
                .as_ref()
                .expect("pdf state")
                .find_text(engine, query),
            DocumentKind::Image => Ok(Vec::new()),
            DocumentKind::Epub => Ok(Vec::new()),
        }
    }

    pub fn apply(&mut self, engine: &PdfEngine, operation: Operation) -> Result<ApplyResult> {
        let changed_pages = match self.kind {
            DocumentKind::Pdf => {
                let pdf = self.pdf.as_mut().expect("pdf state");
                let (changed, inverse) = match &operation {
                    Operation::AddAnnotation { .. }
                    | Operation::EditAnnotation { .. }
                    | Operation::RemoveAnnotation { .. } => pdf.apply_annotation(&operation)?,
                    Operation::RotatePage { .. }
                    | Operation::DeletePage { .. }
                    | Operation::ReorderPages { .. }
                    | Operation::InsertPages { .. }
                    | Operation::TransferPage { .. }
                    | Operation::Redact { .. }
                    | Operation::FillFormFields { .. }
                    | Operation::OcrDocument => pdf.apply_structural(engine, &operation)?,
                    _ => {
                        return Err(AppError::Message(
                            "this operation is not valid for a PDF".into(),
                        ));
                    }
                };
                self.undo.push(PdfHistory {
                    forward: operation,
                    inverse,
                });
                self.redo.clear();
                changed
            }
            DocumentKind::Image => {
                self.image.as_mut().expect("image state").apply(operation)?;
                vec![0]
            }
            DocumentKind::Epub => {
                return Err(AppError::Unsupported("EPUB books are read-only".into()));
            }
        };

        self.version += 1;
        self.dirty = true;
        Ok(self.apply_result(changed_pages))
    }

    pub fn undo(&mut self, engine: &PdfEngine) -> Result<ApplyResult> {
        let changed_pages = match self.kind {
            DocumentKind::Pdf => {
                let Some(history) = self.undo.pop() else {
                    return Err(AppError::Message("nothing to undo".into()));
                };
                let changed = self
                    .pdf
                    .as_mut()
                    .expect("pdf state")
                    .apply_inverse(engine, &history.inverse)?;
                self.redo.push(history);
                changed
            }
            DocumentKind::Image => {
                if !self.image.as_mut().expect("image state").undo() {
                    return Err(AppError::Message("nothing to undo".into()));
                }
                vec![0]
            }
            DocumentKind::Epub => {
                return Err(AppError::Unsupported("EPUB books are read-only".into()));
            }
        };
        self.version += 1;
        self.dirty = true;
        Ok(self.apply_result(changed_pages))
    }

    pub fn redo(&mut self, engine: &PdfEngine) -> Result<ApplyResult> {
        let changed_pages = match self.kind {
            DocumentKind::Pdf => {
                let Some(history) = self.redo.pop() else {
                    return Err(AppError::Message("nothing to redo".into()));
                };
                let pdf = self.pdf.as_mut().expect("pdf state");
                let (changed, inverse) = match &history.forward {
                    Operation::AddAnnotation { .. }
                    | Operation::EditAnnotation { .. }
                    | Operation::RemoveAnnotation { .. } => {
                        pdf.apply_annotation(&history.forward)?
                    }
                    Operation::RotatePage { .. }
                    | Operation::DeletePage { .. }
                    | Operation::ReorderPages { .. }
                    | Operation::InsertPages { .. }
                    | Operation::TransferPage { .. }
                    | Operation::Redact { .. }
                    | Operation::FillFormFields { .. }
                    | Operation::OcrDocument => pdf.apply_structural(engine, &history.forward)?,
                    _ => {
                        return Err(AppError::Message(
                            "this operation is not valid for a PDF".into(),
                        ));
                    }
                };
                self.undo.push(PdfHistory {
                    forward: history.forward,
                    inverse,
                });
                changed
            }
            DocumentKind::Image => {
                if !self.image.as_mut().expect("image state").redo() {
                    return Err(AppError::Message("nothing to redo".into()));
                }
                vec![0]
            }
            DocumentKind::Epub => {
                return Err(AppError::Unsupported("EPUB books are read-only".into()));
            }
        };
        self.version += 1;
        self.dirty = true;
        Ok(self.apply_result(changed_pages))
    }

    pub fn save(&mut self, path: &Path, options: &SaveOptions, keep_path: bool) -> Result<PathBuf> {
        match self.kind {
            DocumentKind::Pdf => {
                let format = pdfdoc::export_path_format(path, options);
                if format != "pdf" {
                    return Err(AppError::Message(
                        "raster export for PDFs is handled by the document service".into(),
                    ));
                }
                let old_password = self.password.clone();
                let new_password = {
                    let pdf = self.pdf.as_mut().expect("pdf state");
                    pdf.save(path, options)?;
                    if !keep_path {
                        pdf.password = old_password.clone();
                    }
                    if keep_path {
                        self.path = path.to_path_buf();
                        self.name = pdfdoc::file_name(path);
                    }
                    pdf.password.clone()
                };
                if keep_path {
                    self.password = new_password;
                }
            }
            DocumentKind::Image => {
                self.image
                    .as_mut()
                    .expect("image state")
                    .export(path, options)?;
                if keep_path {
                    self.path = path.to_path_buf();
                    self.name = pdfdoc::file_name(path);
                }
            }
            DocumentKind::Epub => {
                return Err(AppError::Unsupported(
                    "saving changes to EPUB books is not supported".into(),
                ));
            }
        }
        if keep_path {
            self.dirty = false;
        }
        Ok(path.to_path_buf())
    }

    pub fn metadata(&self) -> MetadataReport {
        match self.kind {
            DocumentKind::Pdf => self.pdf.as_ref().expect("pdf state").metadata(),
            DocumentKind::Image => self.image.as_ref().expect("image state").metadata(),
            DocumentKind::Epub => MetadataReport {
                entries: Vec::new(),
                can_strip_gps: false,
                gps_present: false,
            },
        }
    }

    pub fn info(&self) -> DocumentInfo {
        let mut info = match self.kind {
            DocumentKind::Pdf => {
                let pdf = self.pdf.as_ref().expect("pdf state");
                pdf.info(&self.id, &self.path, self.version, self.dirty)
            }
            DocumentKind::Image => {
                let image = self.image.as_ref().expect("image state");
                let image_info = image.info();
                let mime = if image.format == crate::image_doc::ImageFormatKind::Gif
                    && image.is_pristine()
                {
                    "image/gif"
                } else {
                    "image/png"
                };
                DocumentInfo {
                    id: self.id.clone(),
                    kind: DocumentKind::Image,
                    name: self.name.clone(),
                    path: Some(self.path.display().to_string()),
                    page_count: 1,
                    version: self.version,
                    dirty: self.dirty,
                    can_undo: false,
                    can_redo: false,
                    encrypted: false,
                    read_only: false,
                    signed: false,
                    pages: vec![PageInfo {
                        index: 0,
                        width: image_info.width as f32,
                        height: image_info.height as f32,
                        rotation: 0,
                        crop_x: 0.0,
                        crop_y: 0.0,
                        raw_width: image_info.width as f32,
                        raw_height: image_info.height as f32,
                    }],
                    annotations: Vec::new(),
                    image: Some(image_info),
                    mime: mime.into(),
                }
            }
            DocumentKind::Epub => DocumentInfo {
                id: self.id.clone(),
                kind: DocumentKind::Epub,
                name: self.name.clone(),
                path: Some(self.path.display().to_string()),
                page_count: 1,
                version: self.version,
                dirty: false,
                can_undo: false,
                can_redo: false,
                encrypted: false,
                read_only: true,
                signed: false,
                pages: Vec::new(),
                annotations: Vec::new(),
                image: None,
                mime: "application/epub+zip".into(),
            },
        };
        info.can_undo = match self.kind {
            DocumentKind::Pdf => !self.undo.is_empty(),
            DocumentKind::Image => self
                .image
                .as_ref()
                .map(|image| !image.ops.is_empty())
                .unwrap_or(false),
            DocumentKind::Epub => false,
        };
        info.can_redo = match self.kind {
            DocumentKind::Pdf => !self.redo.is_empty(),
            DocumentKind::Image => self
                .image
                .as_ref()
                .map(|image| !image.redo.is_empty())
                .unwrap_or(false),
            DocumentKind::Epub => false,
        };
        info
    }

    fn apply_result(&self, changed_pages: Vec<u32>) -> ApplyResult {
        let info = self.info();
        ApplyResult {
            version: info.version,
            changed_pages,
            page_count: info.page_count,
            dirty: info.dirty,
            can_undo: info.can_undo,
            can_redo: info.can_redo,
        }
    }
}

fn rasterize_svg(bytes: &[u8]) -> Result<Vec<u8>> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|error| AppError::Message(format!("could not read SVG: {error}")))?;
    let size = tree.size();
    let width = size.width().ceil().clamp(1.0, 20000.0) as u32;
    let height = size.height().ceil().clamp(1.0, 20000.0) as u32;
    if width as u64 * height as u64 > 150_000_000 {
        return Err(AppError::Message(
            "SVG rendered size exceeds 150 megapixels".into(),
        ));
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| AppError::Message("could not allocate SVG render surface".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap
        .encode_png()
        .map_err(|error| AppError::Message(format!("could not encode SVG preview: {error}")))
}

fn pdf_has_interactive_data(doc: &lopdf::Document) -> bool {
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
            lopdf::Object::Array(items) => !items.is_empty(),
            lopdf::Object::Reference(id) => doc
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_array().ok())
                .is_some_and(|items| !items.is_empty()),
            _ => true,
        }
    })
}

// Keeps the document service independent of Tauri command plumbing.
pub struct AppState {
    pub engine: Arc<PdfEngine>,
    pub docs: Arc<Mutex<HashMap<String, DocumentState>>>,
    pub render_gate: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new(engine: PdfEngine) -> Self {
        Self {
            engine: Arc::new(engine),
            docs: Arc::new(Mutex::new(HashMap::new())),
            render_gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn documents(&self) -> Result<MutexGuard<'_, HashMap<String, DocumentState>>> {
        self.docs
            .lock()
            .map_err(|_| AppError::Other("document state is poisoned".into()))
    }

    pub fn open(&self, path: String, password: Option<String>) -> Result<DocumentInfo> {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(AppError::Message(format!(
                "file not found: {}",
                path.display()
            )));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let document = DocumentState::open(&self.engine, id.clone(), &path, password)?;
        let info = document.info();
        self.documents()?.insert(id, document);
        Ok(info)
    }

    pub fn render(&self, id: &str, page: u32, dpi: f32) -> Result<(Vec<u8>, String)> {
        let _gate = self
            .render_gate
            .lock()
            .map_err(|_| AppError::Other("render queue is poisoned".into()))?;
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        document.render(&self.engine, page, dpi)
    }

    pub fn epub_data(&self, id: &str) -> Result<Vec<u8>> {
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        document
            .epub
            .as_ref()
            .map(Clone::clone)
            .ok_or_else(|| AppError::Message("document is not an EPUB book".into()))
    }

    pub fn apply(&self, id: &str, operation: Operation) -> Result<ApplyResult> {
        let mut docs = self.documents()?;
        let document = docs.get_mut(id).ok_or(AppError::DocumentNotFound)?;
        document.apply(&self.engine, operation)
    }

    pub fn undo(&self, id: &str) -> Result<ApplyResult> {
        let mut docs = self.documents()?;
        let document = docs.get_mut(id).ok_or(AppError::DocumentNotFound)?;
        document.undo(&self.engine)
    }

    pub fn redo(&self, id: &str) -> Result<ApplyResult> {
        let mut docs = self.documents()?;
        let document = docs.get_mut(id).ok_or(AppError::DocumentNotFound)?;
        document.redo(&self.engine)
    }

    pub fn find_text(&self, id: &str, query: &str) -> Result<Vec<TextMatch>> {
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        document.find_text(&self.engine, query)
    }

    pub fn save(
        &self,
        id: &str,
        path: String,
        options: SaveOptions,
        keep_path: bool,
    ) -> Result<String> {
        let mut docs = self.documents()?;
        let document = docs.get_mut(id).ok_or(AppError::DocumentNotFound)?;
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
                return Err(AppError::Message(format!(
                    "folder does not exist: {}",
                    parent.display()
                )));
            }
        }
        let format = pdfdoc::export_path_format(&path, &options);
        if document.kind == DocumentKind::Pdf && format != "pdf" {
            // Raster export needs the engine; do it here rather than in state.
            let pdf = document.pdf.as_ref().expect("pdf state");
            pdfdoc::raster_export(pdf, &self.engine, &path, &options)?;
            return Ok(path.display().to_string());
        }
        let written = document.save(&path, &options, keep_path)?;
        Ok(written.display().to_string())
    }

    pub fn metadata(&self, id: &str) -> Result<MetadataReport> {
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        Ok(document.metadata())
    }

    pub fn bookmarks(&self, id: &str) -> Result<Vec<BookmarkInfo>> {
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        Ok(document
            .pdf
            .as_ref()
            .map(PdfState::bookmarks)
            .unwrap_or_default())
    }

    pub fn form_fields(&self, id: &str) -> Result<Vec<FormFieldInfo>> {
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        Ok(document
            .pdf
            .as_ref()
            .map(PdfState::form_fields)
            .unwrap_or_default())
    }

    pub fn fill_form_field(&self, id: &str, name: &str, value: &str) -> Result<DocumentInfo> {
        self.apply(
            id,
            Operation::FillFormFields {
                fields: vec![crate::model::FormFieldUpdate {
                    field: name.to_string(),
                    value: value.to_string(),
                }],
            },
        )?;
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        Ok(document.info())
    }

    pub fn insert_pages(&self, id: &str, source_path: String, at: u32) -> Result<DocumentInfo> {
        let mut docs = self.documents()?;
        let document = docs.get_mut(id).ok_or(AppError::DocumentNotFound)?;
        if document.kind != DocumentKind::Pdf {
            return Err(AppError::Message(
                "page insertion is only available for PDFs".into(),
            ));
        }
        let operation = Operation::InsertPages { source_path, at };
        let (changed, inverse) = document
            .pdf
            .as_mut()
            .expect("pdf state")
            .apply_structural(&self.engine, &operation)?;
        document.undo.push(PdfHistory {
            forward: operation,
            inverse,
        });
        document.redo.clear();
        document.version += 1;
        document.dirty = true;
        let _ = changed;
        Ok(document.info())
    }

    pub fn transfer_page(
        &self,
        source_id: &str,
        destination_id: &str,
        source_page: u32,
        at: u32,
    ) -> Result<DocumentInfo> {
        if source_id == destination_id {
            return Err(AppError::Message(
                "dragging within one PDF reorders pages; cross-document transfer needs two different PDFs".into(),
            ));
        }
        let mut docs = self.documents()?;
        let source = docs.get(source_id).ok_or(AppError::DocumentNotFound)?;
        let source_pdf = source
            .pdf
            .as_ref()
            .ok_or_else(|| AppError::Message("source document is not a PDF".into()))?;
        let source_pdf_bytes = source_pdf.transfer_snapshot(source_page)?;

        let destination = docs
            .get_mut(destination_id)
            .ok_or(AppError::DocumentNotFound)?;
        let operation = Operation::TransferPage {
            source_id: source_id.to_string(),
            source_page,
            at,
            source_pdf_bytes: Some(source_pdf_bytes),
        };
        destination.apply(&self.engine, operation)?;
        Ok(destination.info())
    }

    pub fn merge_pdfs(&self, paths: Vec<String>, output_path: String) -> Result<String> {
        if paths.len() < 2 {
            return Err(AppError::Message(
                "select at least two PDFs to merge".into(),
            ));
        }
        let output = PathBuf::from(&output_path);
        let parent = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(AppError::Message(format!(
                "output folder does not exist: {}",
                parent.display()
            )));
        }
        let output_cmp = output.canonicalize().ok();
        let mut canonical_inputs = std::collections::HashSet::new();
        let mut page_total = 0usize;
        let mut decoded = Vec::with_capacity(paths.len());
        for path in &paths {
            let path = PathBuf::from(path);
            if !path.is_file() {
                return Err(AppError::Message(format!(
                    "PDF not found: {}",
                    path.display()
                )));
            }
            let canonical = path.canonicalize()?;
            if output_cmp
                .as_ref()
                .is_some_and(|output| output == &canonical)
                || !canonical_inputs.insert(canonical)
            {
                return Err(AppError::Message(
                    "merge input and output paths must be distinct".into(),
                ));
            }
            let bytes = std::fs::read(&path)?;
            let parsed = lopdf::Document::load_mem(&bytes).map_err(|error| {
                AppError::Message(format!("could not read {}: {error}", path.display()))
            })?;
            if parsed.is_encrypted() {
                return Err(AppError::Message(format!(
                    "encrypted PDF is not supported in merge: {}",
                    path.display()
                )));
            }
            if crate::annotations::has_signatures(&parsed) || pdf_has_interactive_data(&parsed) {
                return Err(AppError::Unsupported(format!(
                    "merge is disabled for PDFs with signatures, annotations or form fields to avoid dropping interactive data: {}",
                    path.display()
                )));
            }
            let count = parsed.get_pages().len();
            if count == 0 {
                return Err(AppError::Message(format!(
                    "PDF has no pages: {}",
                    path.display()
                )));
            }
            page_total = page_total.saturating_add(count);
            decoded.push(bytes);
        }

        let mut merged = self.engine.load_document(decoded.remove(0), None)?;
        for bytes in decoded {
            let source = self.engine.load_document(bytes, None)?;
            let page_count = source.pages().len();
            let range = format!("1-{page_count}");
            let insertion = merged.pages().len() as i32;
            merged
                .pages_mut()
                .copy_pages_from_document(&source, &range, insertion)
                .map_err(|error| {
                    AppError::Message(format!("could not merge PDF pages: {error}"))
                })?;
        }
        if merged.pages().len() as usize != page_total {
            return Err(AppError::Message(
                "merge did not produce the expected number of pages".into(),
            ));
        }
        let bytes = merged.save_to_bytes()?;
        let verify = lopdf::Document::load_mem(&bytes).map_err(|error| {
            AppError::Message(format!("merged PDF verification failed: {error}"))
        })?;
        if verify.get_pages().len() != page_total {
            return Err(AppError::Message(
                "merged PDF failed its page-count verification".into(),
            ));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut temporary, &bytes)?;
        temporary.persist(&output).map_err(|error| {
            AppError::Other(format!("could not write merged PDF: {}", error.error))
        })?;
        Ok(output.display().to_string())
    }

    pub fn batch_resize_images(
        &self,
        paths: Vec<String>,
        output_dir: String,
        width: u32,
        height: u32,
    ) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Err(AppError::Message("select at least one image".into()));
        }
        if width == 0
            || height == 0
            || width > 20000
            || height > 20000
            || width as u64 * height as u64 > 150_000_000
        {
            return Err(AppError::Message(
                "resize dimensions must be between 1 and 20,000 pixels and below 150 megapixels"
                    .into(),
            ));
        }
        let output_dir = PathBuf::from(output_dir);
        if !output_dir.is_dir() {
            return Err(AppError::Message("choose an existing output folder".into()));
        }
        let mut sources = Vec::with_capacity(paths.len());
        let mut destinations = Vec::with_capacity(paths.len());
        let mut unique = std::collections::HashSet::new();
        for path in paths {
            let source = PathBuf::from(path);
            if !source.is_file() {
                return Err(AppError::Message(format!(
                    "image not found: {}",
                    source.display()
                )));
            }
            let name = source
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("image");
            let destination = output_dir.join(format!("{name}-{width}x{height}.png"));
            if destination.exists() || !unique.insert(destination.clone()) {
                return Err(AppError::Message(format!(
                    "batch output already exists or is duplicated: {}",
                    destination.display()
                )));
            }
            sources.push(source);
            destinations.push(destination);
        }

        let staging = tempfile::TempDir::new_in(&output_dir)?;
        let mut staged = Vec::with_capacity(sources.len());
        for (index, source) in sources.iter().enumerate() {
            let bytes = std::fs::read(source)?;
            let mut image = ImageState::open(bytes, source)?;
            image.apply(Operation::ResizeImage { width, height })?;
            let temp = staging.path().join(format!("{index}.png"));
            image.export(
                &temp,
                &SaveOptions {
                    format: Some("png".into()),
                    ..SaveOptions::default()
                },
            )?;
            staged.push(temp);
        }

        let mut committed = Vec::new();
        for (temp, destination) in staged.iter().zip(&destinations) {
            if let Err(error) = std::fs::rename(temp, destination) {
                for path in &committed {
                    let _ = std::fs::remove_file(path);
                }
                return Err(AppError::Message(format!(
                    "batch export stopped before completing: {error}"
                )));
            }
            committed.push(destination.clone());
        }
        Ok(destinations
            .into_iter()
            .map(|path| path.display().to_string())
            .collect())
    }

    pub fn image_histogram(&self, id: &str) -> Result<Vec<u64>> {
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        let image = document
            .image
            .as_ref()
            .ok_or_else(|| AppError::Message("histogram is only available for images".into()))?;
        let mut bins = vec![0u64; 256];
        for pixel in image.current.pixels() {
            let luma =
                (0.2126 * pixel[0] as f32 + 0.7152 * pixel[1] as f32 + 0.0722 * pixel[2] as f32)
                    .round() as usize;
            bins[luma.min(255)] += 1;
        }
        Ok(bins)
    }

    pub fn info(&self, id: &str) -> Result<DocumentInfo> {
        let docs = self.documents()?;
        let document = docs.get(id).ok_or(AppError::DocumentNotFound)?;
        Ok(document.info())
    }

    pub fn revert(&self, id: &str) -> Result<DocumentInfo> {
        let mut docs = self.documents()?;
        let document = docs.get_mut(id).ok_or(AppError::DocumentNotFound)?;
        document.reload(&self.engine)?;
        Ok(document.info())
    }

    pub fn close(&self, id: &str) -> Result<()> {
        let mut docs = self.documents()?;
        docs.remove(id);
        Ok(())
    }
}
