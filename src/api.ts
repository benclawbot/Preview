import { invoke } from "@tauri-apps/api/core";

import type {
  Annotation,
  ApplyResult,
  BookmarkInfo,
  DocumentInfo,
  FormFieldInfo,
  MetadataReport,
  Operation,
  SaveOptions,
  TextMatch,
} from "./types";

export const api = {
  openDocument(path: string, password?: string | null): Promise<DocumentInfo> {
    return invoke("open_document", { path, password: password ?? null });
  },

  renderPage(id: string, page: number, dpi: number): Promise<ArrayBuffer> {
    return invoke<ArrayBuffer>("render_page", { id, page, dpi });
  },

  getEpubData(id: string): Promise<ArrayBuffer> {
    return invoke<ArrayBuffer>("get_epub_data", { id });
  },

  applyOp(id: string, op: Operation): Promise<ApplyResult> {
    return invoke("apply_op", { id, op });
  },

  undo(id: string): Promise<ApplyResult> {
    return invoke("undo_document", { id });
  },

  redo(id: string): Promise<ApplyResult> {
    return invoke("redo_document", { id });
  },

  findText(id: string, query: string): Promise<TextMatch[]> {
    return invoke("find_text", { id, query });
  },

  saveDocument(id: string, path: string, options: SaveOptions): Promise<string> {
    return invoke("save_document", { id, path, options });
  },

  exportDocument(id: string, path: string, options: SaveOptions): Promise<string> {
    return invoke("export_document", { id, path, options });
  },

  getMetadata(id: string): Promise<MetadataReport> {
    return invoke("get_metadata", { id });
  },

  getBookmarks(id: string): Promise<BookmarkInfo[]> {
    return invoke("get_bookmarks", { id });
  },

  insertPages(id: string, sourcePath: string, at: number): Promise<DocumentInfo> {
    return invoke("insert_pdf_pages", { id, sourcePath, at });
  },

  transferPdfPage(sourceId: string, destinationId: string, sourcePage: number, at: number): Promise<DocumentInfo> {
    return invoke("transfer_pdf_page", { sourceId, destinationId, sourcePage, at });
  },

  mergePdfs(paths: string[], outputPath: string): Promise<string> {
    return invoke("merge_pdfs", { paths, outputPath });
  },

  batchResizeImages(paths: string[], outputDir: string, width: number, height: number): Promise<string[]> {
    return invoke("batch_resize_images", { paths, outputDir, width, height });
  },

  imageHistogram(id: string): Promise<number[]> {
    return invoke("get_image_histogram", { id });
  },

  getFormFields(id: string): Promise<FormFieldInfo[]> {
    return invoke("get_form_fields", { id });
  },

  ocrDocument(id: string): Promise<ApplyResult> {
    return invoke("ocr_document", { id });
  },

  fillFormField(id: string, field: string, value: string): Promise<DocumentInfo> {
    return invoke("fill_form_field", { id, field, value });
  },

  getDocumentInfo(id: string): Promise<DocumentInfo> {
    return invoke("get_document_info", { id });
  },

  revertDocument(id: string): Promise<DocumentInfo> {
    return invoke("revert_document", { id });
  },

  closeDocument(id: string): Promise<void> {
    return invoke("close_document", { id });
  },

  initialDocumentPath(): Promise<string | null> {
    return invoke("initial_document_path");
  },
};

export async function addAnnotation(id: string, annotation: Annotation): Promise<ApplyResult> {
  return api.applyOp(id, {
    op: "add_annotation",
    page: annotation.page,
    annotation,
  });
}

export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}
