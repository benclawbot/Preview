mod annotations;
mod commands;
mod error;
mod export;
mod geometry;
mod image_doc;
mod model;
mod pdfdoc;
mod pdfium_engine;
mod photo_meta;
mod service;

#[cfg(test)]
mod tests;

use tauri::Manager;

use pdfium_engine::PdfEngine;
use service::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let resource_dir = app.path().resource_dir().ok();
            let engine = PdfEngine::load(resource_dir.as_deref()).map_err(|error| {
                eprintln!("Preview could not start: {error}");
                Box::<dyn std::error::Error>::from(error)
            })?;
            app.manage(AppState::new(engine));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_document,
            commands::render_page,
            commands::get_epub_data,
            commands::apply_op,
            commands::undo_document,
            commands::redo_document,
            commands::find_text,
            commands::save_document,
            commands::export_document,
            commands::get_metadata,
            commands::get_bookmarks,
            commands::insert_pdf_pages,
            commands::transfer_pdf_page,
            commands::merge_pdfs,
            commands::batch_resize_images,
            commands::get_image_histogram,
            commands::get_document_info,
            commands::revert_document,
            commands::close_document,
            commands::initial_document_path,
            commands::redact,
            commands::protect_document,
            commands::get_form_fields,
            commands::fill_form_field,
            commands::export_heic,
            commands::ocr_document,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Preview");
}
