import { Viewer } from "./viewer";
import type { DocumentInfo } from "./types";

const PAGE_DRAG_TYPE = "application/x-preview-pdf-page";

export interface RailEvents {
  onSelectPage(index: number): void;
  onMovePage(from: number, to: number): void;
  onDeletePage(index: number): void;
  onTransferPage(sourceId: string, sourcePage: number, at: number): void;
}

interface PageDragPayload {
  sourceId: string;
  page: number;
}

function isPageDrag(data: DataTransfer | null): boolean {
  return !!data && Array.from(data.types).includes(PAGE_DRAG_TYPE);
}

function readPageDrag(data: DataTransfer | null): PageDragPayload | null {
  if (!data) return null;
  try {
    const payload = JSON.parse(data.getData(PAGE_DRAG_TYPE)) as Partial<PageDragPayload>;
    if (typeof payload.sourceId !== "string" || !Number.isInteger(payload.page) || payload.page! < 0) return null;
    return { sourceId: payload.sourceId, page: payload.page! };
  } catch {
    return null;
  }
}

interface ThumbCacheEntry {
  url: string;
}

export class ThumbnailRail {
  private readonly root: HTMLElement;
  private readonly empty: HTMLElement;
  private readonly events: RailEvents;

  private doc: DocumentInfo | null = null;
  private items: HTMLElement[] = [];
  private images: HTMLImageElement[] = [];
  private cache = new Map<number, ThumbCacheEntry>();
  private visible = new Set<number>();
  private observer: IntersectionObserver | null = null;
  private epoch = 0;
  private dragFrom: number | null = null;
  private dragSourceId: string | null = null;
  private dropSlot: { item: number; before: boolean } | null = null;
  private active = -1;
  private disposed = false;

  constructor(root: HTMLElement, empty: HTMLElement, events: RailEvents) {
    this.root = root;
    this.empty = empty;
    this.events = events;
  }

  setDocument(doc: DocumentInfo): void {
    this.doc = doc;
    this.build();
  }

  update(doc: DocumentInfo, resetImages: boolean): void {
    const rebuild =
      !this.doc ||
      this.doc.id !== doc.id ||
      this.doc.page_count !== doc.page_count ||
      this.doc.kind !== doc.kind;
    this.doc = doc;
    if (rebuild) {
      this.build();
      return;
    }
    if (resetImages) {
      this.resetImages();
    }
    this.updateChrome();
  }

  clear(): void {
    this.doc = null;
    this.observer?.disconnect();
    this.observer = null;
    this.revokeAll();
    this.items = [];
    this.images = [];
    this.visible.clear();
    this.root.replaceChildren();
    this.empty.hidden = false;
  }

  setActive(index: number, scroll = true): void {
    if (this.active === index) {
      if (scroll) this.ensureVisible(index);
      return;
    }
    this.active = index;
    this.items.forEach((item, itemIndex) => {
      item.classList.toggle("active", itemIndex === index);
      item.setAttribute("aria-current", itemIndex === index ? "page" : "false");
    });
    if (scroll) this.ensureVisible(index);
  }

  refresh(): void {
    if (!this.doc) return;
    const rootRect = this.root.getBoundingClientRect();
    for (let index = 0; index < this.items.length; index += 1) {
      const item = this.items[index];
      const rect = item.getBoundingClientRect();
      if (rect.bottom > rootRect.top - 300 && rect.top < rootRect.bottom + 300) {
        this.visible.add(index);
        void this.renderItem(index);
      }
    }
  }

  private build(): void {
    this.observer?.disconnect();
    this.observer = null;
    this.revokeAll();
    this.items = [];
    this.images = [];
    this.visible.clear();
    this.active = -1;
    this.root.replaceChildren();
    this.epoch += 1;

    if (!this.doc) {
      this.empty.hidden = false;
      return;
    }
    this.empty.hidden = true;

    const isPdf = this.doc.kind === "pdf";
    for (let index = 0; index < this.doc.pages.length; index += 1) {
      const page = this.doc.pages[index];
      const item = document.createElement("div");
      item.className = "thumb";
      item.dataset.index = String(index);
      item.setAttribute("role", "button");
      item.setAttribute("tabindex", "0");
      item.setAttribute("aria-label", `Page ${index + 1}`);
      item.setAttribute("aria-current", index === 0 ? "page" : "false");
      const canDragPage = isPdf && !this.doc.read_only;
      if (canDragPage) {
        item.draggable = true;
      }
      if (this.doc.kind === "image" && this.doc.image?.has_alpha) {
        item.classList.add("has-alpha");
      }

      const frame = document.createElement("span");
      frame.className = "thumb-frame";
      const skeleton = document.createElement("span");
      skeleton.className = "thumb-skeleton";
      const image = document.createElement("img");
      image.alt = "";
      image.draggable = false;
      image.addEventListener("load", () => item.classList.add("is-loaded"));
      image.addEventListener("error", () => item.classList.add("is-error"));
      frame.append(skeleton, image);

      const meta = document.createElement("span");
      meta.className = "thumb-meta";
      const label = document.createElement("span");
      label.className = "thumb-label";
      label.textContent = isPdf ? String(index + 1) : "Image";
      meta.append(label);

      if (isPdf && this.doc.pages.length > 1 && !this.doc.read_only) {
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "thumb-remove";
        remove.title = `Delete page ${index + 1}`;
        remove.setAttribute("aria-label", `Delete page ${index + 1}`);
        remove.innerHTML = '<svg class="icon"><use href="#i-trash"/></svg>';
        remove.addEventListener("click", (event) => {
          event.stopPropagation();
          this.events.onDeletePage(index);
        });
        meta.append(remove);
      }

      item.append(frame, meta);
      item.addEventListener("click", () => this.events.onSelectPage(index));
      item.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          this.events.onSelectPage(index);
        }
        if (event.key === "Delete" || event.key === "Backspace") {
          if (isPdf && this.doc && this.doc.pages.length > 1 && !this.doc.read_only) {
            event.preventDefault();
            this.events.onDeletePage(index);
          }
        }
      });

      if (canDragPage) {
        item.addEventListener("dragstart", (event) => {
          this.dragFrom = index;
          this.dragSourceId = this.doc?.id ?? null;
          item.classList.add("dragging");
          if (event.dataTransfer) {
            event.dataTransfer.effectAllowed = this.doc && this.doc.pages.length > 1 ? "copyMove" : "copy";
            event.dataTransfer.setData("text/plain", String(index));
            if (this.doc) {
              event.dataTransfer.setData(PAGE_DRAG_TYPE, JSON.stringify({ sourceId: this.doc.id, page: index }));
            }
          }
        });
        item.addEventListener("dragend", () => {
          this.dragFrom = null;
          this.dragSourceId = null;
          this.clearDropMarkers();
        });
      }
      if (canDragPage) {
        item.addEventListener("dragover", (event) => {
          const hasPageDrag = isPageDrag(event.dataTransfer);
          const isCrossDocumentDrag = hasPageDrag && this.dragSourceId !== null && this.dragSourceId !== this.doc?.id;
          const externalPageDrag = hasPageDrag && this.dragSourceId === null;
          if (isCrossDocumentDrag || externalPageDrag) {
            if (this.doc?.kind !== "pdf") return;
            event.preventDefault();
            const rect = item.getBoundingClientRect();
            const before = event.clientY < rect.top + rect.height / 2;
            this.clearDropMarkers();
            item.classList.add(before ? "drop-before" : "drop-after");
            this.dropSlot = { item: index, before };
            if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
            return;
          }
          if (this.dragFrom === null || this.dragSourceId !== this.doc?.id || this.doc.pages.length < 2) return;
          event.preventDefault();
          const rect = item.getBoundingClientRect();
          const before = event.clientY < rect.top + rect.height / 2;
          this.clearDropMarkers();
          item.classList.add(before ? "drop-before" : "drop-after");
          this.dropSlot = { item: index, before };
          if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
        });
        item.addEventListener("drop", (event) => {
          const payload = readPageDrag(event.dataTransfer);
          if (payload && payload.sourceId !== this.doc?.id && this.doc?.kind === "pdf") {
            event.preventDefault();
            const slot = this.dropSlot;
            const at = slot ? slot.item + (slot.before ? 0 : 1) : index;
            this.dropSlot = null;
            this.clearDropMarkers();
            this.dragFrom = null;
            this.dragSourceId = null;
            this.events.onTransferPage(payload.sourceId, payload.page, at);
            return;
          }
          if (this.dragFrom === null || this.dragSourceId !== this.doc?.id) return;
          event.preventDefault();
          this.finishDrag();
        });
      }

      this.items.push(item);
      this.images.push(image);
      this.root.append(item);
      if (!page) break;
    }

    this.observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const index = Number((entry.target as HTMLElement).dataset.index);
          if (Number.isNaN(index)) continue;
          if (entry.isIntersecting) {
            this.visible.add(index);
            void this.renderItem(index);
          } else {
            this.visible.delete(index);
          }
        }
        this.trimCache();
      },
      { root: this.root, rootMargin: "400px 0px" },
    );
    for (const item of this.items) this.observer.observe(item);

    this.setActive(0, false);
    this.refresh();
  }

  private finishDrag(): void {
    const from = this.dragFrom;
    const slot = this.dropSlot;
    this.dragFrom = null;
    this.dropSlot = null;
    this.clearDropMarkers();
    if (from === null || !slot || !this.doc) return;
    const insert = slot.item + (slot.before ? 0 : 1);
    const to = insert > from ? insert - 1 : insert;
    if (to !== from) this.events.onMovePage(from, to);
  }

  private clearDropMarkers(): void {
    for (const item of this.items) {
      item.classList.remove("drop-before", "drop-after", "dragging");
    }
  }

  private ensureVisible(index: number): void {
    const item = this.items[index];
    if (!item) return;
    const rootRect = this.root.getBoundingClientRect();
    const rect = item.getBoundingClientRect();
    if (rect.top < rootRect.top + 8) {
      this.root.scrollTop -= rootRect.top + 8 - rect.top;
    } else if (rect.bottom > rootRect.bottom - 8) {
      this.root.scrollTop += rect.bottom - rootRect.bottom + 8;
    }
  }

  private updateChrome(): void {
    if (!this.doc) return;
    const isPdf = this.doc.kind === "pdf";
    this.items.forEach((item, index) => {
      const label = item.querySelector<HTMLElement>(".thumb-label");
      if (label) label.textContent = isPdf ? String(index + 1) : "Image";
      const remove = item.querySelector<HTMLElement>(".thumb-remove");
      if (remove) {
        const title = `Delete page ${index + 1}`;
        remove.setAttribute("title", title);
        remove.setAttribute("aria-label", title);
      }
    });
  }

  private resetImages(): void {
    this.epoch += 1;
    this.revokeAll();
    this.root.classList.add("refreshing");
    for (const item of this.items) {
      item.classList.remove("is-loaded", "is-error");
    }
    for (const image of this.images) image.removeAttribute("src");
    window.setTimeout(() => this.root.classList.remove("refreshing"), 160);
    this.refresh();
  }

  private async renderItem(index: number): Promise<void> {
    const doc = this.doc;
    if (!doc) return;
    if (this.cache.has(index)) return;
    const image = this.images[index];
    if (!image) return;
    const epoch = this.epoch;
    try {
      const targetWidth = 132;
      const blob = await Viewer.renderThumbnail(doc, index, targetWidth);
      if (this.disposed || this.epoch !== epoch || this.doc?.id !== doc.id) return;
      const url = URL.createObjectURL(blob);
      this.cache.set(index, { url });
      image.src = url;
      this.trimCache();
    } catch {
      this.items[index]?.classList.add("is-error");
    }
  }

  private trimCache(): void {
    if (this.cache.size <= 120) return;
    for (const key of Array.from(this.cache.keys())) {
      if (this.cache.size <= 120) break;
      if (!this.visible.has(key)) {
        const entry = this.cache.get(key);
        if (entry) URL.revokeObjectURL(entry.url);
        this.cache.delete(key);
        this.images[key]?.removeAttribute("src");
      }
    }
  }

  private revokeAll(): void {
    for (const entry of this.cache.values()) {
      URL.revokeObjectURL(entry.url);
    }
    this.cache.clear();
  }

  destroy(): void {
    this.disposed = true;
    this.observer?.disconnect();
    this.revokeAll();
  }
}
