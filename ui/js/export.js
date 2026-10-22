// Export dialog (shared by Manage and Develop): format, size, file-name
// template and embedded metadata. Each photo is rendered with its develop
// settings, including any text on the photo. Originals are untouched.

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
}

function syncUi() {
  const p = readForm();
  $("exp-quality-wrap").hidden = p.format !== "jpeg";
  $("exp-gps").disabled = !p.metadata;
  $("exp-copyright").disabled = !p.metadata;
}

let wired = false;

function wire() {
  if (wired) return;
  wired = true;
  for (const id of ["exp-format", "exp-meta"]) $(id).addEventListener("input", syncUi);
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
  const missing = store.assets.filter((a) => ids.includes(a.id) && a.needsPlace).length;
  $("exp-count").textContent = `${ids.length} photo${ids.length === 1 ? "" : "s"} · with their edits and text · originals untouched`
    + (missing ? ` · ⚠ ${missing} still missing a place` : "");
  syncUi();
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
