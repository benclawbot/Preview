import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";

import { api, errorMessage } from "./api";
import { EpubReader, type EpubOutlineEntry } from "./epub-reader";
import { ThumbnailRail } from "./rail";
import type {
  Annotation,
  ApplyResult,
  BookmarkInfo,
  DocumentInfo,
  FormFieldInfo,
  MetadataReport,
  Operation,
  Point,
  SaveOptions,
  TextMatch,
  Tool,
} from "./types";
import { askPassword, askText, byId, choose, toast } from "./ui";
import { Viewer } from "./viewer";

const OPEN_FILTERS = [
  { name: "PDF, EPUB and images", extensions: ["pdf", "epub", "png", "jpg", "jpeg", "tif", "tiff", "gif", "heic", "heif", "webp", "bmp", "svg", "cr2", "nef", "arw", "dng"] },
  { name: "EPUB book", extensions: ["epub"] },
  { name: "PDF", extensions: ["pdf"] },
  { name: "Images", extensions: ["png", "jpg", "jpeg", "tif", "tiff", "gif", "heic", "heif", "webp", "bmp", "svg", "cr2", "nef", "arw", "dng"] },
];

const SWATCHES = ["#e03131", "#f08c00", "#b08900", "#2f9e44", "#1971c2", "#7048e8", "#343a40"];
const PAGE_DRAG_TYPE = "application/x-preview-pdf-page";
const SIGNATURE_STORAGE_KEY = "preview.saved-signatures.v1";
const MAX_SAVED_SIGNATURES = 12;

interface SavedSignature {
  id: string;
  name: string;
  png: string;
}

function readSavedSignatures(): SavedSignature[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(SIGNATURE_STORAGE_KEY) ?? "[]");
    if (!Array.isArray(value)) return [];
    return value.slice(0, MAX_SAVED_SIGNATURES).filter((item): item is SavedSignature =>
      !!item && typeof item === "object"
      && typeof item.id === "string"
      && typeof item.name === "string"
      && typeof item.png === "string"
      && item.png.length <= 1_500_000
      && /^data:image\/png;base64,[A-Za-z0-9+/]+=*$/.test(item.png),
    );
  } catch {
    return [];
  }
}

interface PageTransferPayload {
  sourceId: string;
  page: number;
}

function readPageTransfer(data: DataTransfer | null): PageTransferPayload | null {
  if (!data) return null;
  try {
    const payload = JSON.parse(data.getData(PAGE_DRAG_TYPE)) as Partial<PageTransferPayload>;
    if (typeof payload.sourceId !== "string" || !Number.isInteger(payload.page) || payload.page! < 0) return null;
    return { sourceId: payload.sourceId, page: payload.page! };
  } catch {
    return null;
  }
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.isContentEditable || !!target.closest("input, textarea, select, [contenteditable='true']");
}

function basename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function splitVCardValue(value: string, separator: string): string[] {
  const parts: string[] = [];
  let current = "";
  let escaped = false;
  for (const character of value) {
    if (escaped) {
      current += `\\${character}`;
      escaped = false;
    } else if (character === "\\") {
      escaped = true;
    } else if (character === separator) {
      parts.push(current);
      current = "";
    } else {
      current += character;
    }
  }
  if (escaped) current += "\\\\";
  parts.push(current);
  return parts.map((part) => part.replace(/\\([nN,;\\])/g, (_match, escapedChar: string) => {
    if (escapedChar === "n" || escapedChar === "N") return "\\n";
    return escapedChar;
  }).replace(/\\n/g, "\n").trim());
}

function parseVCard(raw: string): Record<string, string[]> {
  const lines = raw.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");
  const unfolded: string[] = [];
  for (const line of lines) {
    if (/^[ \t]/.test(line) && unfolded.length) unfolded[unfolded.length - 1] += line.slice(1);
    else unfolded.push(line);
  }
  const properties: Record<string, string[]> = {};
  let insideCard = false;
  for (const line of unfolded) {
    if (/^BEGIN:VCARD$/i.test(line.trim())) {
      if (insideCard) break;
      insideCard = true;
      continue;
    }
    if (!insideCard) continue;
    if (/^END:VCARD$/i.test(line.trim())) break;
    const separator = line.indexOf(":");
    if (separator < 1) continue;
    const [qualifiedName = ""] = line.slice(0, separator).split(";");
    if (/ENCODING\s*=\s*(?:b|base64)/i.test(line.slice(0, separator))) continue;
    const key = qualifiedName.split(".").pop()?.toUpperCase();
    if (!key) continue;
    const values = splitVCardValue(line.slice(separator + 1), key === "N" || key === "ADR" ? ";" : ",");
    if (key === "N" || key === "ADR") properties[key] = values;
    else (properties[key] ??= []).push(...values);
  }
  return properties;
}

function contactValueForField(field: FormFieldInfo, contact: Record<string, string[]>): string | null {
  const name = field.name.split(/[.[\\]]/).filter(Boolean).pop()?.toLowerCase().replace(/[^a-z0-9]/g, "") ?? "";
  const first = contact.N?.[1] || "";
  const last = contact.N?.[0] || "";
  const email = contact.EMAIL?.[0] || "";
  const phone = contact.TEL?.[0] || "";
  const org = contact.ORG?.[0] || "";
  const title = contact.TITLE?.[0] || "";
  const fn = contact.FN?.[0] || [first, last].filter(Boolean).join(" ");
  const address = contact.ADR ?? [];
  const mapping: Record<string, string> = {
    first: first, firstname: first, given: first, givenname: first, fname: first,
    last: last, lastname: last, surname: last, familyname: last, lname: last,
    name: fn, fullname: fn, contactname: fn, formattedname: fn,
    email, emailaddress: email, mail: email,
    phone, telephone: phone, mobilenumber: phone, mobile: phone, workphone: phone,
    company: org, organization: org, organisation: org, org,
    title, jobtitle: title, role: title,
    street: address[2] || "", address: address.filter(Boolean).join(", "),
    city: address[3] || "", locality: address[3] || "",
    state: address[4] || "", region: address[4] || "",
    zip: address[5] || "", zipcode: address[5] || "", postalcode: address[5] || "",
    country: address[6] || "", website: contact.URL?.[0] || "", url: contact.URL?.[0] || "",
    birthday: contact.BDAY?.[0] || "", dob: contact.BDAY?.[0] || "",
  };
  const value = mapping[name];
  if (!value) return null;
  if (["button", "pushbutton", "signature", "radio"].includes(field.kind)) return null;
  if (field.kind === "choice" && !field.options.includes(value)) return null;
  return value;
}

export class PreviewApp {
  private readonly viewer: Viewer;
  private readonly rail: ThumbnailRail;
  private readonly epubReader: EpubReader;
  private document: DocumentInfo | null = null;
  private readonly documents = new Map<string, DocumentInfo>();
  private readonly lastPages = new Map<string, number>();
  private sidebarDocumentKind: DocumentInfo["kind"] | null = null;
  private dragHoverDocumentId: string | null = null;
  private metadata: MetadataReport | null = null;
  private findMatches: TextMatch[] = [];
  private findIndex = -1;
  private findTimer = 0;
  private findRequest = 0;
  private opening = false;
  private operating = false;
  private closingWindow = false;
  private dropUnlisten: (() => void) | null = null;
  private selected: Annotation | null = null;
  private bookmarks: BookmarkInfo[] = [];
  private epubChapter = "";
  private slideshowTimer = 0;
  private contactObserver: IntersectionObserver | null = null;
  private contactUrls: string[] = [];
  private signatureStream: MediaStream | null = null;
  private signatureDrawing = false;
  private signatureLast: { x: number; y: number } | null = null;
  private curvePoints: Point[] = [{ x: 0, y: 0 }, { x: 1, y: 1 }];
  private curveDragIndex = -1;
  private imageAdjustmentsOpen = false;
  private imageAdjustmentsDocumentId: string | null = null;
  private inspectorHiddenBeforeAdjustments = false;

  constructor() {
    const viewerRoot = byId("viewer");
    this.viewer = new Viewer(viewerRoot, {
      onPageChange: (index) => this.pageChanged(index),
      onZoomChange: (percent) => this.zoomChanged(percent),
      onSelect: (annotation) => this.selectionChanged(annotation),
      onCreate: (annotation) => void this.createAnnotation(annotation),
      onEdit: (before, after) => void this.editAnnotation(before, after),
      onEditText: (annotation) => void this.editAnnotationText(annotation),
      onCropChange: (crop) => this.cropChanged(crop),
      onRedact: (page, rect) => void this.redactArea(page, rect),
    });
    this.epubReader = new EpubReader(byId("epub-reader"), {
      onOutline: (entries) => this.renderEpubOutline(entries),
      onPosition: (index, total, chapter) => this.epubPositionChanged(index, total, chapter),
      onZoom: (percent) => this.zoomChanged(percent),
    });
    this.rail = new ThumbnailRail(byId("thumb-list"), byId("sidebar-empty"), {
      onSelectPage: (index) => this.goToPage(index),
      onMovePage: (from, to) => void this.applyOperation({ op: "reorder_pages", from, to }, true),
      onDeletePage: (index) => void this.deletePage(index),
      onTransferPage: (sourceId, sourcePage, at) => {
        const destinationId = this.document?.id;
        if (destinationId) void this.receivePageTransfer(destinationId, { sourceId, page: sourcePage }, at);
      },
    });
    this.wireEvents();
    this.installDragDrop();
    this.renderSwatches("stroke-swatches", "stroke");
    this.renderSwatches("fill-swatches", "fill");
    this.setDocumentUI(null);
  }

  async start(): Promise<void> {
    try {
      const initialPath = await api.initialDocumentPath();
      if (initialPath) await this.openPath(initialPath);
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  }

  private wireEvents(): void {
    byId("btn-open").addEventListener("click", () => void this.pickAndOpen());
    byId("btn-welcome-open").addEventListener("click", () => void this.pickAndOpen());
    byId("btn-save").addEventListener("click", () => void this.saveCurrent());
    byId("btn-export").addEventListener("click", () => void this.showExport());
    byId("btn-undo").addEventListener("click", () => void this.undo());
    byId("btn-redo").addEventListener("click", () => void this.redo());
    byId("btn-zoom-in").addEventListener("click", () => this.zoomIn());
    byId("btn-zoom-out").addEventListener("click", () => this.zoomOut());
    byId("btn-fit-width").addEventListener("click", () => this.fitWidth());
    byId("btn-fit-page").addEventListener("click", () => this.fitPage());
    byId("zoom-label").addEventListener("click", () => this.toggleMenu("zoom-menu", "zoom-label"));
    byId("btn-prev-page").addEventListener("click", () => this.goToPage(this.currentPageNumber() - 1));
    byId("btn-next-page").addEventListener("click", () => this.goToPage(this.currentPageNumber() + 1));
    byId<HTMLInputElement>("page-number").addEventListener("change", (event) => {
      const input = event.currentTarget as HTMLInputElement;
      const page = Number(input.value);
      if (Number.isFinite(page)) this.goToPage(page - 1);
      input.value = String(this.currentPageNumber() + 1);
    });
    byId("btn-sidebar").addEventListener("click", () => document.body.classList.toggle("sidebar-hidden"));
    byId("btn-inspector").addEventListener("click", () => {
      if (this.imageAdjustmentsOpen) {
        this.closeImageAdjustments(false);
        document.body.classList.add("inspector-hidden");
      }
      else document.body.classList.toggle("inspector-hidden");
    });
    byId("btn-inspector-close").addEventListener("click", () => {
      if (this.imageAdjustmentsOpen) {
        this.closeImageAdjustments(false);
        document.body.classList.add("inspector-hidden");
      }
      else document.body.classList.add("inspector-hidden");
    });
    byId("btn-find").addEventListener("click", () => this.openFind());
    byId("btn-contact-sheet").addEventListener("click", () => void this.openContactSheet());
    byId("btn-fullscreen").addEventListener("click", () => void this.toggleFullscreen());
    byId("btn-slideshow").addEventListener("click", () => void this.toggleSlideshow());
    byId("tab-pages").addEventListener("click", () => this.showSidebarTab("pages"));
    byId("tab-outline").addEventListener("click", () => this.showSidebarTab("outline"));
    byId("find-close").addEventListener("click", () => this.closeFind());
    byId<HTMLInputElement>("find-input").addEventListener("input", () => this.scheduleFind());
    byId<HTMLInputElement>("find-input").addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        this.stepFind(event.shiftKey ? -1 : 1);
      }
      if (event.key === "Escape") this.closeFind();
    });
    byId("find-next").addEventListener("click", () => this.stepFind(1));
    byId("find-prev").addEventListener("click", () => this.stepFind(-1));

    for (const button of Array.from(document.querySelectorAll<HTMLButtonElement>(".markup-tools [data-tool], #btn-crop"))) {
      button.addEventListener("click", () => this.activateTool(button.dataset.tool as Tool));
    }
    byId("btn-rotate-left").addEventListener("click", () => void this.rotatePage(-90));
    byId("btn-rotate-right").addEventListener("click", () => void this.rotatePage(90));
    byId("btn-delete-page").addEventListener("click", () => void this.deletePage(this.currentPageNumber()));
    byId("btn-crop-apply").addEventListener("click", () => void this.applyCrop());
    byId("btn-crop-cancel").addEventListener("click", () => this.viewer.clearCrop());
    byId("btn-resize").addEventListener("click", () => this.showResize());
    byId("btn-image-rotate-left").addEventListener("click", () => void this.applyOperation({ op: "rotate_image", degrees: -90 }, true));
    byId("btn-image-rotate-right").addEventListener("click", () => void this.applyOperation({ op: "rotate_image", degrees: 90 }, true));
    byId("btn-flip-h").addEventListener("click", () => void this.applyOperation({ op: "flip_image", horizontal: true }, true));
    byId("btn-flip-v").addEventListener("click", () => void this.applyOperation({ op: "flip_image", horizontal: false }, true));
    byId("btn-image-adjust").addEventListener("click", () => void this.showImageAdjustments());
    byId("btn-instant-alpha").addEventListener("click", () => this.openInstantAlpha());
    byId("btn-signature").addEventListener("click", () => this.openSignatureDialog());

    byId("btn-delete-annotation").addEventListener("click", () => void this.removeSelected());
    byId<HTMLInputElement>("stroke-width").addEventListener("change", () => this.updateSelected({ width: Number(byId<HTMLInputElement>("stroke-width").value) }));
    byId<HTMLInputElement>("opacity").addEventListener("change", () => this.updateSelected({ opacity: Number(byId<HTMLInputElement>("opacity").value) }));
    byId<HTMLInputElement>("font-size").addEventListener("change", () => this.updateSelected({ font_size: Number(byId<HTMLInputElement>("font-size").value) }));
    byId<HTMLTextAreaElement>("annotation-text").addEventListener("change", () => this.updateSelected({ contents: byId<HTMLTextAreaElement>("annotation-text").value || null }));
    byId("btn-bold").addEventListener("click", () => this.updateSelected({ bold: !this.selected?.bold }));
    byId("btn-italic").addEventListener("click", () => this.updateSelected({ italic: !this.selected?.italic }));

    byId("btn-menu").addEventListener("click", () => this.toggleMenu("app-menu", "btn-menu"));
    byId("menu-revert").addEventListener("click", () => void this.revert());
    byId("menu-close").addEventListener("click", () => void this.closeDocument());
    byId("menu-insert-pages").addEventListener("click", () => void this.insertPagesFromPdf());
    byId("menu-merge-pdfs").addEventListener("click", () => void this.mergePdfs());
    byId("menu-extract-pages").addEventListener("click", () => this.openExtractDialog());
    byId("menu-batch-resize").addEventListener("click", () => void this.batchResizeImages());
    byId("menu-print").addEventListener("click", () => { this.hideMenus(); window.print(); });
    byId("menu-protect").addEventListener("click", () => this.openProtectDialog());
    byId("menu-unlock").addEventListener("click", () => void this.exportUnlockedPdf());
    byId("menu-redact").addEventListener("click", () => {
      this.hideMenus();
      this.activateTool("redact");
    });
    byId("menu-autofill").addEventListener("click", () => void this.autoFillFromContact());
    byId("menu-ocr").addEventListener("click", () => void this.runOcr());
    byId("menu-shortcuts").addEventListener("click", () => {
      byId<HTMLDialogElement>("shortcuts-dialog").showModal();
      this.hideMenus();
    });
    for (const button of Array.from(document.querySelectorAll<HTMLButtonElement>("#zoom-menu [data-zoom]"))) {
      button.addEventListener("click", () => {
        const zoom = button.dataset.zoom;
        if (zoom === "fit-width") this.fitWidth();
        else if (zoom === "fit-page") this.fitPage();
        else if (zoom) this.setZoom(Number(zoom) * 100);
        this.hideMenus();
      });
    }
    document.addEventListener("pointerdown", (event) => {
      if (!(event.target instanceof HTMLElement)) return;
      if (!event.target.closest(".menu, #btn-menu, #zoom-label")) this.hideMenus();
    });
    document.addEventListener("keydown", (event) => this.onKeyDown(event));
    document.addEventListener("click", (event) => {
      const target = event.target;
      if (target instanceof HTMLElement && target.matches("[data-dialog-cancel]")) {
        target.closest("dialog")?.close();
      }
    });

    byId<HTMLFormElement>("export-form").addEventListener("submit", (event) => {
      event.preventDefault();
      void this.exportFromDialog();
    });
    byId<HTMLSelectElement>("export-format").addEventListener("change", () => this.updateExportFields());
    byId<HTMLInputElement>("export-quality").addEventListener("input", () => {
      byId("export-quality-value").textContent = byId<HTMLInputElement>("export-quality").value;
    });

    byId<HTMLInputElement>("resize-width").addEventListener("input", () => this.onResizeWidth());
    byId<HTMLInputElement>("resize-height").addEventListener("input", () => this.onResizeHeight());
    for (const preset of Array.from(document.querySelectorAll<HTMLButtonElement>("#resize-presets [data-scale]"))) {
      preset.addEventListener("click", () => this.applyResizePreset(Number(preset.dataset.scale)));
    }
    byId<HTMLFormElement>("resize-form").addEventListener("submit", (event) => {
      event.preventDefault();
      const width = Math.round(Number(byId<HTMLInputElement>("resize-width").value));
      const height = Math.round(Number(byId<HTMLInputElement>("resize-height").value));
      if (width < 1 || height < 1 || width > 20000 || height > 20000) {
        toast("Choose dimensions between 1 and 20,000 pixels.", "error");
        return;
      }
      byId<HTMLDialogElement>("resize-dialog").close();
      void this.applyOperation({ op: "resize_image", width, height }, true);
    });
    byId<HTMLFormElement>("image-adjust-form").addEventListener("submit", (event) => {
      event.preventDefault();
      void this.applyImageAdjustments();
    });
    byId("image-adjust-cancel").addEventListener("click", () => this.closeImageAdjustments());
    byId<HTMLFormElement>("instant-alpha-form").addEventListener("submit", (event) => {
      event.preventDefault();
      const color = byId<HTMLInputElement>("instant-alpha-color").value;
      const tolerance = Number(byId<HTMLInputElement>("instant-alpha-tolerance").value);
      byId<HTMLDialogElement>("instant-alpha-dialog").close();
      void this.applyOperation({ op: "instant_alpha", color, tolerance }, true);
    });
    byId<HTMLInputElement>("instant-alpha-tolerance").addEventListener("input", (event) => {
      byId("instant-alpha-value").textContent = (event.currentTarget as HTMLInputElement).value;
    });
    for (const id of ["adjust-brightness", "adjust-contrast", "adjust-saturation", "adjust-sharpen", "levels-black", "levels-white", "levels-gamma"]) {
      byId<HTMLInputElement>(id).addEventListener("input", () => this.previewImageAdjustments());
    }
    this.wireCurveEditor();
    byId("curve-reset").addEventListener("click", () => {
      this.curvePoints = [{ x: 0, y: 0 }, { x: 1, y: 1 }];
      this.drawCurve();
      this.previewImageAdjustments();
    });
    byId<HTMLFormElement>("extract-form").addEventListener("submit", (event) => {
      event.preventDefault();
      void this.extractPages();
    });
    byId<HTMLFormElement>("protect-form").addEventListener("submit", (event) => {
      event.preventDefault();
      void this.protectPdf();
    });
    this.wireSignatureCanvas();
    byId("signature-clear").addEventListener("click", () => this.clearSignatureCanvas());
    byId<HTMLInputElement>("signature-store").addEventListener("change", (event) => {
      byId("signature-name-row").hidden = !(event.currentTarget as HTMLInputElement).checked;
    });
    byId<HTMLInputElement>("signature-file").addEventListener("change", (event) => {
      const file = (event.currentTarget as HTMLInputElement).files?.[0];
      if (file) void this.loadSignatureFile(file);
    });
    byId("signature-camera").addEventListener("click", () => void this.startSignatureCamera());
    byId("signature-capture").addEventListener("click", () => this.captureSignatureCamera());
    byId<HTMLFormElement>("signature-form").addEventListener("submit", (event) => {
      event.preventDefault();
      void this.placeSignature();
    });
    byId<HTMLDialogElement>("signature-dialog").addEventListener("close", () => this.stopSignatureCamera());
    byId<HTMLDialogElement>("contact-sheet-dialog").addEventListener("close", () => this.closeContactSheet());
    window.addEventListener("fullscreenchange", () => {
      if (!document.fullscreenElement) this.stopSlideshow();
    });
    window.addEventListener("beforeprint", () => {
      this.viewer.selectAnnotation(null);
      this.viewer.clearCrop();
    });
    window.addEventListener("resize", () => this.viewer.refit());
    window.addEventListener("beforeunload", () => {
      this.dropUnlisten?.();
      this.viewer.destroy();
      this.rail.destroy();
      this.epubReader.destroy();
    }, { once: true });
  }

  private async installDragDrop(): Promise<void> {
    try {
      this.dropUnlisten = await getCurrentWebview().onDragDropEvent((event) => {
        const overlay = byId("drop-overlay");
        if (event.payload.type === "enter" || event.payload.type === "over") {
          overlay.hidden = false;
        } else if (event.payload.type === "leave") {
          overlay.hidden = true;
        } else if (event.payload.type === "drop") {
          overlay.hidden = true;
          const path = event.payload.paths[0];
          if (path) void this.openPath(path);
        }
      });
    } catch {
      // Native file drag-and-drop is an enhancement; opening from the picker still works.
    }
  }

  private async pickAndOpen(): Promise<void> {
    if (this.opening) return;
    try {
      const selected = await open({
        title: "Open in Preview",
        filters: OPEN_FILTERS,
        multiple: false,
      });
      if (typeof selected === "string") await this.openPath(selected);
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  }

  private async openPath(path: string): Promise<void> {
    if (this.opening || this.operating || !path) return;
    const alreadyOpen = Array.from(this.documents.values()).find((doc) => doc.path === path);
    if (alreadyOpen) {
      await this.activateDocument(alreadyOpen.id);
      return;
    }
    this.opening = true;
    const previousDocumentId = this.document?.id ?? null;
    let openedDocument: DocumentInfo | null = null;
    this.setBusy(true, `Opening ${basename(path)}…`);
    try {
      let password: string | null = null;
      let wrongPassword = false;
      let next: DocumentInfo;
      while (true) {
        try {
          next = await api.openDocument(path, password);
          break;
        } catch (error) {
          const message = errorMessage(error);
          if (!message.toLowerCase().includes("password")) throw error;
          password = await askPassword(wrongPassword);
          if (password === null) return;
          wrongPassword = true;
        }
      }
      this.closeFind();
      this.document = next;
      openedDocument = next;
      this.documents.set(next.id, next);
      this.lastPages.set(next.id, 0);
      this.metadata = null;
      await this.showDocumentView(next, 0);
      this.setDocumentUI(next);
      await this.refreshMetadata();
      await this.refreshDocumentPanels(next);
      toast(`Opened ${next.name}`, "success");
    } catch (error) {
      if (openedDocument?.kind === "epub") {
        await api.closeDocument(openedDocument.id).catch(() => undefined);
        this.documents.delete(openedDocument.id);
        this.lastPages.delete(openedDocument.id);
        const previous = previousDocumentId ? this.documents.get(previousDocumentId) ?? null : null;
        this.document = previous;
        this.metadata = null;
        if (previous) {
          await this.showDocumentView(previous, this.lastPages.get(previous.id) ?? 0).catch(() => undefined);
        } else {
          this.epubReader.clear();
          this.viewer.root.hidden = false;
          this.viewer.clearDocument();
          this.rail.clear();
        }
        this.setDocumentUI(previous);
      }
      toast(errorMessage(error), "error");
    } finally {
      this.opening = false;
      this.setBusy(false);
    }
  }

  private async showDocumentView(doc: DocumentInfo, page = 0): Promise<void> {
    if (doc.kind === "epub") {
      this.epubChapter = "";
      this.setDocumentUI(doc);
      this.viewer.clearDocument();
      this.viewer.root.hidden = true;
      this.rail.clear();
      this.epubReader.root.hidden = false;
      const data = await api.getEpubData(doc.id);
      await this.epubReader.open(data, page);
      this.showSidebarTab("outline");
      return;
    }

    this.epubReader.clear();
    this.viewer.root.hidden = false;
    this.viewer.setDocument(doc);
    this.rail.setDocument(doc);
    this.showSidebarTab("pages");
    if (page > 0) this.viewer.goToPage(page);
  }

  private async prepareToReplace(): Promise<boolean> {
    if (!this.document?.dirty) return true;
    const answer = await choose(
      "Unsaved changes",
      `Save changes to “${this.document.name}” before opening another file?`,
      [
        { value: "cancel", label: "Cancel", style: "ghost" },
        { value: "discard", label: "Discard", style: "danger" },
        { value: "save", label: "Save", style: "primary" },
      ],
    );
    if (answer === "save") return this.saveCurrent();
    return answer === "discard";
  }

  private async closeDocument(): Promise<void> {
    if (!this.document || this.opening) return;
    if (!(await this.prepareToReplace())) return;
    const id = this.document.id;
    await api.closeDocument(id).catch(() => undefined);
    this.documents.delete(id);
    this.lastPages.delete(id);
    this.stopSlideshow();
    this.closeContactSheet();
    this.closeFind();
    this.selected = null;
    this.document = null;
    const openIds = Array.from(this.documents.keys());
    const remaining = openIds[openIds.length - 1];
    if (remaining) {
      await this.activateDocument(remaining);
      return;
    }
    this.document = null;
    this.metadata = null;
    this.selected = null;
    this.bookmarks = [];
    this.renderBookmarks();
    this.viewer.clearDocument();
    this.rail.clear();
    this.epubReader.clear();
    this.viewer.root.hidden = false;
    this.closeFind();
    this.setDocumentUI(null);
  }

  private async activateDocument(id: string): Promise<void> {
    if (this.document?.id === id) return;
    if (this.opening || this.operating) return;
    const cached = this.documents.get(id);
    if (!cached) return;
    if (this.document) this.lastPages.set(this.document.id, this.currentPageNumber());
    this.opening = true;
    try {
      const next = await api.getDocumentInfo(id);
      this.documents.set(id, next);
      this.document = next;
      this.metadata = null;
      this.closeFind();
      await this.showDocumentView(next, this.lastPages.get(id) ?? 0);
      this.setDocumentUI(next);
      await this.refreshMetadata();
      await this.refreshDocumentPanels(next);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.opening = false;
    }
  }

  private renderDocumentTabs(): void {
    const host = byId("doc-tabs");
    host.replaceChildren();
    const docs = Array.from(this.documents.values());
    host.hidden = docs.length === 0;
    for (const doc of docs) {
      const tab = document.createElement("div");
      tab.className = "doc-tab";
      tab.classList.toggle("active", doc.id === this.document?.id);
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-selected", doc.id === this.document?.id ? "true" : "false");
      tab.setAttribute("tabindex", doc.id === this.document?.id ? "0" : "-1");
      tab.title = doc.kind === "pdf"
        ? `${doc.path ?? doc.name}\nDrop a page on this tab to copy it after the current page, or on a thumbnail to choose its exact position.`
        : (doc.path ?? doc.name);
      tab.addEventListener("click", () => void this.activateDocument(doc.id));
      tab.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          void this.activateDocument(doc.id);
        }
      });
      tab.addEventListener("dragover", (event) => {
        if (!event.dataTransfer?.types.includes(PAGE_DRAG_TYPE) || doc.kind !== "pdf") return;
        event.preventDefault();
        if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
        tab.classList.add("drag-target");
        if (this.document?.id !== doc.id && this.dragHoverDocumentId !== doc.id) {
          this.dragHoverDocumentId = doc.id;
          void this.activateDocument(doc.id).finally(() => {
            if (this.dragHoverDocumentId === doc.id) this.dragHoverDocumentId = null;
          });
        }
      });
      tab.addEventListener("dragleave", (event) => {
        if (!tab.contains(event.relatedTarget as Node | null)) tab.classList.remove("drag-target");
      });
      tab.addEventListener("drop", (event) => {
        const payload = readPageTransfer(event.dataTransfer);
        if (!payload || payload.sourceId === doc.id || doc.kind !== "pdf") return;
        event.preventDefault();
        tab.classList.remove("drag-target");
        void this.receivePageTransfer(doc.id, payload);
      });

      const name = document.createElement("span");
      name.className = "doc-tab-name";
      name.textContent = doc.name;
      tab.append(name);
      if (doc.dirty) {
        const dirty = document.createElement("span");
        dirty.className = "doc-tab-dirty";
        dirty.title = "Unsaved changes";
        tab.append(dirty);
      }
      const close = document.createElement("button");
      close.type = "button";
      close.className = "doc-tab-close";
      close.setAttribute("aria-label", `Close ${doc.name}`);
      close.title = "Close document";
      close.textContent = "×";
      close.addEventListener("click", (event) => {
        event.stopPropagation();
        if (this.document?.id !== doc.id) {
          void this.activateDocument(doc.id).then(() => {
            if (this.document?.id === doc.id) void this.closeDocument();
          });
        } else {
          void this.closeDocument();
        }
      });
      tab.append(close);
      host.append(tab);
    }
  }

  private async receivePageTransfer(destinationId: string, payload: PageTransferPayload, requestedAt?: number): Promise<void> {
    const source = this.documents.get(payload.sourceId);
    const destination = this.documents.get(destinationId);
    if (!source || source.kind !== "pdf" || !destination || destination.kind !== "pdf") return;
    await this.activateDocument(destinationId);
    if (this.document?.id !== destinationId) return;
    const at = requestedAt === undefined
      ? Math.min(destination.page_count, this.currentPageNumber() + 1)
      : Math.max(0, Math.min(destination.page_count, requestedAt));
    this.setBusy(true, `Copying page ${payload.page + 1}…`);
    try {
      const updated = await api.transferPdfPage(source.id, destinationId, payload.page, at);
      this.documents.set(destinationId, updated);
      this.lastPages.set(destinationId, this.currentPageNumber());
      this.document = updated;
      this.viewer.update(updated, true);
      this.rail.update(updated, true);
      this.setDocumentUI(updated);
      await this.refreshMetadata();
      await this.refreshDocumentPanels(updated);
      toast(`Copied page ${payload.page + 1} from ${source.name} into ${destination.name}. Undo reverses the transfer.`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private async saveCurrent(force = false): Promise<boolean> {
    const doc = this.document;
    if (!doc) return false;
    if (doc.read_only) {
      toast(doc.kind === "epub" ? "EPUB books are read-only in Preview." : "Encrypted PDFs are read-only in this MVP.", "error");
      return false;
    }
    if (!doc.dirty && doc.path) return true;
    let path = doc.path;
    if (!path) {
      const result = await save({ title: "Save document", defaultPath: doc.name, filters: [{ name: "PDF", extensions: ["pdf"] }] });
      if (!result) return false;
      path = result;
    }
    const options: SaveOptions = {
      strip_gps: byId<HTMLInputElement>("strip-gps").checked,
      force,
    };
    this.setBusy(true, "Saving…");
    try {
      await api.saveDocument(doc.id, path, options);
      this.document = await api.getDocumentInfo(doc.id);
      this.viewer.update(this.document, false);
      this.rail.update(this.document, false);
      this.setDocumentUI(this.document);
      await this.refreshMetadata();
      toast("Saved", "success");
      return true;
    } catch (error) {
      const message = errorMessage(error);
      if (!force && message.toLowerCase().includes("digital signature")) {
        const answer = await choose(
          "Signed PDF",
          "Saving edits will invalidate the existing digital signature. Save anyway?",
          [
            { value: "cancel", label: "Cancel", style: "ghost" },
            { value: "force", label: "Save anyway", style: "danger" },
          ],
        );
        return answer === "force" ? this.saveCurrent(true) : false;
      }
      toast(message, "error");
      return false;
    } finally {
      this.setBusy(false);
    }
  }

  private async showExport(): Promise<void> {
    if (!this.document || this.document.kind === "epub") return;
    const select = byId<HTMLSelectElement>("export-format");
    select.replaceChildren();
    const options = this.document.kind === "pdf"
      ? [
          ["png", "PNG image"],
          ["jpeg", "JPEG image"],
        ]
      : [
          ["png", "PNG image"],
          ["jpeg", "JPEG image"],
          ["tiff", "TIFF image"],
          ["gif", "GIF image"],
          ["webp", "WebP image"],
          ["bmp", "BMP image"],
          ["pdf", "PDF document"],
          ["heic", "HEIC image (HEVC encoder required)"],
          ["psd", "Photoshop PSD (flattened RGB)"],
          ["jp2", "JPEG 2000 (JP2)"],
        ];
    for (const [value, label] of options) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = label;
      select.append(option);
    }
    if (this.document.kind === "image") {
      const current = this.document.image?.format.toLowerCase() ?? "png";
      select.value = current === "heif" ? "heic" : current === "jpg" ? "jpeg" : current;
      if (!Array.from(select.options).some((option) => option.value === select.value)) select.value = "png";
    } else {
      select.value = "png";
    }
    byId<HTMLInputElement>("export-page").max = String(this.document.page_count);
    byId<HTMLInputElement>("export-page").value = String(this.currentPageNumber() + 1);
    byId<HTMLInputElement>("export-quality").value = "90";
    byId("export-quality-value").textContent = "90";
    byId<HTMLInputElement>("export-strip-gps").checked = false;
    this.updateExportFields();
    byId<HTMLDialogElement>("export-dialog").showModal();
  }

  private updateExportFields(): void {
    const format = byId<HTMLSelectElement>("export-format").value;
    const pdf = this.document?.kind === "pdf";
    byId("export-page-field").hidden = !pdf;
    byId("export-dpi-field").hidden = !pdf;
    byId("export-quality-field").hidden = !["jpeg", "jpg", "heic"].includes(format);
    byId("export-gps-field").hidden = pdf || !this.metadata?.can_strip_gps;
    byId("export-hint").textContent = pdf
      ? "Exports the selected page. PDF export preserves the whole document with Save As."
      : format === "heic"
        ? "HEIC export requires a compatible system HEVC encoder. HEVC licensing must be reviewed before distribution."
        : format === "jp2"
          ? "Exports a lossless JP2 image. Transparency is flattened onto white."
        : format === "psd"
          ? "PSD export is a flattened RGB image (not an editable layered document)."
        : "Export a copy; the open document remains unchanged.";
  }

  private async exportFromDialog(): Promise<void> {
    const doc = this.document;
    if (!doc) return;
    const format = byId<HTMLSelectElement>("export-format").value;
    const page = Math.max(1, Number(byId<HTMLInputElement>("export-page").value) || 1) - 1;
    const dpi = Number(byId<HTMLSelectElement>("export-dpi").value) || 150;
    const quality = Number(byId<HTMLInputElement>("export-quality").value) || 90;
    const stripGps = byId<HTMLInputElement>("export-strip-gps").checked;
    const defaultName = `${doc.name.replace(/\.[^.]+$/, "")}.${format === "jpeg" ? "jpg" : format}`;
    const result = await save({
      title: "Export document",
      defaultPath: defaultName,
      filters: [{ name: format.toUpperCase(), extensions: [format === "jpeg" ? "jpg" : format] }],
    });
    if (!result) return;
    const options: SaveOptions = { format, quality, strip_gps: stripGps };
    if (doc.kind === "pdf") {
      options.page = page;
      options.dpi = dpi;
    }
    byId<HTMLDialogElement>("export-dialog").close();
    this.setBusy(true, "Exporting…");
    try {
      await api.exportDocument(doc.id, result, options);
      toast(`Exported ${basename(result)}`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private async applyOperation(operation: Operation, resetImages: boolean): Promise<void> {
    const doc = this.document;
    if (!doc || this.operating) return;
    this.operating = true;
    try {
      const result: ApplyResult = await api.applyOp(doc.id, operation);
      const updated = await api.getDocumentInfo(doc.id);
      this.document = updated;
      this.viewer.update(updated, resetImages);
      this.rail.update(updated, resetImages);
      this.setDocumentUI(updated);
      await this.refreshMetadata();
      if (operation.op === "insert_pages" || operation.op === "delete_page" || operation.op === "reorder_pages") {
        await this.refreshDocumentPanels(updated);
      }
      if (doc.kind === "pdf" && ["rotate_page", "delete_page", "reorder_pages"].includes(operation.op)) {
        const query = byId<HTMLInputElement>("find-input").value.trim();
        if (query) void this.runFind(query);
      }
      if (result.changed_pages.length > 0 && !resetImages) this.viewer.redraw();
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.operating = false;
    }
  }

  private async undo(): Promise<void> {
    if (!this.document?.can_undo || this.operating) return;
    await this.runHistoryAction(() => api.undo(this.document!.id));
  }

  private async redo(): Promise<void> {
    if (!this.document?.can_redo || this.operating) return;
    await this.runHistoryAction(() => api.redo(this.document!.id));
  }

  private async runHistoryAction(action: () => Promise<ApplyResult>): Promise<void> {
    if (!this.document) return;
    this.operating = true;
    try {
      await action();
      const updated = await api.getDocumentInfo(this.document.id);
      this.document = updated;
      this.viewer.update(updated, true);
      this.rail.update(updated, true);
      this.setDocumentUI(updated);
      await this.refreshMetadata();
      await this.refreshDocumentPanels(updated);
      const query = byId<HTMLInputElement>("find-input").value.trim();
      if (query) void this.runFind(query);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.operating = false;
    }
  }

  private async rotatePage(degrees: number): Promise<void> {
    if (!this.document || this.document.kind !== "pdf") return;
    await this.applyOperation({ op: "rotate_page", page: this.currentPageNumber(), degrees }, true);
  }

  private async deletePage(index: number): Promise<void> {
    const doc = this.document;
    if (!doc || doc.kind !== "pdf" || doc.page_count <= 1) return;
    const answer = await choose(
      "Delete page",
      `Delete page ${index + 1}? This can be undone until the document is saved.`,
      [
        { value: "cancel", label: "Cancel", style: "ghost" },
        { value: "delete", label: "Delete page", style: "danger" },
      ],
    );
    if (answer === "delete") await this.applyOperation({ op: "delete_page", page: index }, true);
  }

  private async createAnnotation(annotation: Annotation): Promise<void> {
    if (!this.document || this.document.read_only) return;
    if (annotation.kind === "free_text" || annotation.kind === "note") {
      const text = await askText(
        annotation.kind === "note" ? "Sticky note" : "Text box",
        "The text is stored in the PDF annotation.",
        "",
        "Add markup",
      );
      if (text === null) return;
      annotation.contents = text;
    }
    await this.applyOperation({ op: "add_annotation", page: annotation.page, annotation }, false);
    this.viewer.select(annotation.id);
  }

  private async editAnnotation(before: Annotation, after: Annotation): Promise<void> {
    if (!this.document || this.document.read_only) return;
    await this.applyOperation({ op: "edit_annotation", id: before.id, before, after }, false);
    this.viewer.select(after.id);
  }

  private async editAnnotationText(annotation: Annotation): Promise<void> {
    if (!this.document || this.document.read_only) return;
    const text = await askText(
      annotation.kind === "note" ? "Edit sticky note" : "Edit text markup",
      "Update the annotation contents.",
      annotation.contents ?? "",
      "Apply",
    );
    if (text === null) return;
    const after = { ...annotation, contents: text };
    await this.editAnnotation(annotation, after);
  }

  private async removeSelected(): Promise<void> {
    if (!this.document || !this.selected) return;
    const id = this.selected.id;
    await this.applyOperation({ op: "remove_annotation", id }, false);
    this.viewer.select(null);
  }

  private updateSelected(changes: Partial<Annotation>): void {
    const before = this.viewer.getSelected();
    if (!before || !this.document || this.document.read_only) return;
    const after = { ...before, ...changes };
    void this.editAnnotation(before, after);
  }

  private selectionChanged(annotation: Annotation | null): void {
    this.selected = annotation;
    const section = byId("selection-section");
    section.hidden = !annotation;
    if (!annotation) return;
    byId("selection-kind").textContent = annotation.kind.replace(/_/g, " ");
    byId<HTMLInputElement>("stroke-width").value = String(annotation.width);
    byId("stroke-width-value").textContent = `${annotation.width} pt`;
    byId<HTMLInputElement>("opacity").value = String(annotation.opacity);
    byId("opacity-value").textContent = `${Math.round(annotation.opacity * 100)}%`;
    byId<HTMLInputElement>("font-size").value = String(annotation.font_size);
    byId<HTMLTextAreaElement>("annotation-text").value = annotation.contents ?? "";
    const textKind = annotation.kind === "free_text" || annotation.kind === "note";
    byId("text-fields").hidden = !textKind;
    byId("fill-field").hidden = !["square", "circle", "free_text", "note"].includes(annotation.kind);
    byId("btn-bold").classList.toggle("active", annotation.bold);
    byId("btn-italic").classList.toggle("active", annotation.italic);
    const readOnly = !!this.document?.read_only;
    for (const control of Array.from(document.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLButtonElement>(
      "#selection-section input, #selection-section textarea, #selection-section button",
    ))) {
      control.disabled = readOnly;
    }
    for (const swatch of Array.from(document.querySelectorAll<HTMLElement>('[data-swatch-group="stroke"] .swatch'))) {
      swatch.classList.toggle("selected", swatch.dataset.color === annotation.color);
    }
    for (const swatch of Array.from(document.querySelectorAll<HTMLElement>('[data-swatch-group="fill"] .swatch'))) {
      swatch.classList.toggle("selected", swatch.dataset.color === (annotation.fill ?? ""));
    }
  }

  private renderSwatches(id: string, group: "stroke" | "fill"): void {
    const host = byId(id);
    host.dataset.swatchGroup = group;
    host.replaceChildren();
    if (group === "fill") {
      const none = document.createElement("button");
      none.type = "button";
      none.className = "swatch swatch-none";
      none.title = "No fill";
      none.setAttribute("aria-label", "No fill");
      none.addEventListener("click", () => this.updateSelected({ fill: null }));
      host.append(none);
    }
    for (const color of SWATCHES) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "swatch";
      button.style.setProperty("--swatch", color);
      button.title = color;
      button.setAttribute("aria-label", color);
      button.dataset.color = color;
      button.addEventListener("click", () => {
        if (group === "stroke") this.updateSelected({ color });
        else this.updateSelected({ fill: color });
      });
      host.append(button);
    }
  }

  private activateTool(tool: Tool): void {
    if (!this.document) return;
    if (tool === "crop" && this.document.kind !== "image") return;
    if (tool === "redact" && this.document.kind !== "pdf") return;
    if (this.document.read_only && tool !== "select") {
      toast("This encrypted PDF is read-only.", "error");
      return;
    }
    this.viewer.setTool(tool);
    for (const button of Array.from(document.querySelectorAll<HTMLButtonElement>("[data-tool]"))) {
      button.classList.toggle("active", button.dataset.tool === tool);
    }
  }

  private async redactArea(page: number, rect: { x: number; y: number; width: number; height: number }): Promise<void> {
    const doc = this.document;
    if (!doc || doc.kind !== "pdf" || doc.read_only) return;
    const answer = await choose(
      "Apply secure redaction",
      "Content intersecting this rectangle will be removed from the PDF, not just covered. This can be undone until you save; the saved redaction cannot be reversed.",
      [
        { value: "cancel", label: "Cancel", style: "ghost" },
        { value: "redact", label: "Remove content", style: "danger" },
      ],
    );
    if (answer !== "redact" || this.document?.id !== doc.id) return;
    await this.applyOperation({ op: "redact", page, rect }, true);
    this.activateTool("select");
  }

  private cropChanged(crop: { x: number; y: number; width: number; height: number } | null): void {
    const bar = byId("crop-bar");
    const active = !!crop && this.document?.kind === "image";
    bar.hidden = !active;
    if (!crop) return;
    byId("crop-dims").textContent = `${Math.round(crop.width)} × ${Math.round(crop.height)} px`;
    byId<HTMLButtonElement>("btn-crop-apply").disabled = crop.width < 1 || crop.height < 1;
  }

  private async applyCrop(): Promise<void> {
    const crop = this.viewer.getCrop();
    if (!crop) {
      toast("Draw a crop rectangle first.");
      return;
    }
    await this.applyOperation({ op: "crop_image", ...crop }, true);
    this.viewer.clearCrop();
    this.activateTool("select");
  }

  private showResize(): void {
    const image = this.document?.image;
    if (!image) return;
    byId("resize-current").textContent = `${image.width} × ${image.height} px`;
    byId<HTMLInputElement>("resize-width").value = String(image.width);
    byId<HTMLInputElement>("resize-height").value = String(image.height);
    byId<HTMLDialogElement>("resize-dialog").showModal();
  }

  private onResizeWidth(): void {
    const image = this.document?.image;
    if (!image || !byId<HTMLInputElement>("resize-lock").checked) return;
    const width = Number(byId<HTMLInputElement>("resize-width").value);
    if (width > 0) byId<HTMLInputElement>("resize-height").value = String(Math.max(1, Math.round(width * image.height / image.width)));
  }

  private onResizeHeight(): void {
    const image = this.document?.image;
    if (!image || !byId<HTMLInputElement>("resize-lock").checked) return;
    const height = Number(byId<HTMLInputElement>("resize-height").value);
    if (height > 0) byId<HTMLInputElement>("resize-width").value = String(Math.max(1, Math.round(height * image.width / image.height)));
  }

  private applyResizePreset(scale: number): void {
    const image = this.document?.image;
    if (!image || !Number.isFinite(scale)) return;
    byId<HTMLInputElement>("resize-width").value = String(Math.max(1, Math.round(image.width * scale)));
    byId<HTMLInputElement>("resize-height").value = String(Math.max(1, Math.round(image.height * scale)));
  }

  private showSidebarTab(tab: "pages" | "outline"): void {
    const pages = tab === "pages";
    byId("thumb-list").hidden = !pages;
    byId("sidebar-empty").hidden = !pages || !!this.document;
    byId("outline-list").hidden = pages;
    byId("tab-pages").classList.toggle("active", pages);
    byId("tab-pages").setAttribute("aria-selected", String(pages));
    byId("tab-outline").classList.toggle("active", !pages);
    byId("tab-outline").setAttribute("aria-selected", String(!pages));
  }

  private renderEpubOutline(entries: EpubOutlineEntry[]): void {
    if (this.document?.kind !== "epub") return;
    const host = byId("outline-list");
    host.replaceChildren();
    if (entries.length === 0) {
      const empty = document.createElement("p");
      empty.className = "insp-empty";
      empty.textContent = "This book has no table of contents.";
      host.append(empty);
      return;
    }
    for (const entry of entries) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "outline-item";
      button.style.setProperty("--depth", String(entry.depth));
      button.title = entry.title;
      button.textContent = entry.title;
      button.addEventListener("click", () => {
        void this.epubReader.displayHref(entry.href).catch((error) => toast(errorMessage(error), "error"));
      });
      host.append(button);
    }
  }

  private async refreshDocumentPanels(doc: DocumentInfo): Promise<void> {
    if (doc.kind === "epub") {
      byId("forms-section").hidden = true;
      return;
    }
    if (doc.kind !== "pdf") {
      this.bookmarks = [];
      this.renderBookmarks();
      byId("forms-section").hidden = true;
      return;
    }
    const [bookmarks, fields] = await Promise.all([
      api.getBookmarks(doc.id).catch(() => [] as BookmarkInfo[]),
      api.getFormFields(doc.id).catch(() => [] as FormFieldInfo[]),
    ]);
    if (this.document?.id !== doc.id) return;
    this.bookmarks = bookmarks;
    this.renderBookmarks();
    this.renderFormFields(fields, doc);
  }

  private renderBookmarks(): void {
    const host = byId("outline-list");
    host.replaceChildren();
    if (!this.bookmarks.length) {
      const empty = document.createElement("p");
      empty.className = "insp-empty";
      empty.textContent = this.document?.kind === "pdf" ? "No bookmarks in this PDF." : "Open a PDF to view its bookmarks.";
      host.append(empty);
      return;
    }
    for (const bookmark of this.bookmarks) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "outline-item";
      button.style.setProperty("--depth", String(bookmark.depth));
      button.title = bookmark.title;
      button.setAttribute("aria-label", `${bookmark.title}, page ${bookmark.page + 1}`);
      button.textContent = bookmark.title || `Page ${bookmark.page + 1}`;
      const page = document.createElement("span");
      page.className = "outline-page";
      page.textContent = String(bookmark.page + 1);
      button.append(page);
      button.addEventListener("click", () => this.goToPage(bookmark.page));
      host.append(button);
    }
  }

  private renderFormFields(fields: FormFieldInfo[], doc: DocumentInfo): void {
    const section = byId("forms-section");
    const host = byId("forms-list");
    host.replaceChildren();
    section.hidden = fields.length === 0;
    byId("forms-empty").hidden = fields.length > 0;
    for (const field of fields) {
      const row = document.createElement("div");
      row.className = "form-entry";
      const label = document.createElement("label");
      label.textContent = field.name;
      let input: HTMLInputElement | HTMLSelectElement;
      if (field.kind === "radio") {
        input = document.createElement("select");
        const off = document.createElement("option");
        off.value = "Off";
        off.textContent = "Not selected";
        input.append(off);
        for (const option of field.options) {
          const element = document.createElement("option");
          element.value = option;
          element.textContent = option;
          input.append(element);
        }
        input.value = field.value || "Off";
        input.disabled = field.options.length === 0 || doc.read_only;
      } else if (field.kind === "button") {
        input = document.createElement("input");
        input.type = "checkbox";
        input.checked = field.checked;
        input.setAttribute("aria-label", field.name);
      } else if (field.kind === "pushbutton") {
        input = document.createElement("input");
        input.type = "text";
        input.value = "Push button";
        input.disabled = true;
      } else if (field.kind === "choice" && field.options.length) {
        input = document.createElement("select");
        for (const option of field.options) {
          const element = document.createElement("option");
          element.value = option;
          element.textContent = option;
          input.append(element);
        }
        input.value = field.value;
      } else {
        input = document.createElement("input");
        input.type = field.kind === "signature" ? "text" : "text";
        input.value = field.value;
        input.placeholder = field.kind === "signature" ? "Signature fields cannot be signed here" : "Enter a value";
        input.disabled = field.kind === "signature" || doc.read_only;
      }
      input.disabled ||= doc.read_only;
      input.addEventListener("change", () => {
        const value = input instanceof HTMLInputElement && input.type === "checkbox"
          ? (input.checked ? "Yes" : "Off")
          : input.value;
        void this.fillFormField(field.name, value);
      });
      row.append(label, input);
      host.append(row);
    }
  }

  private async fillFormField(name: string, value: string): Promise<void> {
    if (!this.document || this.document.read_only) return;
    const id = this.document.id;
    try {
      this.document = await api.fillFormField(id, name, value);
      this.viewer.update(this.document, true);
      this.rail.update(this.document, true);
      this.setDocumentUI(this.document);
      await this.refreshDocumentPanels(this.document);
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  }

  private async autoFillFromContact(): Promise<void> {
    const doc = this.document;
    this.hideMenus();
    if (!doc || doc.kind !== "pdf" || doc.read_only) return;
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".vcf,text/vcard,text/x-vcard";
    input.hidden = true;
    document.body.append(input);
    input.addEventListener("change", () => {
      const file = input.files?.[0];
      input.remove();
      if (file) void this.applyVCardContact(doc, file);
    }, { once: true });
    input.addEventListener("cancel", () => input.remove(), { once: true });
    input.click();
  }

  private async runOcr(): Promise<void> {
    const doc = this.document;
    this.hideMenus();
    if (!doc || doc.kind !== "pdf" || doc.read_only) return;
    const answer = await choose(
      "Recognize scanned text",
      "OCR adds invisible, searchable text to scanned pages that have no text layer. Tesseract 5 and its English language data must be installed. This operation is undoable.",
      [
        { value: "cancel", label: "Cancel", style: "ghost" },
        { value: "recognize", label: "Recognize text", style: "primary" },
      ],
    );
    if (answer !== "recognize" || this.document?.id !== doc.id) return;
    this.setBusy(true, "Recognizing text…");
    try {
      await api.ocrDocument(doc.id);
      this.document = await api.getDocumentInfo(doc.id);
      this.viewer.update(this.document, true);
      this.rail.update(this.document, true);
      this.setDocumentUI(this.document);
      await this.refreshDocumentPanels(this.document);
      toast("OCR text was added. Use Undo to remove it.", "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private async applyVCardContact(doc: DocumentInfo, file: File): Promise<void> {
    try {
      const properties = parseVCard(await file.text());
      if (!Object.keys(properties).length) {
        toast("The selected file does not contain a readable vCard contact.", "error");
        return;
      }
      const fields = await api.getFormFields(doc.id);
      const updates = fields.flatMap((field) => {
        const value = contactValueForField(field, properties);
        return value === null ? [] : [{ field: field.name, value }];
      });
      if (!updates.length) {
        toast("No form fields matched the contact's name, address, phone, email, or organization.", "error");
        return;
      }
      const answer = await choose(
        "Fill form from contact",
        `Fill ${updates.length} matching field${updates.length === 1 ? "" : "s"} from ${file.name}? This change is undoable.`,
        [
          { value: "cancel", label: "Cancel", style: "ghost" },
          { value: "fill", label: "Fill fields", style: "primary" },
        ],
      );
      if (answer !== "fill" || this.document?.id !== doc.id) return;
      await api.applyOp(doc.id, { op: "fill_form_fields", fields: updates });
      this.document = await api.getDocumentInfo(doc.id);
      this.viewer.update(this.document, true);
      this.rail.update(this.document, true);
      this.setDocumentUI(this.document);
      await this.refreshDocumentPanels(this.document);
      toast(`Filled ${updates.length} form fields. Use Undo to reverse the update.`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  }

  private async openContactSheet(): Promise<void> {
    const doc = this.document;
    if (!doc) return;
    const dialog = byId<HTMLDialogElement>("contact-sheet-dialog");
    const grid = byId("contact-sheet-grid");
    this.closeContactSheet();
    grid.replaceChildren();
    byId("contact-sheet-status").textContent = `${doc.page_count} ${doc.page_count === 1 ? "page" : "pages"} · click a thumbnail to jump`;
    const items: HTMLElement[] = [];
    const images: HTMLImageElement[] = [];
    for (let index = 0; index < doc.page_count; index += 1) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "contact-page is-loading";
      button.setAttribute("aria-label", `Open page ${index + 1}`);
      const image = document.createElement("img");
      image.alt = "";
      const label = document.createElement("span");
      label.textContent = doc.kind === "pdf" ? `Page ${index + 1}` : "Image";
      button.append(image, label);
      button.addEventListener("click", () => {
        dialog.close();
        this.goToPage(index);
      });
      grid.append(button);
      items.push(button);
      images.push(image);
    }
    dialog.showModal();
    this.contactObserver = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const index = items.indexOf(entry.target as HTMLElement);
        const image = images[index];
        if (index < 0 || !image || image.dataset.loaded) continue;
        image.dataset.loaded = "true";
        void api.renderPage(doc.id, index, 60).then((bytes) => {
          if (this.document?.id !== doc.id || !dialog.open) return;
          const url = URL.createObjectURL(new Blob([bytes], { type: doc.mime }));
          this.contactUrls.push(url);
          image.src = url;
          items[index]?.classList.remove("is-loading");
        }).catch(() => items[index]?.classList.remove("is-loading"));
        this.contactObserver?.unobserve(entry.target);
      }
    }, { root: grid, rootMargin: "500px" });
    for (const item of items) this.contactObserver.observe(item);
  }

  private closeContactSheet(): void {
    this.contactObserver?.disconnect();
    this.contactObserver = null;
    for (const url of this.contactUrls) URL.revokeObjectURL(url);
    this.contactUrls = [];
  }

  private async toggleFullscreen(): Promise<void> {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else await document.documentElement.requestFullscreen();
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  }

  private async toggleSlideshow(): Promise<void> {
    if (this.slideshowTimer) {
      this.stopSlideshow();
      return;
    }
    if (!this.document || this.document.kind === "epub" || this.document.page_count < 2) return;
    this.slideshowTimer = window.setInterval(() => {
      if (!this.document) return this.stopSlideshow();
      const next = this.currentPageNumber() + 1;
      if (next >= this.document.page_count) {
        this.stopSlideshow();
        return;
      }
      this.goToPage(next);
    }, 3000);
    byId("btn-slideshow").classList.add("active");
    if (!document.fullscreenElement) await document.documentElement.requestFullscreen().catch(() => undefined);
  }

  private stopSlideshow(): void {
    if (this.slideshowTimer) window.clearInterval(this.slideshowTimer);
    this.slideshowTimer = 0;
    byId("btn-slideshow").classList.remove("active");
  }

  private async showImageAdjustments(): Promise<void> {
    const doc = this.document;
    if (!doc?.image) return;
    if (this.imageAdjustmentsOpen && this.imageAdjustmentsDocumentId === doc.id) return;
    if (this.imageAdjustmentsOpen) this.closeImageAdjustments();
    this.inspectorHiddenBeforeAdjustments = document.body.classList.contains("inspector-hidden");
    document.body.classList.remove("inspector-hidden");
    byId("inspector-content").hidden = true;
    byId("image-adjust-controls").hidden = false;
    byId("inspector-title").textContent = "Image adjustments";
    byId("btn-inspector-close").setAttribute("aria-label", "Close image adjustments");
    byId("btn-inspector-close").setAttribute("title", "Close image adjustments");
    this.imageAdjustmentsOpen = true;
    this.imageAdjustmentsDocumentId = doc.id;
    this.viewer.setImagePreviewFilter(null);
    for (const [id, value] of [["adjust-brightness", 0], ["adjust-contrast", 0], ["adjust-saturation", 100], ["adjust-sharpen", 0], ["levels-black", 0], ["levels-white", 100], ["levels-gamma", 100]] as const) {
      byId<HTMLInputElement>(id).value = String(value);
    }
    this.curvePoints = [{ x: 0, y: 0 }, { x: 1, y: 1 }];
    this.drawCurve();
    try {
      const bins = await api.imageHistogram(doc.id);
      if (this.imageAdjustmentsDocumentId === doc.id && this.document?.id === doc.id) this.drawHistogram(bins);
    } catch (error) {
      if (this.imageAdjustmentsDocumentId === doc.id) toast(errorMessage(error), "error");
    }
  }

  private closeImageAdjustments(restoreInspector = true): void {
    if (!this.imageAdjustmentsOpen) return;
    this.viewer.setImagePreviewFilter(null);
    this.imageAdjustmentsOpen = false;
    this.imageAdjustmentsDocumentId = null;
    byId("image-adjust-controls").hidden = true;
    byId("inspector-content").hidden = false;
    byId("inspector-title").textContent = "Inspector";
    byId("btn-inspector-close").setAttribute("aria-label", "Close inspector");
    byId("btn-inspector-close").setAttribute("title", "Close inspector");
    if (restoreInspector && this.inspectorHiddenBeforeAdjustments) document.body.classList.add("inspector-hidden");
    this.inspectorHiddenBeforeAdjustments = false;
  }

  private openInstantAlpha(): void {
    if (this.document?.kind !== "image") return;
    byId<HTMLInputElement>("instant-alpha-color").value = "#ffffff";
    byId<HTMLInputElement>("instant-alpha-tolerance").value = "12";
    byId("instant-alpha-value").textContent = "12";
    byId<HTMLDialogElement>("instant-alpha-dialog").showModal();
  }

  private drawHistogram(bins: number[]): void {
    const canvas = byId<HTMLCanvasElement>("image-histogram");
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    const max = Math.max(1, ...bins);
    ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() || "#527c68";
    for (let index = 0; index < Math.min(256, bins.length); index += 1) {
      const height = (bins[index]! / max) * (canvas.height - 4);
      ctx.fillRect(index, canvas.height - height, 1, height);
    }
  }

  private previewImageAdjustments(): void {
    const brightness = Number(byId<HTMLInputElement>("adjust-brightness").value);
    const contrast = Number(byId<HTMLInputElement>("adjust-contrast").value);
    const saturation = Number(byId<HTMLInputElement>("adjust-saturation").value);
    const black = Number(byId<HTMLInputElement>("levels-black").value) / 100;
    const white = Number(byId<HTMLInputElement>("levels-white").value) / 100;
    const gamma = Number(byId<HTMLInputElement>("levels-gamma").value) / 100;
    const sharpen = Number(byId<HTMLInputElement>("adjust-sharpen").value) / 100;
    const curveChanged = this.curvePoints.some((point) => Math.abs(point.y - point.x) > 0.001);
    if (brightness === 0 && contrast === 0 && saturation === 100 && sharpen === 0
      && black === 0 && white === 1 && gamma === 1 && !curveChanged) {
      this.viewer.setImagePreviewFilter(null);
      return;
    }
    const range = Math.max(0.001, white - black);
    const samples = Array.from({ length: 65 }, (_, index) => {
      const input = index / 64;
      const leveled = Math.pow(Math.min(1, Math.max(0, (input - black) / range)), 1 / gamma);
      return this.curveValueAt(leveled).toFixed(4);
    }).join(" ");
    for (const id of ["curve-filter-r", "curve-filter-g", "curve-filter-b"]) {
      byId(id).setAttribute("tableValues", samples);
    }
    const neighbor = (-sharpen / 4).toFixed(4);
    const center = (1 + sharpen).toFixed(4);
    byId("sharpen-preview-matrix").setAttribute(
      "kernelMatrix",
      `0 ${neighbor} 0 ${neighbor} ${center} ${neighbor} 0 ${neighbor} 0`,
    );
    this.viewer.setImagePreviewFilter(
      `url(#image-curve-preview) url(#image-sharpen-preview) brightness(${Math.max(0, 1 + brightness / 100)}) contrast(${Math.max(0, 1 + contrast / 100)}) saturate(${saturation / 100})`,
    );
  }

  private wireCurveEditor(): void {
    const canvas = byId<HTMLCanvasElement>("image-curve");
    const pointFromEvent = (event: PointerEvent): Point => {
      const rect = canvas.getBoundingClientRect();
      const x = ((event.clientX - rect.left) / Math.max(1, rect.width)) * canvas.width;
      const y = ((event.clientY - rect.top) / Math.max(1, rect.height)) * canvas.height;
      const inset = 14;
      return {
        x: Math.min(1, Math.max(0, (x - inset) / (canvas.width - inset * 2))),
        y: 1 - Math.min(1, Math.max(0, (y - inset) / (canvas.height - inset * 2))),
      };
    };
    canvas.addEventListener("pointerdown", (event) => {
      if (!this.document?.image) return;
      event.preventDefault();
      const point = pointFromEvent(event);
      const rect = canvas.getBoundingClientRect();
      const scaleX = canvas.width / Math.max(1, rect.width);
      const scaleY = canvas.height / Math.max(1, rect.height);
      let closest = -1;
      let distance = 18 * 18;
      for (let index = 1; index < this.curvePoints.length - 1; index += 1) {
        const curvePoint = this.curvePoints[index]!;
        const dx = (curvePoint.x - point.x) * (canvas.width - 28) / scaleX;
        const dy = (curvePoint.y - point.y) * (canvas.height - 28) / scaleY;
        const nextDistance = dx * dx + dy * dy;
        if (nextDistance <= distance) {
          closest = index;
          distance = nextDistance;
        }
      }
      if (closest < 0 && this.curvePoints.length < 16 && point.x > 0.01 && point.x < 0.99) {
        closest = this.curvePoints.findIndex((item) => item.x > point.x);
        if (closest < 1) closest = this.curvePoints.length - 1;
        this.curvePoints.splice(closest, 0, point);
      }
      if (closest < 1 || closest >= this.curvePoints.length - 1) return;
      this.curveDragIndex = closest;
      canvas.setPointerCapture(event.pointerId);
      this.drawCurve();
      this.previewImageAdjustments();
    });
    canvas.addEventListener("pointermove", (event) => {
      if (this.curveDragIndex < 1) return;
      const point = pointFromEvent(event);
      const previous = this.curvePoints[this.curveDragIndex - 1]!;
      const next = this.curvePoints[this.curveDragIndex + 1]!;
      this.curvePoints[this.curveDragIndex] = {
        x: Math.min(next.x - 0.005, Math.max(previous.x + 0.005, point.x)),
        y: point.y,
      };
      this.drawCurve();
      this.previewImageAdjustments();
    });
    const stop = (): void => { this.curveDragIndex = -1; };
    canvas.addEventListener("pointerup", stop);
    canvas.addEventListener("pointercancel", stop);
  }

  private curveValueAt(value: number): number {
    let index = 0;
    while (index + 1 < this.curvePoints.length - 1 && value > this.curvePoints[index + 1]!.x) index += 1;
    const start = this.curvePoints[index]!;
    const end = this.curvePoints[index + 1]!;
    const fraction = (value - start.x) / (end.x - start.x);
    return Math.min(1, Math.max(0, start.y + (end.y - start.y) * fraction));
  }

  private drawCurve(): void {
    const canvas = byId<HTMLCanvasElement>("image-curve");
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const inset = 14;
    const width = canvas.width - inset * 2;
    const height = canvas.height - inset * 2;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.strokeStyle = getComputedStyle(document.documentElement).getPropertyValue("--line").trim() || "#777";
    ctx.lineWidth = 1;
    for (let index = 1; index < 4; index += 1) {
      const offset = inset + width * index / 4;
      ctx.beginPath();
      ctx.moveTo(offset, inset);
      ctx.lineTo(offset, canvas.height - inset);
      ctx.moveTo(inset, inset + height * index / 4);
      ctx.lineTo(canvas.width - inset, inset + height * index / 4);
      ctx.stroke();
    }
    ctx.strokeStyle = getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() || "#527c68";
    ctx.lineWidth = 2;
    ctx.beginPath();
    for (let index = 0; index <= 128; index += 1) {
      const value = index / 128;
      const x = inset + value * width;
      const y = canvas.height - inset - this.curveValueAt(value) * height;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();
    for (const point of this.curvePoints) {
      const x = inset + point.x * width;
      const y = canvas.height - inset - point.y * height;
      ctx.beginPath();
      ctx.arc(x, y, 5, 0, Math.PI * 2);
      ctx.fillStyle = "#ffffff";
      ctx.fill();
      ctx.strokeStyle = getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() || "#527c68";
      ctx.stroke();
    }
  }

  private async applyImageAdjustments(): Promise<void> {
    if (!this.document?.image) return;
    const adjustment = {
      op: "adjust_image" as const,
      brightness: Number(byId<HTMLInputElement>("adjust-brightness").value) * 2.55,
      contrast: Number(byId<HTMLInputElement>("adjust-contrast").value),
      saturation: Number(byId<HTMLInputElement>("adjust-saturation").value) / 100,
      sharpen: Number(byId<HTMLInputElement>("adjust-sharpen").value) / 100,
    };
    const black = Number(byId<HTMLInputElement>("levels-black").value) / 100;
    const white = Number(byId<HTMLInputElement>("levels-white").value) / 100;
    const gamma = Number(byId<HTMLInputElement>("levels-gamma").value) / 100;
    const useAdjustment = adjustment.brightness !== 0 || adjustment.contrast !== 0 || adjustment.saturation !== 1 || adjustment.sharpen !== 0;
    const useLevels = black !== 0 || white !== 1 || gamma !== 1;
    const useCurve = this.curvePoints.some((point) => Math.abs(point.y - point.x) > 0.001);
    this.closeImageAdjustments();
    if (useAdjustment) await this.applyOperation(adjustment, true);
    if (useLevels) await this.applyOperation({ op: "image_levels", black, white, gamma }, true);
    if (useCurve) await this.applyOperation({ op: "image_curves", points: this.curvePoints.map((point) => ({ ...point })) }, true);
  }

  private async insertPagesFromPdf(): Promise<void> {
    const doc = this.document;
    if (!doc || doc.kind !== "pdf") return;
    this.hideMenus();
    const selected = await open({ title: "Insert pages from PDF", filters: [{ name: "PDF", extensions: ["pdf"] }], multiple: false });
    if (typeof selected !== "string") return;
    await this.applyOperation({ op: "insert_pages", source_path: selected, at: this.currentPageNumber() + 1 }, true);
  }

  private async mergePdfs(): Promise<void> {
    this.hideMenus();
    const selected = await open({ title: "Select PDFs to merge in file order", filters: [{ name: "PDF", extensions: ["pdf"] }], multiple: true });
    if (!Array.isArray(selected) || selected.length < 2) return;
    const path = await save({ title: "Save merged PDF", defaultPath: "merged.pdf", filters: [{ name: "PDF", extensions: ["pdf"] }] });
    if (!path) return;
    this.setBusy(true, "Merging PDFs…");
    try {
      await api.mergePdfs(selected, path);
      toast(`Merged ${selected.length} PDFs into ${basename(path)}`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private openExtractDialog(): void {
    if (!this.document || this.document.kind !== "pdf") return;
    byId<HTMLInputElement>("extract-pages").value = "";
    byId<HTMLDialogElement>("extract-dialog").showModal();
    this.hideMenus();
  }

  private async extractPages(): Promise<void> {
    const doc = this.document;
    if (!doc || doc.kind !== "pdf") return;
    const value = byId<HTMLInputElement>("extract-pages").value.trim();
    const pages: number[] = [];
    for (const part of value.split(",").map((piece) => piece.trim()).filter(Boolean)) {
      const match = /^(\d+)(?:\s*-\s*(\d+))?$/.exec(part);
      if (!match) { toast(`Invalid page range: ${part}`, "error"); return; }
      const start = Number(match[1]);
      const end = Number(match[2] ?? match[1]);
      if (start < 1 || end < start || end > doc.page_count) { toast(`Page range out of bounds: ${part}`, "error"); return; }
      for (let page = start; page <= end; page += 1) pages.push(page - 1);
    }
    if (!pages.length || new Set(pages).size !== pages.length) { toast("Select one or more unique pages.", "error"); return; }
    const path = await save({ title: "Export selected pages", defaultPath: `${doc.name.replace(/\.[^.]+$/, "")}-pages.pdf`, filters: [{ name: "PDF", extensions: ["pdf"] }] });
    if (!path) return;
    byId<HTMLDialogElement>("extract-dialog").close();
    this.setBusy(true, "Extracting pages…");
    try {
      await api.exportDocument(doc.id, path, { pages });
      toast(`Exported ${pages.length} pages`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private openProtectDialog(): void {
    if (!this.document || this.document.kind !== "pdf") return;
    this.hideMenus();
    byId<HTMLInputElement>("protect-password").value = "";
    byId<HTMLInputElement>("protect-password-confirm").value = "";
    byId<HTMLDialogElement>("protect-dialog").showModal();
  }

  private async protectPdf(): Promise<void> {
    const doc = this.document;
    if (!doc || doc.kind !== "pdf") return;
    const password = byId<HTMLInputElement>("protect-password").value;
    if (!password || password !== byId<HTMLInputElement>("protect-password-confirm").value) {
      toast("Enter the same non-empty password twice.", "error");
      return;
    }
    const path = await save({ title: "Save password-protected PDF", defaultPath: `${doc.name.replace(/\.[^.]+$/, "")}-protected.pdf`, filters: [{ name: "PDF", extensions: ["pdf"] }] });
    if (!path) return;
    byId<HTMLDialogElement>("protect-dialog").close();
    this.setBusy(true, "Encrypting PDF…");
    try {
      await api.exportDocument(doc.id, path, { password });
      toast(`Saved AES-256 protected copy: ${basename(path)}`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private async exportUnlockedPdf(): Promise<void> {
    const doc = this.document;
    if (!doc || doc.kind !== "pdf" || !doc.encrypted) return;
    this.hideMenus();
    const path = await save({ title: "Export unlocked PDF", defaultPath: `${doc.name.replace(/\.[^.]+$/, "")}-unlocked.pdf`, filters: [{ name: "PDF", extensions: ["pdf"] }] });
    if (!path) return;
    this.setBusy(true, "Writing unlocked PDF…");
    try {
      await api.exportDocument(doc.id, path, { remove_password: true });
      toast(`Saved unlocked copy: ${basename(path)}`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private async batchResizeImages(): Promise<void> {
    this.hideMenus();
    const paths = await open({ title: "Select images to batch resize", filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "tif", "tiff", "gif", "heic", "heif", "webp", "bmp", "cr2", "nef", "arw", "dng"] }], multiple: true });
    if (!Array.isArray(paths) || paths.length === 0) return;
    const sizeText = await askText("Batch resize", "Enter target width × height in pixels (for example, 1200x800).", "1200x800", "Continue");
    if (!sizeText) return;
    const match = /^\s*(\d+)\s*[x×]\s*(\d+)\s*$/i.exec(sizeText);
    if (!match) { toast("Enter dimensions as width x height.", "error"); return; }
    const width = Number(match[1]);
    const height = Number(match[2]);
    const selectedDir = await open({ title: "Choose batch output folder", directory: true, multiple: false });
    if (typeof selectedDir !== "string") return;
    this.setBusy(true, `Resizing ${paths.length} images…`);
    try {
      const outputs = await api.batchResizeImages(paths, selectedDir, width, height);
      toast(`Resized ${outputs.length} images to PNG`, "success");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      this.setBusy(false);
    }
  }

  private wireSignatureCanvas(): void {
    const canvas = byId<HTMLCanvasElement>("signature-canvas");
    const point = (event: PointerEvent) => {
      const bounds = canvas.getBoundingClientRect();
      return {
        x: (event.clientX - bounds.left) * canvas.width / bounds.width,
        y: (event.clientY - bounds.top) * canvas.height / bounds.height,
      };
    };
    canvas.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      canvas.setPointerCapture(event.pointerId);
      this.signatureDrawing = true;
      this.signatureLast = point(event);
      const ctx = canvas.getContext("2d");
      if (!ctx || !this.signatureLast) return;
      ctx.strokeStyle = "#17211d";
      ctx.lineWidth = 3.5;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.beginPath();
      ctx.moveTo(this.signatureLast.x, this.signatureLast.y);
      ctx.lineTo(this.signatureLast.x + 0.01, this.signatureLast.y + 0.01);
      ctx.stroke();
    });
    canvas.addEventListener("pointermove", (event) => {
      if (!this.signatureDrawing || !this.signatureLast) return;
      const next = point(event);
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.beginPath();
      ctx.moveTo(this.signatureLast.x, this.signatureLast.y);
      ctx.lineTo(next.x, next.y);
      ctx.stroke();
      this.signatureLast = next;
    });
    const finish = () => { this.signatureDrawing = false; this.signatureLast = null; };
    canvas.addEventListener("pointerup", finish);
    canvas.addEventListener("pointercancel", finish);
  }

  private openSignatureDialog(): void {
    if (!this.document || this.document.kind !== "pdf" || this.document.read_only) return;
    this.clearSignatureCanvas();
    byId<HTMLInputElement>("signature-store").checked = false;
    byId("signature-name-row").hidden = true;
    byId<HTMLInputElement>("signature-name").value = "My signature";
    this.renderSignatureLibrary();
    byId<HTMLDialogElement>("signature-dialog").showModal();
  }

  private renderSignatureLibrary(): void {
    const host = byId("signature-library");
    host.replaceChildren();
    const signatures = readSavedSignatures();
    if (!signatures.length) {
      const empty = document.createElement("span");
      empty.className = "signature-library-empty";
      empty.textContent = "No signatures saved on this device.";
      host.append(empty);
      return;
    }
    for (const signature of signatures) {
      const preset = document.createElement("button");
      preset.type = "button";
      preset.className = "signature-preset";
      preset.title = `Use saved signature: ${signature.name}`;
      const preview = document.createElement("img");
      preview.src = signature.png;
      preview.alt = "";
      preview.draggable = false;
      const name = document.createElement("span");
      name.textContent = signature.name;
      preset.append(preview, name);
      preset.addEventListener("click", () => void this.useSavedSignature(signature.png));
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "signature-delete";
      remove.textContent = "×";
      remove.title = `Remove saved signature ${signature.name} from this device`;
      remove.setAttribute("aria-label", remove.title);
      remove.addEventListener("click", () => void this.removeSavedSignature(signature));
      const row = document.createElement("span");
      row.className = "signature-preset-row";
      row.append(preset, remove);
      host.append(row);
    }
  }

  private async useSavedSignature(png: string): Promise<void> {
    try {
      const image = new Image();
      image.src = png;
      await image.decode();
      const canvas = byId<HTMLCanvasElement>("signature-canvas");
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      this.clearSignatureCanvas();
      const scale = Math.min((canvas.width - 24) / image.naturalWidth, (canvas.height - 24) / image.naturalHeight);
      const width = image.naturalWidth * scale;
      const height = image.naturalHeight * scale;
      ctx.drawImage(image, (canvas.width - width) / 2, (canvas.height - height) / 2, width, height);
    } catch (error) {
      toast(`Could not load saved signature: ${errorMessage(error)}`, "error");
    }
  }

  private async removeSavedSignature(signature: SavedSignature): Promise<void> {
    const answer = await choose(
      "Remove saved signature",
      `Remove “${signature.name}” from this device? Existing signature stamps in PDFs will not change.`,
      [
        { value: "cancel", label: "Cancel", style: "ghost" },
        { value: "remove", label: "Remove", style: "danger" },
      ],
    );
    if (answer !== "remove") return;
    try {
      const signatures = readSavedSignatures().filter((item) => item.id !== signature.id);
      localStorage.setItem(SIGNATURE_STORAGE_KEY, JSON.stringify(signatures));
      this.renderSignatureLibrary();
    } catch (error) {
      toast(`Could not update the local signature library: ${errorMessage(error)}`, "error");
    }
  }

  private clearSignatureCanvas(): void {
    const canvas = byId<HTMLCanvasElement>("signature-canvas");
    canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
  }

  private async loadSignatureFile(file: File): Promise<void> {
    try {
      const bitmap = await createImageBitmap(file);
      const canvas = byId<HTMLCanvasElement>("signature-canvas");
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      this.clearSignatureCanvas();
      const scale = Math.min((canvas.width - 24) / bitmap.width, (canvas.height - 24) / bitmap.height);
      const width = bitmap.width * scale;
      const height = bitmap.height * scale;
      ctx.drawImage(bitmap, (canvas.width - width) / 2, (canvas.height - height) / 2, width, height);
      bitmap.close();
      this.cleanSignatureBackground(canvas);
    } catch (error) {
      toast(`Could not read signature image: ${errorMessage(error)}`, "error");
    }
  }

  private async startSignatureCamera(): Promise<void> {
    try {
      this.stopSignatureCamera();
      this.signatureStream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "user" }, audio: false });
      const video = byId<HTMLVideoElement>("signature-video");
      video.srcObject = this.signatureStream;
      video.hidden = false;
      byId("signature-capture").hidden = false;
      await video.play();
    } catch (error) {
      toast(`Camera unavailable: ${errorMessage(error)}`, "error");
    }
  }

  private captureSignatureCamera(): void {
    const video = byId<HTMLVideoElement>("signature-video");
    if (!video.videoWidth || !video.videoHeight) return;
    const canvas = byId<HTMLCanvasElement>("signature-canvas");
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    this.clearSignatureCanvas();
    const scale = Math.min((canvas.width - 20) / video.videoWidth, (canvas.height - 20) / video.videoHeight);
    const width = video.videoWidth * scale;
    const height = video.videoHeight * scale;
    ctx.drawImage(video, (canvas.width - width) / 2, (canvas.height - height) / 2, width, height);
    this.cleanSignatureBackground(canvas);
    this.stopSignatureCamera();
  }

  private cleanSignatureBackground(canvas: HTMLCanvasElement): void {
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const image = ctx.getImageData(0, 0, canvas.width, canvas.height);
    for (let index = 0; index < image.data.length; index += 4) {
      const luminance = 0.2126 * image.data[index]! + 0.7152 * image.data[index + 1]! + 0.0722 * image.data[index + 2]!;
      if (luminance > 248) image.data[index + 3] = 0;
      else if (luminance > 210) image.data[index + 3] = Math.round(image.data[index + 3]! * (248 - luminance) / 38);
      image.data[index] = 23;
      image.data[index + 1] = 33;
      image.data[index + 2] = 29;
    }
    ctx.putImageData(image, 0, 0);
  }

  private stopSignatureCamera(): void {
    this.signatureStream?.getTracks().forEach((track) => track.stop());
    this.signatureStream = null;
    const video = byId<HTMLVideoElement>("signature-video");
    video.pause();
    video.srcObject = null;
    video.hidden = true;
    byId("signature-capture").hidden = true;
  }

  private async placeSignature(): Promise<void> {
    const doc = this.document;
    if (!doc || doc.kind !== "pdf") return;
    const canvas = byId<HTMLCanvasElement>("signature-canvas");
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const pixels = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
    let visible = false;
    for (let index = 3; index < pixels.length; index += 4) {
      if (pixels[index]! > 8) { visible = true; break; }
    }
    if (!visible) { toast("Draw a signature or choose an image first.", "error"); return; }
    const pageIndex = this.currentPageNumber();
    const page = doc.pages[pageIndex];
    if (!page) return;
    const png = canvas.toDataURL("image/png");
    if (byId<HTMLInputElement>("signature-store").checked) {
      const signatures = readSavedSignatures();
      if (signatures.length >= MAX_SAVED_SIGNATURES) {
        toast(`The local signature library is limited to ${MAX_SAVED_SIGNATURES} entries; place this signature without saving it or remove an old entry.`, "error");
        return;
      }
      const name = byId<HTMLInputElement>("signature-name").value.trim().slice(0, 48) || "My signature";
      const signature = { id: crypto.randomUUID(), name, png };
      try {
        localStorage.setItem(SIGNATURE_STORAGE_KEY, JSON.stringify([...signatures, signature]));
      } catch (error) {
        toast(`Could not save the signature locally: ${errorMessage(error)}`, "error");
        return;
      }
    }
    const annotation: Annotation = {
      id: crypto.randomUUID(),
      page: pageIndex,
      kind: "signature",
      rect: { x: page.width * 0.1, y: page.height * 0.72, width: page.width * 0.8, height: page.height * 0.18 },
      points: [],
      image_data: png.split(",")[1] ?? null,
      color: "#17211d",
      fill: null,
      width: 1,
      opacity: 1,
      contents: null,
      font_size: 12,
      bold: false,
      italic: false,
      existing: false,
      dirty: false,
    };
    byId<HTMLDialogElement>("signature-dialog").close();
    await this.createAnnotation(annotation);
  }

  private async revert(): Promise<void> {
    if (!this.document?.dirty) {
      this.hideMenus();
      return;
    }
    const answer = await choose(
      "Revert changes",
      "Discard all changes since the last save? This cannot be undone.",
      [
        { value: "cancel", label: "Cancel", style: "ghost" },
        { value: "revert", label: "Revert", style: "danger" },
      ],
    );
    this.hideMenus();
    if (answer !== "revert" || !this.document) return;
    try {
      const updated = await api.revertDocument(this.document.id);
      this.closeFind();
      this.document = updated;
      this.metadata = null;
      this.viewer.setDocument(updated);
      this.rail.setDocument(updated);
      this.setDocumentUI(updated);
      await this.refreshMetadata();
      toast("Reverted to the saved file.");
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  }

  private async refreshMetadata(): Promise<void> {
    if (!this.document) {
      this.metadata = null;
      this.renderMetadata();
      return;
    }
    try {
      this.metadata = await api.getMetadata(this.document.id);
    } catch {
      this.metadata = null;
    }
    this.renderMetadata();
  }

  private renderMetadata(): void {
    const host = byId("metadata-list");
    host.replaceChildren();
    const entries = this.metadata?.entries ?? [];
    byId("metadata-empty").hidden = entries.length > 0;
    for (const entry of entries) {
      const row = document.createElement("div");
      row.className = "metadata-row";
      const label = document.createElement("span");
      label.className = "metadata-key";
      label.textContent = entry.key;
      const value = document.createElement("span");
      value.className = "metadata-value";
      value.textContent = entry.value;
      row.append(label, value);
      host.append(row);
    }
    const canStrip = !!this.metadata?.can_strip_gps;
    byId("strip-gps-row").hidden = !canStrip;
    byId("export-gps-field").hidden = this.document?.kind !== "image" || !canStrip;
  }

  private currentPageNumber(): number {
    return this.document?.kind === "epub" ? this.epubReader.currentPage : this.viewer.currentPageNumber();
  }

  private goToPage(index: number): void {
    if (this.document?.kind === "epub") {
      void this.epubReader.goToPage(index).catch((error) => toast(errorMessage(error), "error"));
    } else {
      this.viewer.goToPage(index);
    }
  }

  private zoomIn(): void {
    if (this.document?.kind === "epub") this.epubReader.zoomBy(10);
    else this.viewer.zoomIn();
  }

  private zoomOut(): void {
    if (this.document?.kind === "epub") this.epubReader.zoomBy(-10);
    else this.viewer.zoomOut();
  }

  private setZoom(percent: number): void {
    if (this.document?.kind === "epub") this.epubReader.setZoom(percent);
    else this.viewer.setZoom(percent);
  }

  private fitWidth(): void {
    if (this.document?.kind === "epub") this.epubReader.resetZoom();
    else this.viewer.fitWidth();
  }

  private fitPage(): void {
    if (this.document?.kind === "epub") this.epubReader.resetZoom();
    else this.viewer.fitPage();
  }

  private epubPositionChanged(index: number, total: number, chapter: string): void {
    const doc = this.document;
    if (doc?.kind !== "epub") return;
    doc.page_count = Math.max(1, total);
    this.epubChapter = chapter;
    this.documents.set(doc.id, doc);
    this.lastPages.set(doc.id, Math.max(0, Math.min(doc.page_count - 1, index)));
    byId("page-count-label").textContent = String(doc.page_count);
    byId("page-total").textContent = `/ ${doc.page_count}`;
    byId<HTMLInputElement>("page-number").max = String(doc.page_count);
    byId<HTMLInputElement>("page-number").value = String(index + 1);
    byId("status-page").textContent = `Position ${index + 1} of ${doc.page_count}`;
    byId("status-dims").textContent = chapter || "EPUB";
    byId<HTMLButtonElement>("btn-prev-page").disabled = index <= 0;
    byId<HTMLButtonElement>("btn-next-page").disabled = index >= doc.page_count - 1;
  }

  private setDocumentUI(doc: DocumentInfo | null): void {
    if (this.imageAdjustmentsOpen && this.imageAdjustmentsDocumentId !== doc?.id) {
      this.closeImageAdjustments();
    }
    if (doc) this.documents.set(doc.id, doc);
    this.renderDocumentTabs();
    if (!doc) {
      this.stopSlideshow();
      this.closeContactSheet();
      this.bookmarks = [];
      this.renderBookmarks();
    }
    document.body.classList.toggle("no-document", !doc);
    document.body.classList.toggle("epub-document", doc?.kind === "epub");
    byId("welcome").hidden = !!doc;
    byId("markupbar").hidden = !doc || doc.kind === "epub";
    byId("markup-label").hidden = doc?.kind !== "pdf";
    byId("markup-tools").hidden = doc?.kind !== "pdf";
    byId("image-actions").hidden = doc?.kind !== "image";
    byId("pdf-actions").hidden = doc?.kind !== "pdf";
    byId("page-nav").classList.toggle("image-nav", doc?.kind === "image");
    const isEpub = doc?.kind === "epub";
    byId("tab-pages").hidden = isEpub;
    byId("tab-outline").textContent = isEpub ? "Contents" : "Outline";
    if (doc?.kind !== this.sidebarDocumentKind) {
      this.showSidebarTab(isEpub ? "outline" : "pages");
      this.sidebarDocumentKind = doc?.kind ?? null;
    }
    byId("doc-name").textContent = doc?.name ?? "No document";
    byId("dirty-dot").hidden = !doc?.dirty;
    byId<HTMLButtonElement>("btn-save").disabled = !doc || doc.read_only || !doc.dirty;
    byId<HTMLButtonElement>("btn-export").disabled = !doc || isEpub;
    byId<HTMLButtonElement>("btn-find").disabled = !doc || doc.kind !== "pdf";
    byId<HTMLButtonElement>("btn-contact-sheet").disabled = !doc || isEpub || doc.page_count < 2;
    byId<HTMLButtonElement>("btn-fullscreen").disabled = !doc;
    byId<HTMLButtonElement>("btn-slideshow").disabled = !doc || isEpub || doc.page_count < 2;
    byId<HTMLButtonElement>("btn-signature").disabled = !doc || doc.kind !== "pdf" || doc.read_only;
    byId<HTMLButtonElement>("btn-undo").disabled = !doc?.can_undo || !!doc.read_only;
    byId<HTMLButtonElement>("btn-redo").disabled = !doc?.can_redo || !!doc.read_only;
    byId<HTMLInputElement>("page-number").disabled = !doc;
    byId<HTMLButtonElement>("btn-prev-page").disabled = !doc || this.currentPageNumber() <= 0;
    byId<HTMLButtonElement>("btn-next-page").disabled = !doc || this.currentPageNumber() >= (doc.page_count - 1);
    for (const id of ["btn-zoom-out", "zoom-label", "btn-zoom-in", "btn-fit-width", "btn-fit-page"]) byId(id).toggleAttribute("disabled", !doc);
    for (const id of ["btn-rotate-left", "btn-rotate-right", "btn-delete-page"]) byId(id).toggleAttribute("disabled", !doc || doc.read_only);
    for (const id of ["btn-crop", "btn-resize", "btn-image-rotate-left", "btn-image-rotate-right", "btn-flip-h", "btn-flip-v", "btn-image-adjust", "btn-instant-alpha"]) byId(id).toggleAttribute("disabled", !doc || doc.read_only || doc.kind !== "image");
    for (const button of Array.from(document.querySelectorAll<HTMLButtonElement>(".markup-tools [data-tool]"))) {
      button.disabled = !doc || doc.read_only || doc.kind !== "pdf";
    }
    byId("menu-revert").toggleAttribute("disabled", !doc?.dirty || !!doc.read_only);
    byId("menu-close").toggleAttribute("disabled", !doc);
    byId("menu-insert-pages").toggleAttribute("disabled", !doc || doc.kind !== "pdf" || !!doc.read_only);
    byId("menu-extract-pages").toggleAttribute("disabled", !doc || doc.kind !== "pdf");
    byId("menu-print").toggleAttribute("disabled", !doc || isEpub);
    byId("menu-protect").toggleAttribute("disabled", !doc || doc.kind !== "pdf");
    byId("menu-autofill").toggleAttribute("disabled", !doc || doc.kind !== "pdf" || !!doc.read_only);
    byId("menu-ocr").toggleAttribute("disabled", !doc || doc.kind !== "pdf" || !!doc.read_only);
    byId("menu-redact").toggleAttribute("disabled", !doc || doc.kind !== "pdf" || !!doc.read_only);
    byId("menu-unlock").hidden = !doc || doc.kind !== "pdf" || !doc.encrypted;
    byId("page-count-label").textContent = String(doc?.page_count ?? 0);
    byId("page-total").textContent = `/ ${doc?.page_count ?? 0}`;
    byId<HTMLInputElement>("page-number").max = String(doc?.page_count ?? 1);
    byId<HTMLInputElement>("page-number").value = doc ? String(this.currentPageNumber() + 1) : "1";
    byId("page-number").setAttribute("aria-label", isEpub ? "Reading position" : "Page number");
    byId("status-page").textContent = doc
      ? (isEpub ? `Position ${this.currentPageNumber() + 1} of ${doc.page_count}` : `Page ${this.currentPageNumber() + 1} of ${doc.page_count}`)
      : "No document";
    byId("status-dims").textContent = isEpub ? this.epubChapter || "EPUB" : this.pageDimensionLabel(doc);
    if (!doc || doc.kind !== "pdf") byId("forms-section").hidden = true;
    const info = byId("doc-facts");
    info.replaceChildren();
    if (doc) {
      const facts: [string, string][] = [
        ["Type", doc.kind === "pdf" ? "PDF document" : doc.kind === "epub" ? "EPUB book" : `${doc.image?.format ?? "Image"} image`],
        [isEpub ? "Reading positions" : "Pages", isEpub ? `${doc.page_count} approximate positions` : String(doc.page_count)],
        ["File", doc.path ?? doc.name],
        ...(doc.kind === "image" && doc.image ? [["Dimensions", `${doc.image.width} × ${doc.image.height} px`] as [string, string]] : []),
        ...(doc.signed ? [["Signature", "Present; saving edits invalidates it"] as [string, string]] : []),
        ...(doc.read_only ? [["Access", isEpub ? "Read-only book" : "Read-only encrypted PDF"] as [string, string]] : []),
      ];
      for (const [key, value] of facts) {
        const dt = document.createElement("dt");
        dt.textContent = key;
        const dd = document.createElement("dd");
        dd.textContent = value;
        dd.title = value;
        info.append(dt, dd);
      }
    }
    void getCurrentWindow().setTitle(doc ? `${doc.dirty ? "• " : ""}${doc.name} — Preview` : "Preview").catch(() => undefined);
    this.rail.setActive(doc?.kind === "epub" ? -1 : doc ? this.currentPageNumber() : -1, false);
    this.renderMetadata();
    if (isEpub) requestAnimationFrame(() => {
      if (this.document?.id === doc?.id) this.epubReader.resize();
    });
    if (this.imageAdjustmentsOpen && doc?.id === this.imageAdjustmentsDocumentId) {
      this.previewImageAdjustments();
    }
  }

  private pageDimensionLabel(doc: DocumentInfo | null): string {
    if (!doc) return "";
    const page = doc.pages[this.currentPageNumber()];
    if (!page) return "";
    if (doc.kind === "image") return `${Math.round(page.width)} × ${Math.round(page.height)} px`;
    return `${Math.round(page.width)} × ${Math.round(page.height)} pt`;
  }

  private pageChanged(index: number): void {
    if (!this.document) return;
    this.lastPages.set(this.document.id, index);
    byId<HTMLInputElement>("page-number").value = String(index + 1);
    byId("status-page").textContent = `Page ${index + 1} of ${this.document.page_count}`;
    byId("status-dims").textContent = this.pageDimensionLabel(this.document);
    byId<HTMLButtonElement>("btn-prev-page").disabled = index <= 0;
    byId<HTMLButtonElement>("btn-next-page").disabled = index >= this.document.page_count - 1;
    this.rail.setActive(index);
  }

  private zoomChanged(percent: number): void {
    byId("zoom-label").textContent = `${Math.round(percent)}%`;
    byId("status-zoom").textContent = `${Math.round(percent)}%`;
  }

  private openFind(): void {
    if (!this.document || this.document.kind !== "pdf") return;
    byId("findbar").hidden = false;
    byId<HTMLInputElement>("find-input").focus();
    byId<HTMLInputElement>("find-input").select();
  }

  private closeFind(): void {
    byId("findbar").hidden = true;
    byId<HTMLInputElement>("find-input").value = "";
    this.findMatches = [];
    this.findIndex = -1;
    this.viewer.clearSearch();
    this.updateFindCount();
  }

  private scheduleFind(): void {
    window.clearTimeout(this.findTimer);
    const query = byId<HTMLInputElement>("find-input").value.trim();
    if (!query || !this.document) {
      this.findMatches = [];
      this.findIndex = -1;
      this.viewer.clearSearch();
      this.updateFindCount();
      return;
    }
    this.findTimer = window.setTimeout(() => void this.runFind(query), 180);
  }

  private async runFind(query: string): Promise<void> {
    if (!this.document) return;
    const request = ++this.findRequest;
    try {
      const matches = await api.findText(this.document.id, query);
      if (request !== this.findRequest || query !== byId<HTMLInputElement>("find-input").value.trim()) return;
      this.findMatches = matches;
      this.findIndex = matches.length ? 0 : -1;
      this.viewer.setSearch(matches, this.findIndex);
      this.updateFindCount();
      if (matches.length) this.viewer.goToMatch(0);
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  }

  private stepFind(direction: -1 | 1): void {
    if (this.findMatches.length === 0) return;
    this.findIndex = (this.findIndex + direction + this.findMatches.length) % this.findMatches.length;
    this.viewer.setSearch(this.findMatches, this.findIndex);
    this.viewer.goToMatch(this.findIndex);
    this.updateFindCount();
  }

  private updateFindCount(): void {
    byId("find-count").textContent = this.findMatches.length
      ? `${this.findIndex + 1} of ${this.findMatches.length}`
      : "0 of 0";
    byId("find-prev").toggleAttribute("disabled", this.findMatches.length === 0);
    byId("find-next").toggleAttribute("disabled", this.findMatches.length === 0);
  }

  private toggleMenu(id: string, anchorId: string): void {
    const menu = byId(id);
    const open = menu.hidden;
    this.hideMenus();
    if (!open) return;
    const anchor = byId(anchorId).getBoundingClientRect();
    menu.hidden = false;
    const rect = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(8, Math.min(window.innerWidth - rect.width - 8, anchor.left))}px`;
    menu.style.top = `${Math.min(window.innerHeight - rect.height - 8, anchor.bottom + 6)}px`;
  }

  private hideMenus(): void {
    byId("app-menu").hidden = true;
    byId("zoom-menu").hidden = true;
  }

  private onKeyDown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      this.hideMenus();
      if (!document.querySelector("dialog[open]")) {
        event.preventDefault();
        void this.closeWindow();
      }
      return;
    }
    const command = event.ctrlKey || event.metaKey;
    if (command && event.key.toLowerCase() === "w") {
      if (document.querySelector("dialog[open]")) return;
      event.preventDefault();
      void this.closeWindow();
      return;
    }
    if (command && event.key.toLowerCase() === "o") {
      event.preventDefault();
      void this.pickAndOpen();
      return;
    }
    if (command && event.key.toLowerCase() === "s") {
      event.preventDefault();
      if (event.shiftKey) void this.showExport();
      else void this.saveCurrent();
      return;
    }
    if (command && event.key.toLowerCase() === "f") {
      event.preventDefault();
      this.openFind();
      return;
    }
    if (command && event.key.toLowerCase() === "z") {
      event.preventDefault();
      if (event.shiftKey) void this.redo();
      else void this.undo();
      return;
    }
    if (command && (event.key === "+" || event.key === "=")) {
      event.preventDefault();
      this.zoomIn();
      return;
    }
    if (command && event.key === "-") {
      event.preventDefault();
      this.zoomOut();
      return;
    }
    if (command && event.key === "0") {
      event.preventDefault();
      this.setZoom(100);
      return;
    }
    if (!isEditableTarget(event.target) && (this.slideshowTimer || document.fullscreenElement) && (event.key === "ArrowRight" || event.key === "ArrowLeft")) {
      event.preventDefault();
      this.goToPage(this.currentPageNumber() + (event.key === "ArrowRight" ? 1 : -1));
      return;
    }
    if (!isEditableTarget(event.target) && event.key.toLowerCase() === "f" && this.document) {
      event.preventDefault();
      void this.toggleFullscreen();
      return;
    }
    if (!isEditableTarget(event.target) && event.key.toLowerCase() === "s" && this.document?.page_count && this.document.page_count > 1) {
      event.preventDefault();
      void this.toggleSlideshow();
      return;
    }
    if (isEditableTarget(event.target)) return;
    if (event.key === "PageDown") {
      event.preventDefault();
      this.goToPage(this.currentPageNumber() + 1);
    } else if (event.key === "PageUp") {
      event.preventDefault();
      this.goToPage(this.currentPageNumber() - 1);
    } else if (event.key === "Delete" || event.key === "Backspace") {
      if (this.selected) {
        event.preventDefault();
        void this.removeSelected();
      }
    }
  }

  private async closeWindow(): Promise<void> {
    if (this.closingWindow) return;
    this.closingWindow = true;
    let closeRequested = false;
    try {
      if (this.opening || this.operating) {
        toast("Wait for the current document operation to finish before closing Preview.", "error");
        return;
      }
      const originalDocumentId = this.document?.id;
      const dirtyDocumentIds = Array.from(this.documents.values())
        .filter((doc) => doc.dirty)
        .map((doc) => doc.id);

      for (const id of dirtyDocumentIds) {
        const doc = this.documents.get(id);
        if (!doc?.dirty) continue;
        if (this.document?.id !== id) {
          await this.activateDocument(id);
          if (this.document?.id !== id) return;
        }
        const answer = await choose(
          "Unsaved changes",
          `Save changes to “${doc.name}” before closing Preview?`,
          [
            { value: "cancel", label: "Cancel", style: "ghost" },
            { value: "discard", label: "Discard", style: "danger" },
            { value: "save", label: "Save", style: "primary" },
          ],
        );
        if (answer === "cancel") {
          if (originalDocumentId && this.document?.id !== originalDocumentId) {
            await this.activateDocument(originalDocumentId);
          }
          return;
        }
        if (answer === "save" && !(await this.saveCurrent())) {
          if (originalDocumentId && this.document?.id !== originalDocumentId) {
            await this.activateDocument(originalDocumentId);
          }
          return;
        }
      }

      await getCurrentWindow().close();
      closeRequested = true;
    } catch (error) {
      toast(`Could not close Preview: ${errorMessage(error)}`, "error");
    } finally {
      if (!closeRequested) this.closingWindow = false;
    }
  }

  private setBusy(busy: boolean, message = ""): void {
    document.body.classList.toggle("busy", busy);
    byId("status-message").textContent = message;
    byId("btn-open").toggleAttribute("disabled", busy);
    byId("btn-save").toggleAttribute("disabled", busy || !this.document || !this.document.dirty || !!this.document.read_only);
    byId("btn-export").toggleAttribute("disabled", busy || !this.document || this.document.kind === "epub");
  }
}
