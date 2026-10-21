// Batch workflow from Manage: apply a develop preset to the selection and
// rename files with a template (with a live preview before anything moves).

import { api } from "./api.js";
import { store, emit, toast, el } from "./state.js";

const $ = (id) => document.getElementById(id);
const plural = (n, w) => `${n} ${w}${n === 1 ? "" : "s"}`;

function runDialog(dlg) {
  dlg.returnValue = ""; // Esc must not reuse the previous dialog's "ok"
  dlg.showModal();
  return new Promise((r) => dlg.addEventListener("close", () => r(dlg.returnValue === "ok"), { once: true }));
}

// ----------------------------------------------------------------- presets

let presetCache = null;

export async function loadPresets(force = false) {
  if (!presetCache || force) presetCache = await api.presets();
  return presetCache;
}

/** Mirrors `workflow::apply_preset`: the photo keeps crop, lens and masks. */
export const applyLook = (base, look) => ({
  ...JSON.parse(JSON.stringify(look)),
  geometry: base.geometry,
  lens: base.lens,
  local: base.local,
});

async function applyPresetDialog() {
  const ids = store.selectedIds();
  if (!ids.length) return toast("Select photos first");
  const presets = await loadPresets(true);
  const sel = $("preset-select");
  sel.replaceChildren(
    ...presets.map((p, i) => el("option", { value: i }, p.id == null ? p.name : `${p.name} (yours)`)));
  $("preset-count").textContent = `${plural(ids.length, "photo")} selected`;
  if (!(await runDialog($("preset-dialog")))) return;
  const preset = presets[Number(sel.value)];
  try {
    await api.applyPreset(ids, preset.settings);
    ids.forEach((id) => emit("asset-developed", id));
    emit("library-changed");
    toast(`“${preset.name}” applied to ${plural(ids.length, "photo")}`);
  } catch (e) {
    toast(`Preset failed: ${e}`, true);
  }
}

// ------------------------------------------------------------------ rename

const RENAME_KEY = "prism.rename";
let previewToken = 0;
let previewTimer = null;
let lastOk = 0;

function renameArgs() {
  return {
    template: $("rename-template").value,
    start: Math.max(0, Math.min(999999, Math.round(Number($("rename-start").value) || 0))),
  };
}

const base = (p) => p.split(/[\\/]/).pop();

async function refreshRenamePreview(ids) {
  const token = ++previewToken;
  const { template, start } = renameArgs();
  const box = $("rename-preview");
  try {
    const plans = await api.renamePreview(ids, template, start);
    if (token !== previewToken) return;
    const changing = plans.filter((p) => !p.problem && p.from !== p.to).length;
    const problems = plans.filter((p) => p.problem).length;
    lastOk = changing;
    box.replaceChildren(el("table", {},
      ...plans.slice(0, 200).map((p) =>
        el("tr", { class: p.problem ? "problem" : p.from === p.to ? "same" : "" },
          el("td", {}, base(p.from)),
          el("td", { class: "arrow" }, "→"),
          el("td", {}, p.problem ? `${base(p.to)} — ${p.problem}` : base(p.to))))));
    $("rename-summary").textContent = [
      `${plural(changing, "file")} will be renamed`,
      problems ? `${problems} skipped` : null,
      plans.length > 200 ? `showing first 200 of ${plans.length}` : null,
    ].filter(Boolean).join(" · ");
    $("rename-ok").disabled = changing === 0;
  } catch (e) {
    if (token !== previewToken) return;
    lastOk = 0;
    box.replaceChildren();
    $("rename-summary").textContent = String(e);
    $("rename-ok").disabled = true;
  }
}

async function renameDialog() {
  const ids = store.selectedInOrder();
  if (!ids.length) return toast("Select photos first");
  try {
    const saved = JSON.parse(localStorage.getItem(RENAME_KEY) || "null");
    if (saved?.template) $("rename-template").value = saved.template;
  } catch {
    /* storage unavailable */
  }
  const schedule = () => {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(() => refreshRenamePreview(ids), 150);
  };
  $("rename-template").oninput = schedule;
  $("rename-start").oninput = schedule;
  await refreshRenamePreview(ids);
  const ok = await runDialog($("rename-dialog"));
  clearTimeout(previewTimer);
  const { template, start } = renameArgs();
  try {
    localStorage.setItem(RENAME_KEY, JSON.stringify({ template }));
  } catch {
    /* storage unavailable */
  }
  if (!ok || !lastOk) return;
  try {
    const res = await api.renameApply(ids, template, start);
    emit("library-changed");
    const msg = [`Renamed ${plural(res.renamed, "file")}`];
    if (res.failed.length) msg.push(`${res.failed.length} not renamed: ${res.failed[0]}`);
    toast(msg.join(" · "), res.failed.length > 0);
  } catch (e) {
    toast(`Rename failed: ${e}`, true);
  }
}

export function init() {
  $("manage-preset").onclick = applyPresetDialog;
  $("manage-rename").onclick = renameDialog;
}
