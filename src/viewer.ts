import { api } from "./api";
import type {
  Annotation,
  AnnotationKind,
  CropRect,
  DocumentInfo,
  PageInfo,
  Point,
  Rect,
  TextMatch,
  Tool,
} from "./types";

export interface ViewerEvents {
  onPageChange(index: number): void;
  onZoomChange(percent: number): void;
  onSelect(annotation: Annotation | null): void;
  onCreate(annotation: Annotation): void;
  onEdit(before: Annotation, after: Annotation): void;
  onEditText(annotation: Annotation): void;
  onCropChange(crop: CropRect | null): void;
  onRedact(page: number, rect: Rect): void;
}

export interface ViewerSettings {
  color: string;
  width: number;
  fontSize: number;
}

type FitMode = "width" | "page" | null;
type Handle = "nw" | "ne" | "sw" | "se";

interface Anchor {
  index: number;
  fraction: number;
}

interface CacheEntry {
  url: string;
  dpi: number;
}

interface DragDraw {
  kind: "draw";
  page: number;
  start: Point;
  draft: Annotation;
}

interface DragMove {
  kind: "move";
  page: number;
  before: Annotation;
  after: Annotation;
  start: Point;
  moved: boolean;
}

interface DragResize {
  kind: "resize";
  page: number;
  before: Annotation;
  after: Annotation;
  start: Point;
  handle: Handle;
  originalBounds: Rect;
  originalPoints: Point[];
  moved: boolean;
}

interface DragCrop {
  kind: "crop";
  page: number;
  start: Point;
}

interface DragRedact {
  kind: "redact";
  page: number;
  start: Point;
  rect: Rect;
}

type DragState = DragDraw | DragMove | DragResize | DragCrop | DragRedact;

const MIN_ZOOM = 0.1;
const MAX_ZOOM = 10;
const ZOOM_STEPS = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1, 1.25, 1.5, 2, 3, 4, 6, 8, 10];
const PAGE_PADDING_X = 48;
const PAGE_PADDING_TOP = 28;
const FONT_STACK = '"Noto Sans", "Segoe UI", system-ui, sans-serif';

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function cloneAnnotation(annotation: Annotation): Annotation {
  return {
    ...annotation,
    rect: { ...annotation.rect },
    points: annotation.points.map((point) => ({ ...point })),
  };
}

function normalizeRect(a: Point, b: Point): Rect {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    width: Math.abs(a.x - b.x),
    height: Math.abs(a.y - b.y),
  };
}

function newId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `a-${Date.now()}-${Math.round(Math.random() * 1e9)}`;
}

function distanceToSegment(point: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lengthSq = dx * dx + dy * dy;
  if (lengthSq === 0) {
    return Math.hypot(point.x - a.x, point.y - a.y);
  }
  const t = clamp(((point.x - a.x) * dx + (point.y - a.y) * dy) / lengthSq, 0, 1);
  return Math.hypot(point.x - (a.x + t * dx), point.y - (a.y + t * dy));
}

function boundsOf(annotation: Annotation): Rect {
  const rect = annotation.rect;
  let left = rect.x;
  let top = rect.y;
  let right = rect.x + rect.width;
  let bottom = rect.y + rect.height;
  for (const point of annotation.points) {
    left = Math.min(left, point.x);
    top = Math.min(top, point.y);
    right = Math.max(right, point.x);
    bottom = Math.max(bottom, point.y);
  }
  return { x: left, y: top, width: right - left, height: bottom - top };
}

function hitAnnotation(annotation: Annotation, point: Point, tolerance: number): boolean {
  if (annotation.kind === "line" || annotation.kind === "arrow") {
    const points = annotation.points.length >= 2 ? annotation.points : [
      { x: annotation.rect.x, y: annotation.rect.y + annotation.rect.height },
      { x: annotation.rect.x + annotation.rect.width, y: annotation.rect.y },
    ];
    for (let i = 0; i < points.length - 1; i += 1) {
      if (distanceToSegment(point, points[i], points[i + 1]) <= tolerance + annotation.width / 2) {
        return true;
      }
    }
    return false;
  }
  if (annotation.kind === "ink") {
    for (let i = 0; i < annotation.points.length - 1; i += 1) {
      if (distanceToSegment(point, annotation.points[i], annotation.points[i + 1]) <= tolerance) {
        return true;
      }
    }
    return false;
  }
  const bounds = boundsOf(annotation);
  return (
    point.x >= bounds.x - tolerance &&
    point.x <= bounds.x + bounds.width + tolerance &&
    point.y >= bounds.y - tolerance &&
    point.y <= bounds.y + bounds.height + tolerance
  );
}

function wrapLines(ctx: CanvasRenderingContext2D, text: string, maxWidth: number): string[] {
  const lines: string[] = [];
  for (const paragraph of text.split("\n")) {
    const words = paragraph.split(/\s+/).filter(Boolean);
    if (words.length === 0) {
      lines.push("");
      continue;
    }
    let line = words[0];
    for (let i = 1; i < words.length; i += 1) {
      const candidate = `${line} ${words[i]}`;
      if (ctx.measureText(candidate).width <= maxWidth) {
        line = candidate;
      } else {
        lines.push(line);
        line = words[i];
      }
    }
    lines.push(line);
  }
  return lines;
}

export class Viewer {
  readonly root: HTMLElement;
  private readonly pagesEl: HTMLElement;
  private readonly events: ViewerEvents;
  private readonly dpr = Math.min(window.devicePixelRatio || 1, 2);

  doc: DocumentInfo | null = null;
  zoom = 1;
  tool: Tool = "select";
  settings: ViewerSettings = { color: "#e03131", width: 2, fontSize: 13 };

  private fitMode: FitMode = null;
  private annotations: Annotation[] = [];
  private readonly signatureImages = new Map<string, HTMLImageElement>();
  private selected: Annotation | null = null;
  private matches: TextMatch[] = [];
  private activeMatch = -1;
  private crop: CropRect | null = null;
  private redactionDraft: { page: number; rect: Rect } | null = null;

  private pageEls: HTMLElement[] = [];
  private images: HTMLImageElement[] = [];
  private canvases: HTMLCanvasElement[] = [];
  private hitLayers: HTMLElement[] = [];
  private pageTops: number[] = [];
  private currentPage = 0;

  private observer: IntersectionObserver | null = null;
  private resizeObserver: ResizeObserver | null = null;
  private visible = new Set<number>();
  private queue: number[] = [];
  private cache = new Map<number, CacheEntry>();
  private renderState = new Map<number, "loading" | "error">();
  private pumping = false;
  private epoch = 0;
  private renderTimer = 0;
  private scrollRaf = 0;
  private resizeTimer = 0;
  private drag: DragState | null = null;
  private destroyed = false;

  constructor(root: HTMLElement, events: ViewerEvents) {
    this.root = root;
    this.events = events;
    this.pagesEl = document.createElement("div");
    this.pagesEl.className = "pages";
    root.append(this.pagesEl);

    root.addEventListener("scroll", this.onScroll, { passive: true });
    root.addEventListener("pointerdown", this.onPointerDown);
    window.addEventListener("pointermove", this.onPointerMove);
    window.addEventListener("pointerup", this.onPointerUp);
    root.addEventListener("dblclick", this.onDoubleClick);
    root.addEventListener("pointermove", this.onHover);

    this.resizeObserver = new ResizeObserver(() => {
      window.clearTimeout(this.resizeTimer);
      this.resizeTimer = window.setTimeout(() => {
        this.updatePageTops();
        if (this.fitMode) {
          this.applyFit(this.fitMode, false);
        }
      }, 90);
    });
    this.resizeObserver.observe(root);
  }

  // -------------------------------------------------------------------------
  // Document lifecycle
  // -------------------------------------------------------------------------

  setDocument(doc: DocumentInfo): void {
    this.epoch += 1;
    this.clearCache();
    this.doc = doc;
    this.annotations = doc.annotations.map(cloneAnnotation);
    this.selected = null;
    this.matches = [];
    this.activeMatch = -1;
    this.crop = null;
    this.fitMode = null;
    this.currentPage = 0;
    this.buildPages();
    this.events.onSelect(null);
    this.root.scrollTop = 0;
    this.applyInitialFit();
    this.updateCurrentPage(true);
  }

  clearDocument(): void {
    this.epoch += 1;
    this.observer?.disconnect();
    this.observer = null;
    this.clearCache();
    this.doc = null;
    this.annotations = [];
    this.selected = null;
    this.matches = [];
    this.activeMatch = -1;
    this.crop = null;
    this.pageEls = [];
    this.images = [];
    this.canvases = [];
    this.hitLayers = [];
    this.pageTops = [];
    this.visible.clear();
    this.pagesEl.replaceChildren();
    this.events.onSelect(null);
    this.events.onCropChange(null);
  }

  update(doc: DocumentInfo, resetImages: boolean): void {
    const previous = this.doc;
    const sizeChanged =
      !previous ||
      previous.page_count !== doc.page_count ||
      doc.pages.some((page, index) => {
        const old = previous.pages[index];
        return (
          !old ||
          Math.abs(old.width - page.width) > 0.01 ||
          Math.abs(old.height - page.height) > 0.01 ||
          Math.abs(old.raw_width - page.raw_width) > 0.01 ||
          Math.abs(old.raw_height - page.raw_height) > 0.01 ||
          old.rotation !== page.rotation
        );
      });

    this.doc = doc;
    this.annotations = doc.annotations.map(cloneAnnotation);
    this.selected = this.selected
      ? this.annotations.find((annotation) => annotation.id === this.selected?.id) ?? null
      : null;

    if (sizeChanged) {
      const anchor = this.captureAnchor();
      this.buildPages();
      this.restoreAnchor(anchor);
    } else if (resetImages) {
      this.invalidateRenderedImages();
      this.layoutPages();
    } else {
      this.redraw();
    }

    this.events.onSelect(this.selected ? cloneAnnotation(this.selected) : null);
    this.updateCurrentPage(false);
  }

  setImagePreviewFilter(filter: string | null): void {
    if (this.doc?.kind !== "image") return;
    const image = this.images[0];
    if (image) image.style.filter = filter ?? "";
  }

  private applyInitialFit(): void {
    if (!this.doc) return;
    if (this.doc.kind === "image") {
      this.applyFit("page", true);
      return;
    }
    const widthPercent = this.fitWidthPercent();
    if (widthPercent < 99.5) {
      this.applyZoom(widthPercent, "width", true);
    } else {
      this.applyZoom(100, null, true);
    }
  }

  // -------------------------------------------------------------------------
  // DOM construction and layout
  // -------------------------------------------------------------------------

  private buildPages(): void {
    this.observer?.disconnect();
    this.observer = null;
    this.revokeCache();
    this.pageEls = [];
    this.images = [];
    this.canvases = [];
    this.hitLayers = [];
    this.visible.clear();
    this.queue = [];
    this.renderState.clear();
    this.pagesEl.replaceChildren();
    if (!this.doc) {
      return;
    }

    for (let index = 0; index < this.doc.pages.length; index += 1) {
      const page = this.doc.pages[index];
      const section = document.createElement("section");
      section.className = "page";
      section.dataset.page = String(index);
      section.setAttribute("aria-label", `Page ${index + 1}`);
      if (this.doc.kind === "image" && this.doc.image?.has_alpha) {
        section.classList.add("has-alpha");
      }

      if (this.doc.kind === "pdf") {
        const chip = document.createElement("span");
        chip.className = "page-chip";
        chip.textContent = String(index + 1);
        section.append(chip);
      }

      const image = document.createElement("img");
      image.className = "page-img";
      image.alt = "";
      image.draggable = false;
      image.addEventListener("load", () => section.classList.add("is-ready"));
      image.addEventListener("error", () => section.classList.add("is-error"));
      section.append(image);

      const hits = document.createElement("div");
      hits.className = "page-hits";
      section.append(hits);

      const canvas = document.createElement("canvas");
      canvas.className = "page-canvas";
      canvas.dataset.page = String(index);
      section.append(canvas);

      const spinner = document.createElement("div");
      spinner.className = "page-spinner";
      spinner.setAttribute("aria-hidden", "true");
      section.append(spinner);

      this.pageEls.push(section);
      this.images.push(image);
      this.canvases.push(canvas);
      this.hitLayers.push(hits);
      this.pagesEl.append(section);
      if (!page) break;
    }

    this.observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const index = Number((entry.target as HTMLElement).dataset.page);
          if (Number.isNaN(index)) continue;
          if (entry.isIntersecting) {
            this.visible.add(index);
            this.queueRender(index);
          } else {
            this.visible.delete(index);
          }
        }
        this.trimCache();
      },
      { root: this.root, rootMargin: "800px 0px 800px 0px" },
    );
    for (const page of this.pageEls) {
      this.observer.observe(page);
    }

    this.layoutPages();
    this.rebuildAllHits();
    this.queueVisiblePages();
  }

  private layoutPages(): void {
    if (!this.doc) return;
    for (let index = 0; index < this.pageEls.length; index += 1) {
      const page = this.doc.pages[index];
      if (!page) continue;
      const width = page.width * this.zoom;
      const height = page.height * this.zoom;
      const section = this.pageEls[index];
      section.style.width = `${width}px`;
      section.style.height = `${height}px`;
      const canvas = this.canvases[index];
      const scale = Math.min(
        this.dpr,
        8192 / Math.max(1, width),
        8192 / Math.max(1, height),
      );
      canvas.width = Math.max(1, Math.round(width * scale));
      canvas.height = Math.max(1, Math.round(height * scale));
      canvas.dataset.scale = String(scale);
      this.drawPage(index);
    }
    this.updatePageTops();
  }

  private updatePageTops(): void {
    if (!this.doc) {
      this.pageTops = [];
      return;
    }
    const rootRect = this.root.getBoundingClientRect();
    const scrollTop = this.root.scrollTop;
    this.pageTops = this.pageEls.map((element) => {
      const rect = element.getBoundingClientRect();
      return rect.top - rootRect.top + scrollTop;
    });
  }

  private pageTop(index: number): number {
    const element = this.pageEls[index];
    if (!element) return 0;
    return element.getBoundingClientRect().top - this.root.getBoundingClientRect().top + this.root.scrollTop;
  }

  private currentPageIndex(): number {
    if (this.pageTops.length === 0) return 0;
    const threshold =
      this.root.scrollTop + Math.min(220, Math.max(60, this.root.clientHeight * 0.3));
    let index = 0;
    for (let i = 0; i < this.pageTops.length; i += 1) {
      if (this.pageTops[i] <= threshold) index = i;
      else break;
    }
    return index;
  }

  private updateCurrentPage(force: boolean): void {
    if (!this.doc) return;
    const index = clamp(this.currentPageIndex(), 0, Math.max(0, this.doc.pages.length - 1));
    if (force || index !== this.currentPage) {
      this.currentPage = index;
      this.events.onPageChange(index);
    }
  }

  // -------------------------------------------------------------------------
  // Rendering
  // -------------------------------------------------------------------------

  private targetDpi(): number {
    return clamp(72 * this.zoom * this.dpr, 24, 864);
  }

  private queueVisiblePages(): void {
    if (!this.doc) return;
    const rootRect = this.root.getBoundingClientRect();
    for (let index = 0; index < this.pageEls.length; index += 1) {
      const rect = this.pageEls[index].getBoundingClientRect();
      if (rect.bottom > rootRect.top - 800 && rect.top < rootRect.bottom + 800) {
        this.visible.add(index);
        this.queueRender(index);
      }
    }
  }

  private queueRender(index: number): void {
    if (!this.doc || index < 0 || index >= this.pageEls.length) return;
    if (this.renderState.get(index) === "loading") return;
    const cached = this.cache.get(index);
    const dpi = this.targetDpi();
    if (cached && Math.abs(cached.dpi - dpi) / dpi < 0.08) return;
    if (this.queue.includes(index)) return;
    this.queue.push(index);
    void this.pump();
  }

  private async pump(): Promise<void> {
    if (this.pumping) return;
    this.pumping = true;
    try {
      while (this.queue.length > 0) {
        const index = this.queue.shift();
        if (index === undefined || !this.doc) continue;
        const docId = this.doc.id;
        const epoch = this.epoch;
        const dpi = this.targetDpi();
        this.renderState.set(index, "loading");
        try {
          const buffer = await api.renderPage(docId, index, dpi);
          if (this.destroyed || this.epoch !== epoch || this.doc?.id !== docId) {
            if (this.renderState.get(index) === "loading") {
              this.renderState.delete(index);
            }
            continue;
          }
          const blob = new Blob([buffer], { type: this.doc.mime || "image/png" });
          const url = URL.createObjectURL(blob);
          const previous = this.cache.get(index);
          if (previous) URL.revokeObjectURL(previous.url);
          this.cache.set(index, { url, dpi });
          this.renderState.delete(index);
          const image = this.images[index];
          if (image) {
            image.src = url;
          }
          this.trimCache();
        } catch {
          this.renderState.set(index, "error");
          this.pageEls[index]?.classList.add("is-error");
        }
      }
    } finally {
      this.pumping = false;
    }
  }

  private trimCache(): void {
    if (this.cache.size <= 10) return;
    for (const key of Array.from(this.cache.keys())) {
      if (this.cache.size <= 10) break;
      if (!this.visible.has(key) && key !== this.currentPage) {
        const entry = this.cache.get(key);
        if (entry) URL.revokeObjectURL(entry.url);
        this.cache.delete(key);
        this.images[key]?.removeAttribute("src");
        this.pageEls[key]?.classList.remove("is-ready");
      }
    }
  }

  private revokeCache(): void {
    for (const entry of this.cache.values()) {
      URL.revokeObjectURL(entry.url);
    }
    this.cache.clear();
  }

  private clearCache(): void {
    this.revokeCache();
    for (const image of this.images) {
      image.removeAttribute("src");
      image.removeAttribute("srcset");
    }
    this.renderState.clear();
    this.queue = [];
  }

  private invalidateRenderedImages(): void {
    this.epoch += 1;
    this.clearCache();
    for (const page of this.pageEls) {
      page.classList.remove("is-ready", "is-error");
    }
    this.queueVisiblePages();
  }

  private scheduleRerender(): void {
    window.clearTimeout(this.renderTimer);
    this.renderTimer = window.setTimeout(() => {
      this.epoch += 1;
      this.queue = [];
      this.renderState.clear();
      this.visible.forEach((index) => this.queueRender(index));
      this.queueVisiblePages();
    }, 180);
  }

  // -------------------------------------------------------------------------
  // Zoom and navigation
  // -------------------------------------------------------------------------

  zoomPercent(): number {
    return Math.round(this.zoom * 1000) / 10;
  }

  setZoom(percent: number): void {
    this.applyZoom(percent, null);
  }

  zoomIn(): void {
    const current = this.zoom;
    const next = ZOOM_STEPS.find((step) => step > current + 0.001) ?? MAX_ZOOM;
    this.applyZoom(next * 100, null);
  }

  zoomOut(): void {
    const current = this.zoom;
    const next = [...ZOOM_STEPS].reverse().find((step) => step < current - 0.001) ?? MIN_ZOOM;
    this.applyZoom(next * 100, null);
  }

  actualSize(): void {
    this.applyZoom(100, null);
  }

  fitWidth(): void {
    this.applyFit("width", true);
  }

  fitPage(): void {
    this.applyFit("page", true);
  }

  private fitWidthPercent(): number {
    if (!this.doc) return 100;
    const page = this.doc.pages[this.currentPage] ?? this.doc.pages[0];
    if (!page) return 100;
    const available = Math.max(100, this.root.clientWidth - PAGE_PADDING_X * 2);
    return clamp((available / Math.max(1, page.width)) * 100, MIN_ZOOM * 100, MAX_ZOOM * 100);
  }

  private fitPagePercent(): number {
    if (!this.doc) return 100;
    const page = this.doc.pages[this.currentPage] ?? this.doc.pages[0];
    if (!page) return 100;
    const availableWidth = Math.max(100, this.root.clientWidth - PAGE_PADDING_X * 2);
    const availableHeight = Math.max(100, this.root.clientHeight - 88);
    return clamp(
      Math.min(availableWidth / Math.max(1, page.width), availableHeight / Math.max(1, page.height)) * 100,
      MIN_ZOOM * 100,
      MAX_ZOOM * 100,
    );
  }

  private applyFit(mode: "width" | "page", animateScale: boolean): void {
    const percent = mode === "width" ? this.fitWidthPercent() : this.fitPagePercent();
    this.applyZoom(percent, mode, animateScale);
  }

  private applyZoom(percent: number, mode: FitMode, preserveAnchor = true): void {
    const next = clamp(percent / 100, MIN_ZOOM, MAX_ZOOM);
    const anchor = preserveAnchor ? this.captureAnchor() : null;
    this.zoom = next;
    this.fitMode = mode;
    this.layoutPages();
    if (anchor) this.restoreAnchor(anchor);
    this.events.onZoomChange(this.zoomPercent());
    this.scheduleRerender();
  }

  private captureAnchor(): Anchor {
    if (!this.doc || this.pageEls.length === 0) {
      return { index: 0, fraction: 0 };
    }
    const index = clamp(this.currentPageIndex(), 0, this.pageEls.length - 1);
    const top = this.pageTop(index);
    const height = this.pageEls[index].getBoundingClientRect().height;
    const fraction = height > 0 ? clamp((this.root.scrollTop - top) / height, 0, 1) : 0;
    return { index, fraction };
  }

  private restoreAnchor(anchor: Anchor): void {
    if (this.pageEls.length === 0) return;
    const index = clamp(anchor.index, 0, this.pageEls.length - 1);
    const top = this.pageTop(index);
    const height = this.pageEls[index].getBoundingClientRect().height;
    this.root.scrollTop = Math.max(0, top + height * anchor.fraction);
    this.updatePageTops();
  }

  refit(): void {
    if (!this.doc) return;
    if (this.fitMode) {
      this.applyFit(this.fitMode, false);
    } else {
      this.updatePageTops();
    }
  }

  goToPage(index: number): void {
    if (!this.doc) return;
    const target = clamp(index, 0, this.pageEls.length - 1);
    const element = this.pageEls[target];
    if (!element) return;
    this.root.scrollTo({ top: Math.max(0, this.pageTop(target) - PAGE_PADDING_TOP), behavior: "auto" });
    this.currentPage = target;
    this.events.onPageChange(target);
    this.queueRender(target);
    this.updatePageTops();
  }

  currentPageNumber(): number {
    return this.currentPage;
  }

  pageCount(): number {
    return this.doc?.pages.length ?? 0;
  }

  // -------------------------------------------------------------------------
  // Selection and annotations
  // -------------------------------------------------------------------------

  setTool(tool: Tool): void {
    this.tool = tool;
    if (tool !== "select") {
      this.select(null);
    }
    if (tool !== "crop") {
      this.clearCrop();
    }
    if (tool !== "redact") {
      this.redactionDraft = null;
    }
    for (const canvas of this.canvases) {
      canvas.style.cursor = tool === "redact" ? "crosshair" : "";
    }
  }

  setSettings(settings: Partial<ViewerSettings>): void {
    this.settings = { ...this.settings, ...settings };
  }

  select(id: string | null): void {
    const next = id ? this.annotations.find((annotation) => annotation.id === id) ?? null : null;
    if (this.selected?.id === next?.id) {
      if (this.selected && next) {
        this.selected = next;
      }
      return;
    }
    this.selected = next;
    this.redraw();
    this.events.onSelect(next ? cloneAnnotation(next) : null);
  }

  selectAnnotation(annotation: Annotation | null): void {
    this.select(annotation?.id ?? null);
  }

  getSelected(): Annotation | null {
    return this.selected ? cloneAnnotation(this.selected) : null;
  }

  getAnnotation(id: string): Annotation | null {
    const annotation = this.annotations.find((candidate) => candidate.id === id);
    return annotation ? cloneAnnotation(annotation) : null;
  }

  annotationCount(): number {
    return this.annotations.length;
  }

  // -------------------------------------------------------------------------
  // Search
  // -------------------------------------------------------------------------

  setSearch(matches: TextMatch[], active: number): void {
    this.matches = matches;
    this.activeMatch = active;
    this.rebuildAllHits();
    this.highlightActiveMatch();
  }

  clearSearch(): void {
    this.matches = [];
    this.activeMatch = -1;
    this.rebuildAllHits();
  }

  private rebuildAllHits(): void {
    for (const layer of this.hitLayers) {
      layer.replaceChildren();
    }
    if (!this.doc) return;
    this.matches.forEach((match, index) => {
      const page = this.doc?.pages[match.page];
      const layer = this.hitLayers[match.page];
      if (!page || !layer) return;
      const hit = document.createElement("div");
      hit.className = "search-hit";
      hit.dataset.match = String(index);
      hit.style.left = `${(match.rect.x / page.width) * 100}%`;
      hit.style.top = `${(match.rect.y / page.height) * 100}%`;
      hit.style.width = `${(match.rect.width / page.width) * 100}%`;
      hit.style.height = `${(match.rect.height / page.height) * 100}%`;
      layer.append(hit);
    });
    this.highlightActiveMatch();
  }

  private highlightActiveMatch(): void {
    for (const layer of this.hitLayers) {
      for (const hit of Array.from(layer.children)) {
        hit.classList.toggle(
          "active",
          Number((hit as HTMLElement).dataset.match) === this.activeMatch,
        );
      }
    }
  }

  goToMatch(index: number): void {
    const match = this.matches[index];
    if (!match) return;
    this.activeMatch = index;
    this.highlightActiveMatch();
    const page = this.doc?.pages[match.page];
    if (!page) return;
    const top = this.pageTop(match.page);
    const y = match.rect.y * this.zoom;
    const target = top + y - this.root.clientHeight * 0.32;
    this.root.scrollTo({ top: Math.max(0, target), behavior: "auto" });
    this.queueRender(match.page);
    this.currentPage = match.page;
    this.events.onPageChange(match.page);
    this.updatePageTops();
    const hit = this.hitLayers[match.page]?.querySelector<HTMLElement>(
      `.search-hit[data-match="${index}"]`,
    );
    if (hit) {
      hit.classList.remove("pulse");
      void hit.offsetWidth;
      hit.classList.add("pulse");
    }
  }

  // -------------------------------------------------------------------------
  // Crop
  // -------------------------------------------------------------------------

  getCrop(): CropRect | null {
    if (!this.crop) return null;
    const page = this.doc?.pages[0];
    if (!page) return null;
    const x = clamp(Math.round(this.crop.x), 0, Math.max(0, Math.round(page.width) - 1));
    const y = clamp(Math.round(this.crop.y), 0, Math.max(0, Math.round(page.height) - 1));
    const width = clamp(Math.round(this.crop.width), 1, Math.round(page.width) - x);
    const height = clamp(Math.round(this.crop.height), 1, Math.round(page.height) - y);
    return { x, y, width, height };
  }

  hasCrop(): boolean {
    return this.crop !== null;
  }

  clearCrop(): void {
    if (this.crop) {
      this.crop = null;
      this.redraw();
    }
    this.events.onCropChange(null);
  }

  // -------------------------------------------------------------------------
  // Painting
  // -------------------------------------------------------------------------

  redraw(): void {
    for (let index = 0; index < this.canvases.length; index += 1) {
      this.drawPage(index);
    }
  }

  private drawPage(index: number): void {
    const doc = this.doc;
    const canvas = this.canvases[index];
    const page = doc?.pages[index];
    if (!doc || !canvas || !page) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const scale = Number(canvas.dataset.scale || this.dpr);
    const transform = scale * this.zoom;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.setTransform(transform, 0, 0, transform, 0, 0);

    for (const annotation of this.annotations) {
      if (annotation.page === index) {
        this.paintAnnotation(ctx, annotation);
      }
    }

    if (this.selected && this.selected.page === index) {
      this.paintSelection(ctx, this.selected);
    }

    if (this.drag?.kind === "draw" && this.drag.page === index) {
      this.paintAnnotation(ctx, this.drag.draft);
      this.paintDraftOutline(ctx, this.drag.draft);
    }

    if (this.redactionDraft?.page === index) {
      this.paintRedactionDraft(ctx, this.redactionDraft.rect);
    }

    if (index === 0 && this.crop && doc.kind === "image") {
      this.paintCrop(ctx, page);
    }
  }

  private paintAnnotation(ctx: CanvasRenderingContext2D, annotation: Annotation): void {
    const rect = annotation.rect;
    const color = annotation.color;
    ctx.save();
    ctx.globalAlpha = annotation.opacity;
    ctx.strokeStyle = color;
    ctx.fillStyle = color;
    ctx.lineWidth = Math.max(0.5, annotation.width);
    ctx.lineCap = "round";
    ctx.lineJoin = "round";

    switch (annotation.kind) {
      case "highlight": {
        ctx.fillStyle = color;
        ctx.fillRect(rect.x, rect.y, rect.width, rect.height);
        break;
      }
      case "underline":
      case "strikeout": {
        const y =
          annotation.kind === "underline"
            ? rect.y + rect.height - Math.max(1, annotation.width / 2)
            : rect.y + rect.height / 2;
        ctx.beginPath();
        ctx.moveTo(rect.x, y);
        ctx.lineTo(rect.x + rect.width, y);
        ctx.stroke();
        break;
      }
      case "square": {
        const inset = annotation.width / 2;
        if (annotation.fill) {
          ctx.fillStyle = annotation.fill;
          ctx.fillRect(rect.x + inset, rect.y + inset, Math.max(0.5, rect.width - inset * 2), Math.max(0.5, rect.height - inset * 2));
          ctx.fillStyle = color;
        }
        ctx.strokeRect(rect.x + inset, rect.y + inset, Math.max(0.5, rect.width - annotation.width), Math.max(0.5, rect.height - annotation.width));
        break;
      }
      case "circle": {
        ctx.beginPath();
        ctx.ellipse(
          rect.x + rect.width / 2,
          rect.y + rect.height / 2,
          Math.max(0.5, rect.width / 2 - annotation.width / 2),
          Math.max(0.5, rect.height / 2 - annotation.width / 2),
          0,
          0,
          Math.PI * 2,
        );
        if (annotation.fill) {
          ctx.fillStyle = annotation.fill;
          ctx.fill();
          ctx.fillStyle = color;
        }
        ctx.stroke();
        break;
      }
      case "line":
      case "arrow": {
        const points =
          annotation.points.length >= 2
            ? annotation.points
            : [
                { x: rect.x, y: rect.y + rect.height },
                { x: rect.x + rect.width, y: rect.y },
              ];
        ctx.beginPath();
        ctx.moveTo(points[0].x, points[0].y);
        for (let i = 1; i < points.length; i += 1) ctx.lineTo(points[i].x, points[i].y);
        ctx.stroke();
        if (annotation.kind === "arrow") {
          const end = points[points.length - 1];
          const start = points[points.length - 2];
          const angle = Math.atan2(end.y - start.y, end.x - start.x);
          const head = clamp(annotation.width * 4, 6, 22);
          ctx.beginPath();
          ctx.moveTo(end.x, end.y);
          ctx.lineTo(
            end.x - head * Math.cos(angle - Math.PI / 7),
            end.y - head * Math.sin(angle - Math.PI / 7),
          );
          ctx.lineTo(
            end.x - head * Math.cos(angle + Math.PI / 7),
            end.y - head * Math.sin(angle + Math.PI / 7),
          );
          ctx.closePath();
          ctx.fill();
        }
        break;
      }
      case "free_text": {
        if (annotation.fill) {
          ctx.fillStyle = annotation.fill;
          ctx.fillRect(rect.x, rect.y, rect.width, rect.height);
        }
        const text = annotation.contents?.trim();
        if (text) {
          ctx.save();
          ctx.beginPath();
          ctx.rect(rect.x + 3, rect.y + 3, Math.max(1, rect.width - 6), Math.max(1, rect.height - 6));
          ctx.clip();
          ctx.fillStyle = color;
          const size = Math.max(6, annotation.font_size);
          ctx.font = `${annotation.italic ? "italic " : ""}${annotation.bold ? "600 " : "400 "}${size}px ${FONT_STACK}`;
          ctx.textBaseline = "alphabetic";
          const lines = wrapLines(ctx, text, Math.max(10, rect.width - 8));
          const lineHeight = size * 1.34;
          let y = rect.y + 4 + size;
          for (const line of lines) {
            if (y > rect.y + rect.height - 2) break;
            ctx.fillText(line, rect.x + 4, y);
            y += lineHeight;
          }
          ctx.restore();
        }
        break;
      }
      case "note": {
        const fold = Math.min(rect.width, rect.height) * 0.3;
        ctx.fillStyle = annotation.fill || "#ffd43b";
        ctx.strokeStyle = color;
        ctx.beginPath();
        ctx.moveTo(rect.x + 1, rect.y + 1);
        ctx.lineTo(rect.x + rect.width - fold, rect.y + 1);
        ctx.lineTo(rect.x + rect.width - 1, rect.y + fold);
        ctx.lineTo(rect.x + rect.width - 1, rect.y + rect.height - 1);
        ctx.lineTo(rect.x + 1, rect.y + rect.height - 1);
        ctx.closePath();
        ctx.fill();
        ctx.stroke();
        ctx.beginPath();
        ctx.moveTo(rect.x + rect.width - fold, rect.y + 1);
        ctx.lineTo(rect.x + rect.width - fold, rect.y + fold);
        ctx.lineTo(rect.x + rect.width - 1, rect.y + fold);
        ctx.stroke();
        ctx.strokeStyle = "rgba(50, 40, 10, .6)";
        ctx.lineWidth = Math.max(0.6, annotation.width * 0.5);
        for (let i = 0; i < 2; i += 1) {
          const y = rect.y + rect.height * (0.48 + i * 0.2);
          ctx.beginPath();
          ctx.moveTo(rect.x + rect.width * 0.2, y);
          ctx.lineTo(rect.x + rect.width * 0.78, y);
          ctx.stroke();
        }
        break;
      }
      case "ink": {
        if (annotation.points.length >= 2) {
          ctx.beginPath();
          ctx.moveTo(annotation.points[0].x, annotation.points[0].y);
          for (let i = 1; i < annotation.points.length; i += 1) {
            ctx.lineTo(annotation.points[i].x, annotation.points[i].y);
          }
          ctx.stroke();
        }
        break;
      }
      case "signature": {
        if (!annotation.image_data) break;
        let image = this.signatureImages.get(annotation.id);
        if (!image) {
          image = new Image();
          image.onload = () => this.drawPage(annotation.page);
          image.src = `data:image/png;base64,${annotation.image_data}`;
          this.signatureImages.set(annotation.id, image);
        }
        if (image.complete && image.naturalWidth > 0) {
          ctx.drawImage(image, rect.x, rect.y, rect.width, rect.height);
        }
        break;
      }
    }
    ctx.restore();
  }

  private paintDraftOutline(ctx: CanvasRenderingContext2D, annotation: Annotation): void {
    const bounds = boundsOf(annotation);
    ctx.save();
    ctx.strokeStyle = "rgba(31, 95, 139, .85)";
    ctx.lineWidth = 1.2 / this.zoom;
    ctx.setLineDash([4 / this.zoom, 4 / this.zoom]);
    ctx.strokeRect(bounds.x, bounds.y, bounds.width, bounds.height);
    ctx.restore();
  }

  private paintRedactionDraft(ctx: CanvasRenderingContext2D, rect: Rect): void {
    ctx.save();
    ctx.fillStyle = "rgba(0, 0, 0, .84)";
    ctx.fillRect(rect.x, rect.y, rect.width, rect.height);
    ctx.strokeStyle = "#ffffff";
    ctx.lineWidth = 1.5 / this.zoom;
    ctx.setLineDash([5 / this.zoom, 3 / this.zoom]);
    ctx.strokeRect(rect.x, rect.y, rect.width, rect.height);
    ctx.setLineDash([]);
    const fontSize = 11 / this.zoom;
    ctx.font = `700 ${fontSize}px ${FONT_STACK}`;
    ctx.fillStyle = "#ffffff";
    ctx.textBaseline = "middle";
    ctx.fillText("REDACT", rect.x + 4 / this.zoom, rect.y + Math.min(rect.height / 2, fontSize));
    ctx.restore();
  }

  private paintSelection(ctx: CanvasRenderingContext2D, annotation: Annotation): void {
    const bounds = boundsOf(annotation);
    const handle = 7 / this.zoom;
    ctx.save();
    ctx.strokeStyle = "#1f5f8b";
    ctx.fillStyle = "#ffffff";
    ctx.lineWidth = 1.4 / this.zoom;
    ctx.setLineDash([5 / this.zoom, 4 / this.zoom]);
    ctx.strokeRect(bounds.x - 1 / this.zoom, bounds.y - 1 / this.zoom, bounds.width + 2 / this.zoom, bounds.height + 2 / this.zoom);
    ctx.setLineDash([]);
    const corners = this.handlePoints(bounds);
    for (const point of Object.values(corners)) {
      ctx.beginPath();
      ctx.rect(point.x - handle / 2, point.y - handle / 2, handle, handle);
      ctx.fill();
      ctx.stroke();
    }
    ctx.restore();
  }

  private paintCrop(ctx: CanvasRenderingContext2D, page: PageInfo): void {
    if (!this.crop) return;
    const crop = this.crop;
    ctx.save();
    ctx.fillStyle = "rgba(14, 20, 24, .48)";
    ctx.fillRect(0, 0, page.width, crop.y);
    ctx.fillRect(0, crop.y + crop.height, page.width, Math.max(0, page.height - crop.y - crop.height));
    ctx.fillRect(0, crop.y, crop.x, crop.height);
    ctx.fillRect(crop.x + crop.width, crop.y, Math.max(0, page.width - crop.x - crop.width), crop.height);
    ctx.strokeStyle = "rgba(255, 255, 255, .95)";
    ctx.lineWidth = 1.5 / this.zoom;
    ctx.setLineDash([6 / this.zoom, 4 / this.zoom]);
    ctx.strokeRect(crop.x, crop.y, crop.width, crop.height);
    ctx.setLineDash([]);
    const handle = 7 / this.zoom;
    const corners = this.handlePoints(crop);
    ctx.fillStyle = "rgba(255, 255, 255, .95)";
    ctx.strokeStyle = "rgba(15, 20, 24, .55)";
    ctx.lineWidth = 1 / this.zoom;
    for (const point of Object.values(corners)) {
      ctx.fillRect(point.x - handle / 2, point.y - handle / 2, handle, handle);
      ctx.strokeRect(point.x - handle / 2, point.y - handle / 2, handle, handle);
    }

    const label = `${Math.round(crop.width)} × ${Math.round(crop.height)} px`;
    const fontSize = 12 / this.zoom;
    ctx.font = `600 ${fontSize}px ${FONT_STACK}`;
    const padding = 5 / this.zoom;
    const textWidth = ctx.measureText(label).width;
    const boxWidth = textWidth + padding * 2;
    const boxHeight = fontSize + padding * 2;
    let boxX = crop.x;
    let boxY = crop.y - boxHeight - 6 / this.zoom;
    if (boxY < 2 / this.zoom) boxY = crop.y + 6 / this.zoom;
    boxX = clamp(boxX, 2 / this.zoom, Math.max(2 / this.zoom, page.width - boxWidth - 2 / this.zoom));
    ctx.fillStyle = "rgba(20, 24, 28, .86)";
    ctx.beginPath();
    ctx.roundRect(boxX, boxY, boxWidth, boxHeight, 3 / this.zoom);
    ctx.fill();
    ctx.fillStyle = "#ffffff";
    ctx.textBaseline = "middle";
    ctx.fillText(label, boxX + padding, boxY + boxHeight / 2);
    ctx.restore();
  }

  private handlePoints(rect: Rect): Record<Handle, Point> {
    return {
      nw: { x: rect.x, y: rect.y },
      ne: { x: rect.x + rect.width, y: rect.y },
      sw: { x: rect.x, y: rect.y + rect.height },
      se: { x: rect.x + rect.width, y: rect.y + rect.height },
    };
  }

  // -------------------------------------------------------------------------
  // Pointer interaction
  // -------------------------------------------------------------------------

  private sectionForEvent(event: PointerEvent | MouseEvent): HTMLElement | null {
    const target = event.target as HTMLElement | null;
    return target?.closest<HTMLElement>(".page") ?? null;
  }

  private pointFor(section: HTMLElement, event: PointerEvent | MouseEvent): Point {
    const rect = section.getBoundingClientRect();
    const page = this.doc?.pages[Number(section.dataset.page)];
    const width = page?.width ?? 1;
    const height = page?.height ?? 1;
    return {
      x: clamp(((event.clientX - rect.left) / Math.max(1, rect.width)) * width, 0, width),
      y: clamp(((event.clientY - rect.top) / Math.max(1, rect.height)) * height, 0, height),
    };
  }

  private hitTest(page: number, point: Point): Annotation | null {
    const tolerance = 6 / this.zoom;
    for (let i = this.annotations.length - 1; i >= 0; i -= 1) {
      const annotation = this.annotations[i];
      if (annotation.page === page && hitAnnotation(annotation, point, tolerance)) {
        return annotation;
      }
    }
    return null;
  }

  private handleAt(annotation: Annotation, point: Point): Handle | null {
    const tolerance = 9 / this.zoom;
    const corners = this.handlePoints(boundsOf(annotation));
    let best: Handle | null = null;
    let bestDistance = tolerance;
    for (const [handle, corner] of Object.entries(corners) as [Handle, Point][]) {
      const distance = Math.hypot(point.x - corner.x, point.y - corner.y);
      if (distance <= bestDistance) {
        best = handle;
        bestDistance = distance;
      }
    }
    return best;
  }

  private defaultAnnotation(kind: AnnotationKind, page: number, rect: Rect, points: Point[] = []): Annotation {
    const colors: Partial<Record<AnnotationKind, string>> = {
      highlight: "#ffe066",
      underline: "#e03131",
      strikeout: "#e03131",
      note: "#b08900",
      free_text: "#1f2937",
    };
    return {
      id: newId(),
      page,
      kind,
      rect,
      points,
      image_data: null,
      color: colors[kind] ?? this.settings.color,
      fill: kind === "note" ? "#ffd43b" : null,
      width: this.settings.width,
      opacity: kind === "highlight" ? 0.4 : 1,
      contents: null,
      font_size: this.settings.fontSize,
      bold: false,
      italic: false,
      existing: false,
      dirty: false,
    };
  }

  private onPointerDown = (event: PointerEvent): void => {
    if (!this.doc || event.button !== 0) return;
    const section = this.sectionForEvent(event);
    if (!section) return;
    const pageIndex = Number(section.dataset.page);
    const page = this.doc.pages[pageIndex];
    if (!page) return;
    const point = this.pointFor(section, event);
    event.preventDefault();

    if (this.tool === "crop") {
      if (this.doc.kind !== "image" || pageIndex !== 0) return;
      this.drag = { kind: "crop", page: pageIndex, start: point };
      this.crop = { x: point.x, y: point.y, width: 0, height: 0 };
      this.drawPage(pageIndex);
      this.events.onCropChange(this.getCrop());
      return;
    }

    if (this.tool === "redact") {
      if (this.doc.kind !== "pdf") return;
      this.redactionDraft = { page: pageIndex, rect: { x: point.x, y: point.y, width: 0, height: 0 } };
      this.drag = { kind: "redact", page: pageIndex, start: point, rect: this.redactionDraft.rect };
      this.drawPage(pageIndex);
      return;
    }

    const annotation = this.hitTest(pageIndex, point);
    if (this.tool === "select") {
      if (annotation) {
        this.select(annotation.id);
        const handle = this.handleAt(annotation, point);
        if (handle) {
          this.drag = {
            kind: "resize",
            page: pageIndex,
            before: cloneAnnotation(annotation),
            after: annotation,
            start: point,
            handle,
            originalBounds: boundsOf(annotation),
            originalPoints: annotation.points.map((p) => ({ ...p })),
            moved: false,
          };
        } else {
          this.drag = {
            kind: "move",
            page: pageIndex,
            before: cloneAnnotation(annotation),
            after: annotation,
            start: point,
            moved: false,
          };
        }
      } else {
        this.select(null);
      }
      return;
    }

    if (this.tool === "highlight" || this.tool === "underline" || this.tool === "strikeout") {
      const draft = this.defaultAnnotation(this.tool, pageIndex, { x: point.x, y: point.y, width: 0, height: 0 });
      this.drag = { kind: "draw", page: pageIndex, start: point, draft };
    } else if (this.tool === "square" || this.tool === "circle" || this.tool === "line" || this.tool === "arrow") {
      const draft = this.defaultAnnotation(this.tool, pageIndex, { x: point.x, y: point.y, width: 0, height: 0 }, [
        { ...point },
        { ...point },
      ]);
      this.drag = { kind: "draw", page: pageIndex, start: point, draft };
    } else if (this.tool === "free_text") {
      const width = Math.min(220, Math.max(80, page.width - point.x));
      const height = Math.min(64, Math.max(28, page.height - point.y));
      const rect = { x: point.x, y: point.y, width, height };
      this.drag = {
        kind: "draw",
        page: pageIndex,
        start: point,
        draft: this.defaultAnnotation("free_text", pageIndex, rect),
      };
    } else if (this.tool === "note") {
      const size = 26 / this.zoom;
      const rect = { x: point.x, y: point.y, width: size, height: size };
      this.drag = {
        kind: "draw",
        page: pageIndex,
        start: point,
        draft: this.defaultAnnotation("note", pageIndex, rect),
      };
    }

    if (this.drag?.kind === "draw") {
      this.drawPage(pageIndex);
    }
  };

  private onPointerMove = (event: PointerEvent): void => {
    if (!this.drag || !this.doc) return;
    const section = this.pageEls[this.drag.page];
    if (!section) return;
    const point = this.pointFor(section, event);

    if (this.drag.kind === "crop") {
      const page = this.doc.pages[this.drag.page];
      if (!page) return;
      const rect = normalizeRect(this.drag.start, point);
      this.crop = {
        x: clamp(rect.x, 0, page.width),
        y: clamp(rect.y, 0, page.height),
        width: clamp(rect.width, 0, page.width - rect.x),
        height: clamp(rect.height, 0, page.height - rect.y),
      };
      this.drawPage(this.drag.page);
      this.events.onCropChange(this.getCrop());
      return;
    }

    if (this.drag.kind === "redact") {
      const page = this.doc.pages[this.drag.page];
      if (!page) return;
      const rect = normalizeRect(this.drag.start, point);
      this.drag.rect = {
        x: clamp(rect.x, 0, page.width),
        y: clamp(rect.y, 0, page.height),
        width: clamp(rect.width, 0, page.width - rect.x),
        height: clamp(rect.height, 0, page.height - rect.y),
      };
      this.redactionDraft = { page: this.drag.page, rect: this.drag.rect };
      this.drawPage(this.drag.page);
      return;
    }

    if (this.drag.kind === "draw") {
      const draft = this.drag.draft;
      if (draft.kind === "line" || draft.kind === "arrow") {
        draft.points = [{ ...this.drag.start }, { ...point }];
        draft.rect = normalizeRect(this.drag.start, point);
      } else if (draft.kind === "note") {
        // Sticky notes stay square; the click point is the anchor.
        const size = 26 / this.zoom;
        draft.rect = { x: this.drag.start.x, y: this.drag.start.y, width: size, height: size };
      } else if (draft.kind === "free_text") {
        const rect = normalizeRect(this.drag.start, point);
        if (rect.width > 24 && rect.height > 16) {
          draft.rect = rect;
        }
      } else {
        draft.rect = normalizeRect(this.drag.start, point);
      }
      this.drawPage(this.drag.page);
      return;
    }

    if (this.drag.kind === "move") {
      const dx = point.x - this.drag.start.x;
      const dy = point.y - this.drag.start.y;
      if (Math.abs(dx) > 0.25 || Math.abs(dy) > 0.25) this.drag.moved = true;
      this.drag.after.rect = {
        x: this.drag.before.rect.x + dx,
        y: this.drag.before.rect.y + dy,
        width: this.drag.before.rect.width,
        height: this.drag.before.rect.height,
      };
      this.drag.after.points = this.drag.before.points.map((p) => ({ x: p.x + dx, y: p.y + dy }));
      this.drawPage(this.drag.page);
      return;
    }

    if (this.drag.kind === "resize") {
      const anchor = {
        nw: { x: this.drag.originalBounds.x + this.drag.originalBounds.width, y: this.drag.originalBounds.y + this.drag.originalBounds.height },
        ne: { x: this.drag.originalBounds.x, y: this.drag.originalBounds.y + this.drag.originalBounds.height },
        sw: { x: this.drag.originalBounds.x + this.drag.originalBounds.width, y: this.drag.originalBounds.y },
        se: { x: this.drag.originalBounds.x, y: this.drag.originalBounds.y },
      }[this.drag.handle];
      let rect = normalizeRect(anchor, point);
      rect = {
        x: rect.x,
        y: rect.y,
        width: Math.max(6, rect.width),
        height: Math.max(4, rect.height),
      };
      if (point.x < anchor.x) rect.x = anchor.x - rect.width;
      if (point.y < anchor.y) rect.y = anchor.y - rect.height;
      this.drag.moved = true;
      this.drag.after.rect = rect;
      if (this.drag.originalPoints.length > 0) {
        const old = this.drag.originalBounds;
        const sx = old.width !== 0 ? rect.width / old.width : 1;
        const sy = old.height !== 0 ? rect.height / old.height : 1;
        this.drag.after.points = this.drag.originalPoints.map((p) => ({
          x: rect.x + (p.x - old.x) * sx,
          y: rect.y + (p.y - old.y) * sy,
        }));
      }
      this.drawPage(this.drag.page);
    }
  };

  private onPointerUp = (): void => {
    if (!this.drag) return;
    const drag = this.drag;
    this.drag = null;

    if (drag.kind === "crop") {
      if (!this.crop || this.crop.width < 2 || this.crop.height < 2) {
        this.crop = null;
        this.drawPage(drag.page);
        this.events.onCropChange(null);
      } else {
        this.drawPage(drag.page);
        this.events.onCropChange(this.getCrop());
      }
      return;
    }

    if (drag.kind === "redact") {
      this.redactionDraft = null;
      this.drawPage(drag.page);
      if (drag.rect.width >= 2 && drag.rect.height >= 2) {
        this.events.onRedact(drag.page, drag.rect);
      }
      return;
    }

    if (drag.kind === "draw") {
      this.drawPage(drag.page);
      const draft = drag.draft;
      if (draft.kind === "free_text" || draft.kind === "note") {
        this.events.onCreate(cloneAnnotation(draft));
        return;
      }
      if (draft.kind === "line" || draft.kind === "arrow") {
        const first = draft.points[0] ?? { x: 0, y: 0 };
        const second = draft.points[1] ?? first;
        const length = Math.hypot(second.x - first.x, second.y - first.y);
        if (length < 3) return;
      } else if (draft.rect.width < 2 || draft.rect.height < 2) {
        return;
      }
      this.events.onCreate(cloneAnnotation(draft));
      return;
    }

    if (!drag.moved) {
      this.drawPage(drag.page);
      return;
    }
    this.events.onEdit(drag.before, cloneAnnotation(drag.after));
    this.drawPage(drag.page);
  };

  private onDoubleClick = (event: MouseEvent): void => {
    if (!this.doc || this.tool !== "select") return;
    const section = this.sectionForEvent(event);
    if (!section) return;
    const pageIndex = Number(section.dataset.page);
    const point = this.pointFor(section, event);
    const annotation = this.hitTest(pageIndex, point);
    if (annotation) {
      event.preventDefault();
      this.select(annotation.id);
      this.events.onEditText(cloneAnnotation(annotation));
    }
  };

  private onHover = (event: PointerEvent): void => {
    if (this.drag || !this.doc || this.tool !== "select") return;
    const section = this.sectionForEvent(event);
    if (!section) return;
    const pageIndex = Number(section.dataset.page);
    const point = this.pointFor(section, event);
    const canvas = this.canvases[pageIndex];
    if (!canvas) return;
    if (this.selected && this.selected.page === pageIndex) {
      const handle = this.handleAt(this.selected, point);
      if (handle) {
        canvas.style.cursor = handle === "nw" || handle === "se" ? "nwse-resize" : "nesw-resize";
        return;
      }
    }
    const annotation = this.hitTest(pageIndex, point);
    canvas.style.cursor = annotation ? "move" : "";
  };

  // -------------------------------------------------------------------------
  // Scrolling and teardown
  // -------------------------------------------------------------------------

  private onScroll = (): void => {
    if (this.scrollRaf) return;
    this.scrollRaf = requestAnimationFrame(() => {
      this.scrollRaf = 0;
      this.updateCurrentPage(false);
    });
  };

  destroy(): void {
    this.destroyed = true;
    this.observer?.disconnect();
    this.resizeObserver?.disconnect();
    this.root.removeEventListener("scroll", this.onScroll);
    this.root.removeEventListener("pointerdown", this.onPointerDown);
    this.root.removeEventListener("dblclick", this.onDoubleClick);
    this.root.removeEventListener("pointermove", this.onHover);
    window.removeEventListener("pointermove", this.onPointerMove);
    window.removeEventListener("pointerup", this.onPointerUp);
    window.clearTimeout(this.renderTimer);
    window.clearTimeout(this.resizeTimer);
    if (this.scrollRaf) cancelAnimationFrame(this.scrollRaf);
    this.revokeCache();
    this.pagesEl.remove();
  }

  /** Used by the thumbnail rail without exposing the render internals there. */
  static async renderThumbnail(doc: DocumentInfo, page: number, cssWidth: number): Promise<Blob> {
    const info = doc.pages[page];
    const dpi = info
      ? clamp((cssWidth / Math.max(1, info.width)) * 72 * Math.min(window.devicePixelRatio || 1, 2), 12, 240)
      : 24;
    const buffer = await api.renderPage(doc.id, page, dpi);
    return new Blob([buffer], { type: doc.mime || "image/png" });
  }
}
