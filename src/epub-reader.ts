import ePub, { type Book, type Location, type NavItem, type Rendition } from "epubjs";

export interface EpubOutlineEntry {
  title: string;
  href: string;
  depth: number;
}

export interface EpubReaderEvents {
  onOutline(entries: EpubOutlineEntry[]): void;
  onPosition(index: number, total: number, chapter: string, atStart: boolean, atEnd: boolean): void;
  onZoom(percent: number): void;
}

function flattenToc(items: NavItem[], depth = 0, output: EpubOutlineEntry[] = []): EpubOutlineEntry[] {
  for (const item of items) {
    if (output.length >= 1500) break;
    if (item.href && item.label?.trim()) {
      output.push({ title: item.label.trim(), href: item.href, depth });
    }
    if (item.subitems?.length) flattenToc(item.subitems, depth + 1, output);
  }
  return output;
}

export class EpubReader {
  readonly root: HTMLElement;
  private readonly events: EpubReaderEvents;
  private book: Book | null = null;
  private rendition: Rendition | null = null;
  private generation = 0;
  private current = 0;
  private total = 1;
  private zoom = 100;
  private outline: EpubOutlineEntry[] = [];
  private readonly resizeObserver: ResizeObserver;
  private displayed = false;

  constructor(root: HTMLElement, events: EpubReaderEvents) {
    this.root = root;
    this.events = events;
    this.root.tabIndex = 0;
    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(this.root);
  }

  get currentPage(): number {
    return this.current;
  }

  get pageCount(): number {
    return this.total;
  }

  resize(): void {
    if (!this.displayed) return;
    const width = this.root.clientWidth;
    const height = this.root.clientHeight;
    if (width > 0 && height > 0) this.rendition?.resize(width, height);
  }

  destroy(): void {
    this.clear();
    this.resizeObserver.disconnect();
  }

  clear(): void {
    this.generation += 1;
    const book = this.book;
    this.book = null;
    this.rendition = null;
    this.current = 0;
    this.total = 1;
    this.outline = [];
    this.displayed = false;
    try {
      book?.destroy();
    } catch {
      // epub.js may already have torn down an in-flight book.
    }
    this.root.replaceChildren();
    this.root.hidden = true;
  }

  async open(data: ArrayBuffer, initialPage = 0): Promise<void> {
    this.clear();
    const generation = this.generation;
    this.root.hidden = false;
    this.root.replaceChildren();

    const book = ePub(data, { replacements: "blobUrl" });
    this.book = book;
    await book.ready;
    if (generation !== this.generation) return;

    const navigation = await book.loaded.navigation;
    this.outline = flattenToc(navigation.toc);
    this.events.onOutline(this.outline);

    const rendition = book.renderTo(this.root, {
      width: Math.max(1, this.root.clientWidth),
      height: Math.max(1, this.root.clientHeight),
      flow: "paginated",
      layout: "reflowable",
      spread: "none",
      manager: "default",
      minSpreadWidth: 1200,
      allowScriptedContent: false,
    });
    this.rendition = rendition;
    rendition.themes.register("preview-reader", {
      body: {
        "font-family": "Georgia, 'Times New Roman', serif",
        "line-height": "1.65",
      },
      img: {
        "max-width": "100%",
        height: "auto",
      },
    });
    rendition.themes.select("preview-reader");
    rendition.on("relocated", (location: Location) => {
      if (generation === this.generation) this.onRelocated(location);
    });
    rendition.on("keydown", (event: KeyboardEvent) => this.onKeyDown(event));

    await book.locations.generate(1200);
    if (generation !== this.generation) return;
    this.total = Math.max(1, book.locations.length());
    this.current = Math.max(0, Math.min(this.total - 1, Math.trunc(initialPage)));
    this.events.onPosition(this.current, this.total, "", this.current <= 0, false);
    const cfi = book.locations.cfiFromLocation(this.current);
    await rendition.display(cfi === "-1" ? undefined : cfi);
    if (generation === this.generation) {
      this.displayed = true;
      this.resize();
      rendition.themes.fontSize(`${this.zoom}%`);
      this.events.onZoom(this.zoom);
    }
  }

  async goToPage(index: number): Promise<void> {
    if (!this.book || !this.rendition || this.total < 1) return;
    const page = Math.max(0, Math.min(this.total - 1, Math.trunc(index)));
    const cfi = this.book.locations.cfiFromLocation(page);
    if (cfi !== "-1") await this.rendition.display(cfi);
  }

  async turnPage(direction: -1 | 1): Promise<void> {
    if (!this.rendition) return;
    if (direction > 0) await this.rendition.next();
    else await this.rendition.prev();
  }

  async displayHref(href: string): Promise<void> {
    if (!this.rendition || !href) return;
    await this.rendition.display(href);
  }

  zoomBy(delta: number): void {
    this.setZoom(this.zoom + delta);
  }

  setZoom(percent: number): void {
    this.zoom = Math.max(70, Math.min(180, Math.round(percent / 10) * 10));
    this.rendition?.themes.fontSize(`${this.zoom}%`);
    this.events.onZoom(this.zoom);
  }

  resetZoom(): void {
    this.setZoom(100);
  }

  private onRelocated(location: Location): void {
    const book = this.book;
    if (!book) return;
    const position = Number(book.locations.locationFromCfi(location.start.cfi));
    if (position >= 0) this.current = Math.max(0, Math.min(this.total - 1, position));
    const chapter = this.outline.find((item) => item.href.split("#", 1)[0] === location.start.href.split("#", 1)[0])?.title ?? "";
    this.events.onPosition(this.current, this.total, chapter, location.atStart, location.atEnd);
  }

  private onKeyDown(event: KeyboardEvent): void {
    const command = event.ctrlKey || event.metaKey;
    if (event.key === "Escape" || (command && event.key.toLowerCase() === "w")) {
      event.preventDefault();
      document.dispatchEvent(new KeyboardEvent("keydown", {
        key: event.key,
        ctrlKey: event.ctrlKey,
        metaKey: event.metaKey,
        bubbles: true,
        cancelable: true,
      }));
      return;
    }
    if (!command && !event.altKey && (event.key === "PageDown" || event.key === "ArrowRight" || event.key === " ")) {
      event.preventDefault();
      void this.turnPage(1);
    } else if (!command && !event.altKey && (event.key === "PageUp" || event.key === "ArrowLeft")) {
      event.preventDefault();
      void this.turnPage(-1);
    }
  }
}
