// Shared UI state + a tiny event bus. Modules subscribe to the events they
// care about instead of calling each other directly.

export const bus = new EventTarget();
export const emit = (type, detail) => bus.dispatchEvent(new CustomEvent(type, { detail }));
export const on = (type, fn) => bus.addEventListener(type, (e) => fn(e.detail));

export const store = {
  mode: "manage",
  /** Current SearchQuery (camelCase, mirrors prism_core::SearchQuery). */
  query: { sort: "name", descending: false },
  /** Human description of the current source (folder, collection…). */
  scope: "All photos",
  /** Manage layout: "grid" or "map". */
  layout: "grid",
  /** Active smart (CLIP) search text, or null. */
  smart: null,
  assets: [],
  selected: new Set(),
  /** Index into `assets` of the focused photo. */
  focus: -1,

  get current() {
    return this.assets[this.focus] ?? null;
  },
  selectedIds() {
    if (this.selected.size) return [...this.selected];
    return this.current ? [this.current.id] : [];
  },
  /** Selected ids in grid order, so numbered names follow what the user sees. */
  selectedInOrder() {
    const sel = new Set(this.selectedIds());
    return this.assets.filter((a) => sel.has(a.id)).map((a) => a.id);
  },
  replaceAsset(a) {
    const i = this.assets.findIndex((x) => x.id === a.id);
    if (i >= 0) this.assets[i] = a;
  },
};

export const LABELS = [
  { id: 1, name: "Red", color: "var(--label-1)" },
  { id: 2, name: "Yellow", color: "var(--label-2)" },
  { id: 3, name: "Green", color: "var(--label-3)" },
  { id: 4, name: "Blue", color: "var(--label-4)" },
  { id: 5, name: "Purple", color: "var(--label-5)" },
];

let toastTimer;
export function toast(msg, error = false) {
  const t = document.getElementById("toast");
  t.textContent = msg;
  t.className = "toast show" + (error ? " error" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.className = "toast"), error ? 5000 : 2200);
}

export function promptText(label, value = "") {
  const dlg = document.getElementById("prompt-dialog");
  document.getElementById("prompt-label").textContent = label;
  const input = document.getElementById("prompt-input");
  input.value = value;
  dlg.returnValue = ""; // Esc must not reuse the previous dialog's "ok"
  dlg.showModal();
  input.select();
  return new Promise((resolve) => {
    dlg.addEventListener("close", () => resolve(dlg.returnValue === "ok" ? input.value.trim() : null), { once: true });
  });
}

export const el = (tag, attrs = {}, ...children) => {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else if (k === "style") n.style.cssText = v;
    else if (v !== false && v != null) n.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) if (c != null) n.append(c.nodeType ? c : document.createTextNode(c));
  return n;
};

export const fmt = {
  bytes: (n) => (n > 1e6 ? (n / 1e6).toFixed(1) + " MB" : Math.round(n / 1e3) + " KB"),
  shutter: (t) => (t == null ? null : t >= 1 ? `${t}s` : `1/${Math.round(1 / t)}s`),
  stars: (r) => "★".repeat(r) + "☆".repeat(5 - r),
};
