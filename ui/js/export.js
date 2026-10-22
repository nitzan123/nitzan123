// Export dialog (shared by Manage and Develop): format, size and file-name
// template, plus an optional caption burned into the exported copies —
// capture date, place name from GPS (OpenStreetMap) and custom text.
// Originals are untouched.

import { api } from "./api.js";
import { store, toast } from "./state.js";

const $ = (id) => document.getElementById(id);
const PREFS_KEY = "prism.export";

const DEFAULT_PREFS = {
  format: "jpeg",
  names: "",
  metadata: true,
  gps: true,
  copyright: "",
  quality: 92,
  size: 0,
  caption: false,
  date: true,
  dateFormat: "long",
  location: true,
  placeLanguage: "en",
  customText: "",
  corner: "bottomRight",
  textSize: 3,
};

function loadPrefs() {
  try {
    return { ...DEFAULT_PREFS, ...JSON.parse(localStorage.getItem(PREFS_KEY) || "{}") };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

function savePrefs(p) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(p));
  } catch {
    /* storage unavailable: preferences just won't persist */
  }
}

function readForm() {
  return {
    format: $("exp-format").value,
    names: $("exp-names").value.trim(),
    metadata: $("exp-meta").checked,
    gps: $("exp-gps").checked,
    copyright: $("exp-copyright").value.trim(),
    quality: Math.round(Math.min(100, Math.max(10, Number($("exp-quality").value) || 92))),
    size: Math.round(Math.min(20000, Math.max(0, Number($("exp-size").value) || 0))),
    caption: $("cap-enabled").checked,
    date: $("cap-date").checked,
    dateFormat: $("cap-date-format").value,
    location: $("cap-location").checked,
    placeLanguage: $("cap-lang").value,
    customText: $("cap-text").value,
    corner: $("cap-corner").value,
    textSize: Number($("cap-size").value),
  };
}

function writeForm(p) {
  $("exp-format").value = p.format;
  $("exp-names").value = p.names;
  $("exp-meta").checked = p.metadata;
  $("exp-gps").checked = p.gps;
  $("exp-copyright").value = p.copyright;
  $("exp-quality").value = p.quality;
  $("exp-size").value = p.size;
  $("cap-enabled").checked = p.caption;
  $("cap-date").checked = p.date;
  $("cap-date-format").value = p.dateFormat;
  $("cap-location").checked = p.location;
  $("cap-lang").value = p.placeLanguage;
  $("cap-text").value = p.customText;
  $("cap-corner").value = p.corner;
  $("cap-size").value = p.textSize;
}

/** Form → `prism_core::overlay::Caption` (or null when disabled). */
function captionFrom(p) {
  if (!p.caption) return null;
  return {
    date: p.date,
    dateFormat: p.dateFormat,
    location: p.location,
    customText: p.customText,
    corner: p.corner,
    size: p.textSize,
    opacity: 95,
    placeLanguage: p.placeLanguage,
  };
}

let previewToken = 0;
let previewTimer = null;

function syncCaptionUi() {
  const p = readForm();
  $("exp-quality-wrap").hidden = p.format !== "jpeg";
  $("exp-gps").disabled = !p.metadata;
  $("exp-copyright").disabled = !p.metadata;
  $("cap-options").hidden = !p.caption;
  $("cap-privacy").hidden = !(p.caption && p.location);
  clearTimeout(previewTimer);
  previewTimer = setTimeout(refreshPreview, 250);
}

async function refreshPreview() {
  const img = $("cap-preview");
  const note = $("cap-note");
  const caption = captionFrom(readForm());
  const target = store.current;
  if (!caption || !target) {
    img.hidden = true;
    note.textContent = "";
    return;
  }
  const token = ++previewToken;
  note.textContent = "Rendering preview…";
  try {
    const res = await api.captionPreview(target.id, caption);
    if (token !== previewToken) return;
    img.src = res.dataUrl;
    img.hidden = false;
    note.textContent = res.note ? `Preview (${target.fileName}): ${res.note}` : `Preview: ${target.fileName}`;
  } catch (e) {
    if (token === previewToken) note.textContent = `Preview failed: ${e}`;
  }
}

let wired = false;

function wire() {
  if (wired) return;
  wired = true;
  for (const id of ["exp-format", "exp-meta", "cap-enabled", "cap-date", "cap-date-format", "cap-location", "cap-lang", "cap-corner", "cap-size"]) {
    $(id).addEventListener("input", syncCaptionUi);
  }
  $("cap-text").addEventListener("input", syncCaptionUi);
  api.listen("export-progress", ({ done, total }) => {
    if (total > 1) toast(`Exporting ${done}/${total}…`);
  });
}

/** Opens the dialog for the current selection. `beforeExport` flushes edits. */
export async function openExportDialog(beforeExport = async () => {}) {
  wire();
  await beforeExport();
  const ids = store.selectedInOrder();
  if (!ids.length) return toast("Select photos to export");
  const dlg = $("export-dialog");
  writeForm(loadPrefs());
  $("exp-count").textContent = `${ids.length} photo${ids.length === 1 ? "" : "s"} · develop settings applied · originals untouched · name tokens: {name} {seq:3} {date} {time} {camera}`;
  syncCaptionUi();
  dlg.returnValue = ""; // Esc must not reuse the previous dialog's "ok"
  dlg.showModal();
  const ok = await new Promise((r) => dlg.addEventListener("close", () => r(dlg.returnValue === "ok"), { once: true }));
  const prefs = readForm();
  savePrefs(prefs);
  if (!ok) return;
  const dir = await api.pickFolder("Export to folder");
  if (!dir) return;
  toast(`Exporting ${ids.length}…`);
  try {
    const res = await api.export(ids, dir, {
      format: prefs.format,
      quality: prefs.quality,
      maxDim: prefs.size,
      caption: captionFrom(prefs),
      nameTemplate: prefs.names || null,
      metadata: prefs.metadata,
      includeGps: prefs.gps,
      copyright: prefs.copyright || null,
    });
    const parts = [`Exported ${res.written.length} to ${dir}`];
    if (res.failed.length) parts.push(`failed ${res.failed.length}: ${res.failed[0]}`);
    parts.push(...res.notes);
    toast(parts.join(" · "), res.failed.length > 0);
  } catch (e) {
    toast(String(e), true);
  }
}
