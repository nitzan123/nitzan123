// Develop + Edit workspace: non-destructive settings, live preview,
// undo/redo, autosave, export. Edit mode reuses this workspace and swaps the
// right-hand panel for local adjustments (see edit.js).

import { api } from "./api.js";
import { store, emit, on, el, toast } from "./state.js";
import { slider, section, tabs, drawHistogram, curveEditor } from "./controls.js";
import { renderFilmstrip } from "./view.js";
import { setFocus } from "./manage.js";
import * as edit from "./edit.js";
import { openExportDialog } from "./export.js";

const $ = (id) => document.getElementById(id);

/** Mirrors `DevelopSettings::default()` in prism-core. */
export const defaults = () => ({
  version: 1,
  whiteBalance: { temperature: 0, tint: 0 },
  light: { exposure: 0, contrast: 0, highlights: 0, shadows: 0, whites: 0, blacks: 0, clarity: 0 },
  color: { vibrance: 0, saturation: 0 },
  mixer: { hue: Array(8).fill(0), saturation: Array(8).fill(0), luminance: Array(8).fill(0) },
  curve: { points: [[0, 0], [1, 1]] },
  splitTone: { highlightHue: 45, highlightSaturation: 0, shadowHue: 220, shadowSaturation: 0, balance: 0 },
  detail: { sharpenAmount: 0, sharpenRadius: 1, sharpenThreshold: 0, noiseReduction: 0 },
  vignette: { amount: 0, midpoint: 50, feather: 50 },
  geometry: { rotate90: 0, flipHorizontal: false, straighten: 0, crop: null },
  local: [],
});

const BANDS = [
  ["Red", "#e5534b"], ["Orange", "#ee8a3c"], ["Yellow", "#e8c547"], ["Green", "#57b65f"],
  ["Aqua", "#4cc3c3"], ["Blue", "#4b8fe5"], ["Purple", "#8c6ee0"], ["Magenta", "#d062b8"],
];

export const dev = {
  id: null,
  s: defaults(),
  committed: "",
  undo: [],
  redo: [],
  before: false,
  inflight: false,
  pending: false,
  saveTimer: null,
  dirty: false,
  fullAspect: null,
  lastPreview: null,
};

const bindings = []; // { get, ctrl }
let curve;

// ----------------------------------------------------------- path helpers

const getPath = (obj, path) => path.split(".").reduce((o, k) => o?.[k], obj);
function setPath(obj, path, v) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((o, k) => o[k], obj)[last] = v;
}

function bind(path, opts) {
  const ctrl = slider({
    ...opts,
    value: getPath(dev.s, path),
    onInput: (v) => change((s) => setPath(s, path, v)),
    onCommit: commit,
  });
  bindings.push({ get: () => getPath(dev.s, path), ctrl });
  return ctrl;
}

function refreshControls() {
  for (const b of bindings) b.ctrl.update(b.get());
  curve?.update(dev.s.curve.points);
  edit.render(ctx);
}

// ------------------------------------------------------- change / commit

export function change(mutator) {
  mutator(dev.s);
  dev.dirty = true;
  preview();
  scheduleSave();
}

export function commit() {
  const now = JSON.stringify(dev.s);
  if (now === dev.committed) return;
  dev.undo.push(dev.committed);
  if (dev.undo.length > 200) dev.undo.shift();
  dev.redo = [];
  dev.committed = now;
}

function restore(json) {
  dev.s = JSON.parse(json);
  dev.committed = json;
  dev.dirty = true;
  refreshControls();
  preview();
  scheduleSave();
}

function undo() {
  if (!dev.undo.length) return;
  dev.redo.push(dev.committed);
  restore(dev.undo.pop());
}

function redo() {
  if (!dev.redo.length) return;
  dev.undo.push(dev.committed);
  restore(dev.redo.pop());
}

// ----------------------------------------------------------- persistence

function scheduleSave() {
  clearTimeout(dev.saveTimer);
  dev.saveTimer = setTimeout(flushSave, 600);
}

export async function flushSave() {
  clearTimeout(dev.saveTimer);
  if (!dev.dirty || dev.id == null) return;
  const id = dev.id;
  dev.dirty = false;
  try {
    await api.developSave(id, dev.s);
    $("dev-status").textContent = "Saved";
    emit("asset-developed", id);
  } catch (e) {
    dev.dirty = true;
    toast(`Save failed: ${e}`, true);
  }
}

async function load(asset) {
  await flushSave();
  dev.id = asset.id;
  dev.fullAspect = null;
  dev.before = false;
  $("dev-before").classList.remove("active");
  $("dev-name").textContent = asset.fileName;
  $("dev-status").textContent = "";
  try {
    dev.s = Object.assign(defaults(), await api.developGet(asset.id));
  } catch (e) {
    toast(String(e), true);
    dev.s = defaults();
  }
  dev.committed = JSON.stringify(dev.s);
  dev.undo = [];
  dev.redo = [];
  dev.dirty = false;
  edit.reset();
  refreshControls();
  renderFilmstrip($("dev-strip"));
  preview();
}

// --------------------------------------------------------------- preview

export async function preview() {
  if (dev.id == null) return;
  if (dev.inflight) {
    dev.pending = true;
    return;
  }
  dev.inflight = true;
  $("dev-status").textContent = "Rendering…";
  const stage = $("dev-stage").getBoundingClientRect();
  const max = Math.min(2048, Math.max(512, Math.round(Math.max(stage.width, stage.height) * devicePixelRatio)));
  const id = dev.id;
  const settings = dev.before ? defaults() : dev.s;
  try {
    const p = await api.developPreview(id, settings, max);
    if (id === dev.id) {
      const img = $("dev-img");
      img.src = p.dataUrl;
      await img.decode().catch(() => {});
      dev.lastPreview = p;
      if (!settings.geometry.crop) dev.fullAspect = p.width / p.height;
      drawHistogram($("histogram"), p.histogram);
      curve?.setHistogram(p.histogram);
      edit.drawOverlay(ctx);
      $("dev-status").textContent = dev.dirty ? "" : "Saved";
    }
  } catch (e) {
    $("dev-status").textContent = "";
    toast(`Preview failed: ${e}`, true);
  }
  dev.inflight = false;
  if (dev.pending) {
    dev.pending = false;
    preview();
  }
}

// ---------------------------------------------------------------- panel

function frameAspect() {
  if (dev.fullAspect) return dev.fullAspect;
  const a = store.current;
  let r = a?.width && a?.height ? a.width / a.height : 1.5;
  if ((a?.orientation ?? 1) >= 5) r = 1 / r;
  if (dev.s.geometry.rotate90 % 2) r = 1 / r;
  return r;
}

function cropTo(ratio) {
  change((s) => {
    if (!ratio) {
      s.geometry.crop = null;
      return;
    }
    const A = frameAspect();
    const r = ratio === "orig" ? A : ratio;
    const [w, h] = r > A ? [1, A / r] : [r / A, 1];
    s.geometry.crop = { x: (1 - w) / 2, y: (1 - h) / 2, w, h };
  });
  commit();
  refreshControls();
}

function cropSlider(key, label) {
  const ctrl = slider({
    label, min: 0, max: 100, step: 0.5, def: key === "w" || key === "h" ? 100 : 0,
    value: 0,
    onInput: (v) => change((s) => {
      const c = s.geometry.crop ?? { x: 0, y: 0, w: 1, h: 1 };
      c[key] = v / 100;
      c.w = Math.min(c.w, 1 - c.x);
      c.h = Math.min(c.h, 1 - c.y);
      s.geometry.crop = c;
    }),
    onCommit: commit,
  });
  bindings.push({ get: () => (dev.s.geometry.crop?.[key] ?? (key === "w" || key === "h" ? 1 : 0)) * 100, ctrl });
  return ctrl;
}

function buildPanel() {
  const p = $("dev-panel");
  const mixerGroups = ["hue", "saturation", "luminance"].map((kind, gi) => {
    const g = el("div", { hidden: gi !== 0 },
      ...BANDS.map(([name, color], i) =>
        bind(`mixer.${kind}.${i}`, { label: name, min: -100, max: 100, color })));
    return g;
  });
  curve = curveEditor({
    points: dev.s.curve.points,
    onInput: (pts) => change((s) => (s.curve.points = pts)),
    onCommit: commit,
  });
  const btn = (label, fn, title) => el("button", { class: "btn small", onclick: fn, title }, label);

  p.replaceChildren(
    el("div", { class: "panel row gap" },
      el("button", { class: "btn small primary", onclick: auto, title: "Automatic exposure, range and presence (keeps white balance)" }, "Auto tone"),
      el("button", { class: "btn small", onclick: autoWb, title: "Conservative automatic white balance" }, "Auto WB"),
      btn("Copy", copySettings, "Copy develop settings"),
      btn("Paste", pasteSettings, "Paste develop settings to the selection")),
    section("White balance", true,
      bind("whiteBalance.temperature", { label: "Temperature", min: -100, max: 100 }),
      bind("whiteBalance.tint", { label: "Tint", min: -100, max: 100 })),
    section("Light", true,
      bind("light.exposure", { label: "Exposure", min: -5, max: 5, step: 0.01 }),
      bind("light.contrast", { label: "Contrast", min: -100, max: 100 }),
      bind("light.highlights", { label: "Highlights", min: -100, max: 100 }),
      bind("light.shadows", { label: "Shadows", min: -100, max: 100 }),
      bind("light.whites", { label: "Whites", min: -100, max: 100 }),
      bind("light.blacks", { label: "Blacks", min: -100, max: 100 }),
      bind("light.clarity", { label: "Clarity", min: -100, max: 100 })),
    section("Color", true,
      bind("color.vibrance", { label: "Vibrance", min: -100, max: 100 }),
      bind("color.saturation", { label: "Saturation", min: -100, max: 100 })),
    section("Color mixer", false,
      tabs(["Hue", "Saturation", "Luminance"], (i) => mixerGroups.forEach((g, j) => (g.hidden = i !== j))),
      ...mixerGroups),
    section("Tone curve", false,
      curve,
      el("p", { class: "hint" }, "Click to add a point, drag to shape, double-click to remove."),
      btn("Reset curve", () => { change((s) => (s.curve.points = [[0, 0], [1, 1]])); commit(); refreshControls(); })),
    section("Split tone", false,
      bind("splitTone.highlightHue", { label: "Highlight hue", min: 0, max: 360, def: 45 }),
      bind("splitTone.highlightSaturation", { label: "Highlight sat.", min: 0, max: 100 }),
      bind("splitTone.shadowHue", { label: "Shadow hue", min: 0, max: 360, def: 220 }),
      bind("splitTone.shadowSaturation", { label: "Shadow sat.", min: 0, max: 100 }),
      bind("splitTone.balance", { label: "Balance", min: -100, max: 100 })),
    section("Detail", false,
      bind("detail.sharpenAmount", { label: "Sharpen", min: 0, max: 150 }),
      bind("detail.sharpenRadius", { label: "Radius", min: 0.5, max: 3, step: 0.1, def: 1 }),
      bind("detail.sharpenThreshold", { label: "Threshold", min: 0, max: 100 }),
      bind("detail.noiseReduction", { label: "Noise red.", min: 0, max: 100 })),
    section("Vignette", false,
      bind("vignette.amount", { label: "Amount", min: -100, max: 100 }),
      bind("vignette.midpoint", { label: "Midpoint", min: 0, max: 100, def: 50 }),
      bind("vignette.feather", { label: "Feather", min: 0, max: 100, def: 50 })),
    section("Geometry", false,
      el("div", { class: "btn-row" },
        btn("⟲ Rotate", () => { change((s) => (s.geometry.rotate90 = (s.geometry.rotate90 + 3) % 4, s.geometry.crop = null)); dev.fullAspect = null; commit(); refreshControls(); }),
        btn("⟳ Rotate", () => { change((s) => (s.geometry.rotate90 = (s.geometry.rotate90 + 1) % 4, s.geometry.crop = null)); dev.fullAspect = null; commit(); refreshControls(); }),
        btn("⇋ Flip", () => { change((s) => (s.geometry.flipHorizontal = !s.geometry.flipHorizontal)); commit(); })),
      bind("geometry.straighten", { label: "Straighten", min: -45, max: 45, step: 0.1 }),
      el("h3", {}, "Crop"),
      el("div", { class: "btn-row" },
        btn("Free", () => cropTo(null)), btn("Original", () => cropTo("orig")), btn("1:1", () => cropTo(1)),
        btn("4:5", () => cropTo(4 / 5)), btn("3:2", () => cropTo(3 / 2)), btn("16:9", () => cropTo(16 / 9))),
      cropSlider("x", "Left %"), cropSlider("y", "Top %"), cropSlider("w", "Width %"), cropSlider("h", "Height %")),
  );
}

// -------------------------------------------------------- copy / paste

let clipboard = null;

function copySettings() {
  const { local, ...rest } = JSON.parse(JSON.stringify(dev.s));
  clipboard = rest; // local masks are image-specific
  toast("Develop settings copied (without local masks)");
}

async function pasteSettings() {
  if (!clipboard) return toast("Nothing copied yet");
  const ids = store.selectedIds();
  for (const id of ids) {
    if (id === dev.id) continue;
    const cur = Object.assign(defaults(), await api.developGet(id));
    await api.developSave(id, { ...cur, ...JSON.parse(JSON.stringify(clipboard)), local: cur.local });
    emit("asset-developed", id);
  }
  if (ids.includes(dev.id)) {
    change((s) => Object.assign(s, JSON.parse(JSON.stringify(clipboard)), { local: s.local }));
    commit();
    refreshControls();
  }
  toast(`Pasted to ${ids.length} photo${ids.length === 1 ? "" : "s"}`);
}

async function auto() {
  try {
    const a = await api.developAuto(dev.id);
    change((s) => {
      s.light = a.light;
      s.color = a.color;
    });
    commit();
    refreshControls();
  } catch (e) {
    toast(String(e), true);
  }
}

async function autoWb() {
  try {
    const [temperature, tint] = await api.developAutoWb(dev.id);
    change((s) => (s.whiteBalance = { temperature, tint }));
    commit();
    refreshControls();
  } catch (e) {
    toast(String(e), true);
  }
}

// ------------------------------------------------------------------ init

/** Context handed to edit.js so it can mutate settings without cycles. */
const ctx = { dev, change, commit, preview, refresh: () => refreshControls() };

function toggleBefore() {
  dev.before = !dev.before;
  $("dev-before").classList.toggle("active", dev.before);
  preview();
}

export function init() {
  buildPanel();
  edit.init(ctx);
  $("dev-before").onclick = toggleBefore;
  $("dev-undo").onclick = undo;
  $("dev-redo").onclick = redo;
  $("dev-reset").onclick = () => {
    change((s) => Object.assign(s, defaults()));
    commit();
    refreshControls();
  };
  $("dev-export").onclick = () => openExportDialog(flushSave);
  new ResizeObserver(() => edit.drawOverlay(ctx)).observe($("dev-stage"));

  on("mode", async ({ mode, prev }) => {
    const working = mode === "develop" || mode === "edit";
    $("dev-panel").hidden = mode !== "develop";
    $("edit-panel").hidden = mode !== "edit";
    if (!working) {
      if (prev === "develop" || prev === "edit") flushSave();
      return;
    }
    if (store.current && store.current.id !== dev.id) await load(store.current);
    else preview();
    edit.drawOverlay(ctx);
  });
  on("focus-changed", () => {
    const working = store.mode === "develop" || store.mode === "edit";
    if (working && store.current && store.current.id !== dev.id) load(store.current);
  });
  on("key", (e) => {
    if (store.mode !== "develop" && store.mode !== "edit") return;
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.key.toLowerCase() === "z") {
      e.preventDefault();
      e.shiftKey ? redo() : undo();
    } else if (ctrl && e.key.toLowerCase() === "y") {
      e.preventDefault();
      redo();
    } else if (e.key === "\\") {
      toggleBefore();
    } else if (e.key === "ArrowRight") {
      setFocus(store.focus + 1);
    } else if (e.key === "ArrowLeft") {
      setFocus(store.focus - 1);
    }
  });
  window.addEventListener("beforeunload", () => flushSave());
}
