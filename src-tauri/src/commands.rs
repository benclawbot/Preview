use tauri::State;

use crate::error::Result;
use crate::model::{
    ApplyResult, BookmarkInfo, DocumentInfo, FormFieldInfo, MetadataReport, Operation, Rect,
    SaveOptions, TextMatch,
};
use crate::service::AppState;

#[tauri::command]
pub fn open_document(
    state: State<'_, AppState>,
    path: String,
    password: Option<String>,
) -> Result<DocumentInfo> {
    state.open(path, password)
}

/// Returns raw image bytes. The MIME type is in `DocumentInfo.mime`.
#[tauri::command]
pub fn render_page(
    state: State<'_, AppState>,
    id: String,
    page: u32,
    dpi: f32,
) -> Result<tauri::ipc::Response> {
    let (bytes, _mime) = state.render(&id, page, dpi)?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub fn get_epub_data(state: State<'_, AppState>, id: String) -> Result<tauri::ipc::Response> {
    Ok(tauri::ipc::Response::new(state.epub_data(&id)?))
}

#[tauri::command]
pub fn apply_op(state: State<'_, AppState>, id: String, op: Operation) -> Result<ApplyResult> {
    state.apply(&id, op)
}

#[tauri::command]
pub fn undo_document(state: State<'_, AppState>, id: String) -> Result<ApplyResult> {
    state.undo(&id)
}

#[tauri::command]
pub fn redo_document(state: State<'_, AppState>, id: String) -> Result<ApplyResult> {
    state.redo(&id)
}

#[tauri::command]
pub fn find_text(state: State<'_, AppState>, id: String, query: String) -> Result<Vec<TextMatch>> {
    state.find_text(&id, &query)
}

#[tauri::command]
pub fn save_document(
    state: State<'_, AppState>,
    id: String,
    path: String,
    options: Option<SaveOptions>,
) -> Result<String> {
    state.save(&id, path, options.unwrap_or_default(), true)
}

#[tauri::command]
pub fn export_document(
    state: State<'_, AppState>,
    id: String,
    path: String,
    options: Option<SaveOptions>,
) -> Result<String> {
    state.save(&id, path, options.unwrap_or_default(), false)
}

#[tauri::command]
pub fn get_metadata(state: State<'_, AppState>, id: String) -> Result<MetadataReport> {
    state.metadata(&id)
}

#[tauri::command]
pub fn get_bookmarks(state: State<'_, AppState>, id: String) -> Result<Vec<BookmarkInfo>> {
    state.bookmarks(&id)
}

#[tauri::command]
pub fn insert_pdf_pages(
    state: State<'_, AppState>,
    id: String,
    source_path: String,
    at: u32,
) -> Result<DocumentInfo> {
    state.insert_pages(&id, source_path, at)
}

#[tauri::command]
pub fn transfer_pdf_page(
    state: State<'_, AppState>,
    source_id: String,
    destination_id: String,
    source_page: u32,
    at: u32,
) -> Result<DocumentInfo> {
    state.transfer_page(&source_id, &destination_id, source_page, at)
}

#[tauri::command]
pub fn merge_pdfs(
    state: State<'_, AppState>,
    paths: Vec<String>,
    output_path: String,
) -> Result<String> {
    state.merge_pdfs(paths, output_path)
}

#[tauri::command]
pub fn batch_resize_images(
    state: State<'_, AppState>,
    paths: Vec<String>,
    output_dir: String,
    width: u32,
    height: u32,
) -> Result<Vec<String>> {
    state.batch_resize_images(paths, output_dir, width, height)
}

#[tauri::command]
pub fn get_image_histogram(state: State<'_, AppState>, id: String) -> Result<Vec<u64>> {
    state.image_histogram(&id)
}

#[tauri::command]
pub fn get_document_info(state: State<'_, AppState>, id: String) -> Result<DocumentInfo> {
    state.info(&id)
}

#[tauri::command]
pub fn revert_document(state: State<'_, AppState>, id: String) -> Result<DocumentInfo> {
    state.revert(&id)
}

#[tauri::command]
pub fn close_document(state: State<'_, AppState>, id: String) -> Result<()> {
    state.close(&id)
}

/// The first file passed on the command line, used when a desktop environment
/// opens a PDF, EPUB, or image with Preview. Tauri's single-instance plugin is not in
/// the MVP, so a second invocation starts a second window.
#[tauri::command]
pub fn initial_document_path() -> Option<String> {
    std::env::args().skip(1).find_map(|arg| {
        let path = std::path::Path::new(&arg);
        (path.is_file() && !arg.starts_with('-')).then(|| path.display().to_string())
    })
}

// ---------------------------------------------------------------------------
// v1 operation endpoints.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn redact(
    state: State<'_, AppState>,
    id: String,
    page: u32,
    rect: Rect,
) -> Result<ApplyResult> {
    state.apply(&id, Operation::Redact { page, rect })
}

#[tauri::command]
pub fn protect_document(
    state: State<'_, AppState>,
    id: String,
    path: String,
    password: String,
) -> Result<String> {
    state.save(
        &id,
        path,
        SaveOptions {
            password: Some(password),
            ..SaveOptions::default()
        },
        false,
    )
}

#[tauri::command]
pub fn get_form_fields(state: State<'_, AppState>, id: String) -> Result<Vec<FormFieldInfo>> {
    state.form_fields(&id)
}

#[tauri::command]
pub fn fill_form_field(
    state: State<'_, AppState>,
    id: String,
    field: String,
    value: String,
) -> Result<DocumentInfo> {
    state.fill_form_field(&id, &field, &value)
}

#[tauri::command]
pub fn export_heic(state: State<'_, AppState>, id: String, path: String) -> Result<String> {
    state.save(
        &id,
        path,
        SaveOptions {
            format: Some("heic".into()),
            ..SaveOptions::default()
        },
        false,
    )
}

#[tauri::command]
pub fn ocr_document(state: State<'_, AppState>, id: String) -> Result<ApplyResult> {
    state.apply(&id, Operation::OcrDocument)
}
