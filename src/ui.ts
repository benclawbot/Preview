export function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing required element #${id}`);
  return element as T;
}

export function toast(message: string, kind: "info" | "error" | "success" = "info"): void {
  const host = byId("toasts");
  const item = document.createElement("div");
  item.className = `toast toast-${kind}`;
  item.textContent = message;
  host.append(item);
  requestAnimationFrame(() => item.classList.add("visible"));
  window.setTimeout(() => {
    item.classList.remove("visible");
    window.setTimeout(() => item.remove(), 220);
  }, kind === "error" ? 6500 : 3600);
}

export interface Choice {
  value: string;
  label: string;
  style?: "primary" | "danger" | "ghost";
}

export function choose(
  title: string,
  message: string,
  choices: Choice[],
): Promise<string | null> {
  const dialog = byId<HTMLDialogElement>("confirm-dialog");
  const form = byId<HTMLFormElement>("confirm-form");
  byId("confirm-title").textContent = title;
  byId("confirm-message").textContent = message;
  const actions = byId("confirm-actions");
  actions.replaceChildren();
  for (const choice of choices) {
    const button = document.createElement("button");
    button.type = "submit";
    button.value = choice.value;
    button.dataset.choice = choice.value;
    button.className = `btn ${choice.style ?? "ghost"}`;
    button.textContent = choice.label;
    actions.append(button);
  }

  return new Promise((resolve) => {
    let result: string | null = null;
    form.onsubmit = (event) => {
      event.preventDefault();
      const submitter = (event as SubmitEvent).submitter as HTMLButtonElement | null;
      result = submitter?.dataset.choice ?? null;
      dialog.close();
    };
    dialog.onclose = () => {
      form.onsubmit = null;
      dialog.onclose = null;
      resolve(result);
    };
    dialog.showModal();
    actions.querySelector<HTMLButtonElement>("button")?.focus();
  });
}

export function askPassword(showError = false): Promise<string | null> {
  const dialog = byId<HTMLDialogElement>("password-dialog");
  const form = byId<HTMLFormElement>("password-form");
  const input = byId<HTMLInputElement>("password-input");
  const error = byId("password-error");
  input.value = "";
  error.hidden = !showError;
  return new Promise((resolve) => {
    let result: string | null = null;
    form.onsubmit = (event) => {
      event.preventDefault();
      result = input.value;
      dialog.close();
    };
    dialog.onclose = () => {
      form.onsubmit = null;
      dialog.onclose = null;
      resolve(result);
    };
    dialog.showModal();
    input.focus();
  });
}

export function askText(
  title: string,
  hint: string,
  initial = "",
  submitLabel = "Done",
): Promise<string | null> {
  const dialog = byId<HTMLDialogElement>("text-dialog");
  const form = byId<HTMLFormElement>("text-form");
  const input = byId<HTMLTextAreaElement>("text-value");
  byId("text-title").textContent = title;
  byId("text-hint").textContent = hint;
  byId("text-submit").textContent = submitLabel;
  input.value = initial;
  return new Promise((resolve) => {
    let result: string | null = null;
    form.onsubmit = (event) => {
      event.preventDefault();
      result = input.value;
      dialog.close();
    };
    dialog.onclose = () => {
      form.onsubmit = null;
      dialog.onclose = null;
      resolve(result);
    };
    dialog.showModal();
    input.focus();
    input.setSelectionRange(input.value.length, input.value.length);
  });
}

export function cancelDialogs(): void {
  for (const dialog of document.querySelectorAll<HTMLDialogElement>("dialog[open]")) {
    dialog.close();
  }
}
