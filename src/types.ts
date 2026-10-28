export type DocumentKind = "pdf" | "image" | "epub";

export type AnnotationKind =
  | "highlight"
  | "underline"
  | "strikeout"
  | "square"
  | "circle"
  | "line"
  | "arrow"
  | "free_text"
  | "note"
  | "ink"
  | "signature";

export type Tool =
  | "select"
  | "highlight"
  | "underline"
  | "strikeout"
  | "square"
  | "circle"
  | "line"
  | "arrow"
  | "free_text"
  | "note"
  | "ink"
  | "redact"
  | "crop";

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

export interface Annotation {
  id: string;
  page: number;
  kind: AnnotationKind;
  rect: Rect;
  points: Point[];
  image_data: string | null;
  color: string;
  fill: string | null;
  width: number;
  opacity: number;
  contents: string | null;
  font_size: number;
  bold: boolean;
  italic: boolean;
  existing: boolean;
  dirty: boolean;
}

export interface PageInfo {
  index: number;
  width: number;
  height: number;
  rotation: number;
  crop_x: number;
  crop_y: number;
  raw_width: number;
  raw_height: number;
}

export interface ImageInfo {
  width: number;
  height: number;
  format: string;
  animated: boolean;
  has_alpha: boolean;
  orientation: number;
}

export interface DocumentInfo {
  id: string;
  kind: DocumentKind;
  name: string;
  path: string | null;
  page_count: number;
  version: number;
  dirty: boolean;
  can_undo: boolean;
  can_redo: boolean;
  encrypted: boolean;
  read_only: boolean;
  signed: boolean;
  pages: PageInfo[];
  annotations: Annotation[];
  image: ImageInfo | null;
  mime: string;
}

export interface ApplyResult {
  version: number;
  changed_pages: number[];
  page_count: number;
  dirty: boolean;
  can_undo: boolean;
  can_redo: boolean;
}

export interface TextMatch {
  page: number;
  rect: Rect;
  text: string;
}

export interface MetadataEntry {
  group: string;
  key: string;
  value: string;
}

export interface MetadataReport {
  entries: MetadataEntry[];
  can_strip_gps: boolean;
  gps_present: boolean;
}

export interface BookmarkInfo {
  title: string;
  page: number;
  depth: number;
}

export interface FormFieldInfo {
  name: string;
  kind: string;
  value: string;
  options: string[];
  checked: boolean;
}

export interface SaveOptions {
  format?: string;
  quality?: number;
  password?: string;
  remove_password?: boolean;
  strip_gps?: boolean;
  page?: number;
  dpi?: number;
  force?: boolean;
  pages?: number[];
}

export type Operation =
  | { op: "rotate_page"; page: number; degrees: number }
  | { op: "delete_page"; page: number }
  | { op: "reorder_pages"; from: number; to: number }
   | { op: "insert_pages"; source_path: string; at: number }
   | { op: "transfer_page"; source_id: string; source_page: number; at: number }
  | { op: "add_annotation"; page: number; annotation: Annotation }
  | { op: "edit_annotation"; id: string; before?: Annotation | null; after: Annotation }
   | { op: "remove_annotation"; id: string }
   | { op: "fill_form_fields"; fields: { field: string; value: string }[] }
   | { op: "ocr_document" }
  | { op: "crop_image"; x: number; y: number; width: number; height: number }
  | { op: "resize_image"; width: number; height: number }
  | { op: "rotate_image"; degrees: number }
  | { op: "flip_image"; horizontal: boolean }
   | { op: "adjust_image"; brightness: number; contrast: number; saturation: number; sharpen: number }
   | { op: "image_levels"; black: number; white: number; gamma: number }
   | { op: "image_curves"; points: Point[] }
   | { op: "redact"; page: number; rect: Rect }
   | { op: "instant_alpha"; color: string; tolerance: number };

export interface CropRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ExportChoice {
  format: string;
  quality: number;
  page: number;
  dpi: number;
  stripGps: boolean;
}
