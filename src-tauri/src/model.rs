use serde::{Deserialize, Serialize};

/// A rectangle in page space, measured in PDF points with a top-left origin.
/// This is the coordinate system the UI uses for every page, regardless of the
/// page's `/Rotate` value or CropBox offset.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn normalized(self) -> Self {
        let x = self.x.min(self.right());
        let y = self.y.min(self.bottom());
        Self {
            x,
            y,
            width: self.width.abs(),
            height: self.height.abs(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    Highlight,
    Underline,
    Strikeout,
    Square,
    Circle,
    Line,
    Arrow,
    FreeText,
    Note,
    Ink,
    Signature,
}

impl AnnotationKind {
    pub fn is_text_markup(self) -> bool {
        matches!(
            self,
            AnnotationKind::Highlight | AnnotationKind::Underline | AnnotationKind::Strikeout
        )
    }
}

/// A markup annotation stored in application space.
///
/// Annotations are real PDF annotation objects: this struct is the editing
/// model, and `annotations.rs` turns it into `/Annots` dictionaries with
/// appearance streams at save time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    pub id: String,
    pub page: u32,
    pub kind: AnnotationKind,
    pub rect: Rect,
    #[serde(default)]
    pub points: Vec<Point>,
    #[serde(default)]
    pub image_data: Option<String>,
    /// `#rrggbb`
    pub color: String,
    /// `#rrggbb`; only used by closed shapes.
    #[serde(default)]
    pub fill: Option<String>,
    #[serde(default = "default_stroke_width")]
    pub width: f32,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub contents: Option<String>,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    /// True when the annotation came from the PDF rather than this session.
    #[serde(default)]
    pub existing: bool,
    /// True when an existing annotation was edited and must be rewritten.
    #[serde(default)]
    pub dirty: bool,
}

fn default_stroke_width() -> f32 {
    2.0
}

fn default_opacity() -> f32 {
    1.0
}

fn default_font_size() -> f32 {
    12.0
}

impl Annotation {
    pub fn new(id: String, page: u32, kind: AnnotationKind, rect: Rect) -> Self {
        let color = match kind {
            AnnotationKind::Highlight => "#ffe066",
            AnnotationKind::Underline | AnnotationKind::Strikeout => "#e03131",
            AnnotationKind::Note => "#ffd43b",
            _ => "#e03131",
        };
        Self {
            id,
            page,
            kind,
            rect: rect.normalized(),
            points: Vec::new(),
            image_data: None,
            color: color.to_string(),
            fill: None,
            width: default_stroke_width(),
            opacity: if kind == AnnotationKind::Highlight {
                0.42
            } else {
                1.0
            },
            contents: None,
            font_size: default_font_size(),
            bold: false,
            italic: false,
            existing: false,
            dirty: false,
        }
    }
}

/// Every document edit is one of these. The UI sends them to `apply_op`; the
/// document service derives an inverse and keeps it on the undo stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    RotatePage {
        page: u32,
        degrees: i32,
    },
    DeletePage {
        page: u32,
    },
    ReorderPages {
        from: u32,
        to: u32,
    },
    InsertPages {
        source_path: String,
        at: u32,
    },
    TransferPage {
        source_id: String,
        source_page: u32,
        at: u32,
        /// Internal immutable snapshot retained for redo. The UI cannot set it.
        #[serde(skip)]
        source_pdf_bytes: Option<Vec<u8>>,
    },
    AddAnnotation {
        page: u32,
        annotation: Annotation,
    },
    EditAnnotation {
        id: String,
        #[serde(default)]
        before: Option<Annotation>,
        after: Annotation,
    },
    RemoveAnnotation {
        id: String,
    },
    FillFormFields {
        fields: Vec<FormFieldUpdate>,
    },
    OcrDocument,

    CropImage {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    ResizeImage {
        width: u32,
        height: u32,
    },
    RotateImage {
        degrees: i32,
    },
    FlipImage {
        horizontal: bool,
    },
    AdjustImage {
        brightness: f32,
        contrast: f32,
        saturation: f32,
        sharpen: f32,
    },
    ImageLevels {
        black: f32,
        white: f32,
        gamma: f32,
    },
    ImageCurves {
        points: Vec<Point>,
    },
    InstantAlpha {
        color: String,
        tolerance: u8,
    },

    /// v1: remove page objects and image pixels under the rectangle.
    Redact {
        page: u32,
        rect: Rect,
    },
}

/// The inverse of an applied operation. Some inverses carry data that the
/// forward operation does not have (a deleted page object, an annotation's
/// previous properties), so they are not themselves user-facing operations.
#[derive(Debug, Clone)]
pub enum Inverse {
    RotatePage {
        page: u32,
        degrees: i32,
    },
    InsertPage {
        page: u32,
        id: lopdf::ObjectId,
        object: lopdf::Object,
        parent: Option<lopdf::ObjectId>,
        index: usize,
        annotations: Vec<Annotation>,
        annotation_objects: Vec<(String, lopdf::ObjectId)>,
    },
    ReorderPages {
        from: u32,
        to: u32,
    },
    RestorePdf {
        bytes: Vec<u8>,
        annotations: Vec<Annotation>,
        annotation_objects: Vec<(String, lopdf::ObjectId)>,
    },
    AddAnnotation(Annotation),
    RemoveAnnotation(String),
    EditAnnotation {
        id: String,
        before: Annotation,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Pdf,
    Image,
    Epub,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageInfo {
    pub index: u32,
    pub width: f32,
    pub height: f32,
    pub rotation: i32,
    /// CropBox origin in unrotated PDF user space.
    pub crop_x: f32,
    pub crop_y: f32,
    /// Unrotated CropBox dimensions.
    pub raw_width: f32,
    pub raw_height: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub animated: bool,
    pub has_alpha: bool,
    /// 1-8 EXIF orientation as stored in the source, before baking.
    pub orientation: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentInfo {
    pub id: String,
    pub kind: DocumentKind,
    pub name: String,
    pub path: Option<String>,
    pub page_count: u32,
    pub version: u64,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub encrypted: bool,
    pub read_only: bool,
    pub signed: bool,
    pub pages: Vec<PageInfo>,
    pub annotations: Vec<Annotation>,
    pub image: Option<ImageInfo>,
    /// MIME type returned by `render_page` for the current document state.
    pub mime: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub version: u64,
    pub changed_pages: Vec<u32>,
    pub page_count: u32,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextMatch {
    pub page: u32,
    pub rect: Rect,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SaveOptions {
    pub format: Option<String>,
    pub quality: Option<u8>,
    pub password: Option<String>,
    /// Explicitly write an unencrypted PDF copy, even when the source was encrypted.
    pub remove_password: bool,
    pub strip_gps: bool,
    /// Page used when exporting a PDF as PNG or JPEG.
    pub page: Option<u32>,
    /// Resolution used when exporting a PDF as PNG or JPEG.
    pub dpi: Option<f32>,
    /// Proceed even when the PDF contains a digital signature.
    pub force: bool,
    /// Zero-based page indices to write into an extracted PDF.
    pub pages: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetadataEntry {
    pub group: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetadataReport {
    pub entries: Vec<MetadataEntry>,
    pub can_strip_gps: bool,
    pub gps_present: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BookmarkInfo {
    pub title: String,
    pub page: u32,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FormFieldInfo {
    pub name: String,
    pub kind: String,
    pub value: String,
    pub options: Vec<String>,
    pub checked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormFieldUpdate {
    pub field: String,
    pub value: String,
}
