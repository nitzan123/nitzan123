// Edit mode: masked local adjustments (radial, linear gradient, sky,
// whole image) + one-click looks built from them. Masks live inside the
// develop settings, so everything stays non-destructive.

import { el, toast } from "./state.js";
import { api } from "./api.js";
import { aiReady, open as openAi } from "./ai.js";
import { slider } from "./controls.js";

const $ = (id) => document.getElementById(id);
let selected = -1;

const ADJ = () => ({
  enabled: true, name: "", mask: { kind: "full" }, invert: false, lumaRange: null,
  exposure: 0, contrast: 0, saturation: 0, temperature: 0, tint: 0, blur: 0,
});

const MASKS = {
  radial: () => ({ kind: "radial", cx: 0.5, cy: 0.5, rx: 0.25, ry: 0.25, feather: 50 }),
  linear: () => ({ kind: "linear", x0: 0.5, y0: 0, x1: 0.5, y1: 0.6 }),
  sky: () => ({ kind: "sky", sensitivity: 50 }),
  full: () => ({ kind: "full" }),
};
const KIND_NAMES = { radial: "Radial", linear: "Gradient", sky: "Sky (auto-detect)", full: "Whole image", bitmap: "AI mask" };

/** Asks the backend for an AI mask of the current photo, then adds it. */
async function addAi(ctx, kind, partial, name) {
  if (!aiReady()) {
    toast("AI masks need the AI models");
    return openAi();
  }
  const id = ctx.dev.id;
  toast(`Finding ${kind}…`);
  try {
    const mask = await api.aiMask(id, kind);
    if (id !== ctx.dev.id) return;
    add(ctx, { ...partial, mask }, name);
  } catch (e) {
    toast(String(e), true);
  }
}

const LOOKS = [
  ["Brighten sky", { mask: MASKS.sky(), exposure: 0.4, saturation: 15 }],
  ["Dramatic sky", { mask: MASKS.sky(), exposure: -0.6, contrast: 25, saturation: 20 }],
  ["Blur background", { mask: { ...MASKS.radial(), rx: 0.3, ry: 0.4, feather: 60 }, invert: true, blur: 60, exposure: -0.15 }],
  ["Spotlight", { mask: { ...MASKS.radial(), rx: 0.35, ry: 0.35, feather: 80 }, invert: true, exposure: -0.9 }],
  ["Warm foreground", { mask: { kind: "linear", x0: 0.5, y0: 1, x1: 0.5, y1: 0.4 }, temperature: 30, exposure: 0.2 }],
];

export function reset() {
  selected = -1;
}

function add(ctx, partial, name) {
  ctx.change((s) => s.local.push({ ...ADJ(), ...partial, name }));
  ctx.commit();
  selected = ctx.dev.s.local.length - 1;
  render(ctx);
  drawOverlay(ctx);
}

export function render(ctx) {
  const box = $("edit-panel");
  const locals = ctx.dev.s.local;
  if (selected >= locals.length) selected = locals.length - 1;

  const items = locals.map((adj, i) => {
    const upd = (fn) => ctx.change((s) => fn(s.local[i]));
    const sl = (label, key, min, max, step = 1, def = 0, scale = 1) =>
      slider({ label, min, max, step, def, value: getter(adj, key) * scale, onInput: (v) => upd((a) => setter(a, key, v / scale)), onCommit: ctx.commit });

    const maskControls = {
      radial: () => [sl("Centre X", "mask.cx", 0, 100, 0.5, 50, 100), sl("Centre Y", "mask.cy", 0, 100, 0.5, 50, 100),
        sl("Width", "mask.rx", 1, 100, 0.5, 25, 100), sl("Height", "mask.ry", 1, 100, 0.5, 25, 100), sl("Feather", "mask.feather", 0, 100, 1, 50)],
      linear: () => [sl("Start X", "mask.x0", 0, 100, 0.5, 50, 100), sl("Start Y", "mask.y0", 0, 100, 0.5, 0, 100),
        sl("End X", "mask.x1", 0, 100, 0.5, 50, 100), sl("End Y", "mask.y1", 0, 100, 0.5, 60, 100)],
      sky: () => [sl("Sensitivity", "mask.sensitivity", 0, 100, 1, 50),
        el("p", { class: "hint" }, "Heuristic detection (colour, brightness, connected to the top edge). Works best on clear or overcast skies.")],
      full: () => [],
      bitmap: () => [el("p", { class: "hint" }, `AI-detected ${adj.mask.label}. Use Invert to adjust everything else.`)],
    }[adj.mask.kind]();

    const lumaToggle = el("label", { class: "check" },
      el("input", { type: "checkbox", checked: !!adj.lumaRange, onchange: (e) => {
        upd((a) => (a.lumaRange = e.target.checked ? [0, 1] : null));
        ctx.commit();
        render(ctx);
      } }), "Limit to brightness range");
    const lumaControls = adj.lumaRange
      ? [sl("From", "lumaRange.0", 0, 100, 1, 0, 100), sl("To", "lumaRange.1", 0, 100, 1, 100, 100)]
      : [];

    const head = el("div", { class: "local-head", onclick: (e) => {
      if (e.target.closest("input,button")) return;
      selected = selected === i ? -1 : i;
      render(ctx);
      drawOverlay(ctx);
    } },
      el("input", { type: "checkbox", checked: adj.enabled, title: "Enabled", onchange: (e) => { upd((a) => (a.enabled = e.target.checked)); ctx.commit(); } }),
      el("span", { class: "name" }, adj.name || KIND_NAMES[adj.mask.kind]),
      el("button", { class: "icon-btn", title: "Invert mask", onclick: () => { upd((a) => (a.invert = !a.invert)); ctx.commit(); render(ctx); } }, adj.invert ? "◑" : "◐"),
      el("button", { class: "icon-btn", title: "Delete", onclick: () => {
        ctx.change((s) => s.local.splice(i, 1));
        ctx.commit();
        selected = -1;
        render(ctx);
        drawOverlay(ctx);
      } }, "✕"));

    const body = selected === i
      ? el("div", { class: "local-body" },
          el("h3", {}, "Mask"), ...maskControls, lumaToggle, ...lumaControls,
          el("h3", {}, "Adjust"),
          sl("Exposure", "exposure", -3, 3, 0.01), sl("Contrast", "contrast", -100, 100), sl("Saturation", "saturation", -100, 100),
          sl("Temperature", "temperature", -100, 100), sl("Tint", "tint", -100, 100), sl("Blur", "blur", 0, 100))
      : null;
    return el("div", { class: "local-item" + (selected === i ? " active" : "") }, head, body);
  });

  box.replaceChildren(
    el("div", { class: "panel" },
      el("h3", {}, "Add local adjustment"),
      el("div", { class: "btn-row" },
        ...Object.keys(MASKS).map((k) => el("button", { class: "btn small", onclick: () => add(ctx, { mask: MASKS[k](), exposure: k === "linear" ? -0.5 : 0.3 }, KIND_NAMES[k]) }, KIND_NAMES[k]))),
      el("div", { class: "btn-row" },
        el("button", { class: "btn small", onclick: () => addAi(ctx, "subject", { exposure: 0.3 }, "Subject (AI)") }, "✨ Subject (AI)"),
        el("button", { class: "btn small", onclick: () => addAi(ctx, "sky", { exposure: -0.3 }, "Sky (AI)") }, "✨ Sky (AI)"),
        el("button", { class: "btn small", onclick: () => addAi(ctx, "subject", { invert: true, blur: 60, exposure: -0.15 }, "Blur background (AI)") }, "✨ Blur background")),
      el("h3", {}, "Quick looks"),
      el("div", { class: "btn-row" }, ...LOOKS.map(([name, p]) => el("button", { class: "btn small", onclick: () => add(ctx, structuredClone(p), name) }, name)))),
    el("div", { class: "panel" },
      el("h3", {}, `Adjustments (${locals.length})`),
      locals.length ? items : el("p", { class: "muted" }, "None yet. Add one above; drag the handles on the photo to place it.")),
  );
}

const getter = (obj, path) => path.split(".").reduce((o, k) => o?.[k], obj) ?? 0;
function setter(obj, path, v) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((o, k) => o[k], obj)[last] = v;
}

// ------------------------------------------------------------- overlay

let dragState = null;

export function drawOverlay(ctx) {
  const svg = $("mask-overlay");
  const img = $("dev-img");
  svg.replaceChildren();
  const adj = ctx.dev.s.local[selected];
  const mode = document.querySelector(".modes [aria-selected=true]")?.dataset.mode;
  if (mode !== "edit" || !adj || !img.naturalWidth || ctx.dev.before) return;
  const stage = $("dev-stage").getBoundingClientRect();
  const r = img.getBoundingClientRect();
  Object.assign(svg.style, { left: r.left - stage.left + "px", top: r.top - stage.top + "px", width: r.width + "px", height: r.height + "px" });
  svg.setAttribute("viewBox", `0 0 ${r.width} ${r.height}`);
  const NS = "http://www.w3.org/2000/svg";
  const node = (tag, attrs) => {
    const n = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
    svg.append(n);
    return n;
  };
  const handle = (x, y, onMove) => {
    const h = node("circle", { cx: x * r.width, cy: y * r.height, r: 7, class: "handle" });
    h.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      dragState = { onMove, ctx };
    });
  };
  const m = adj.mask;
  const line = { stroke: "#fff", "stroke-width": 1.5, fill: "none", "stroke-dasharray": "5 4" };
  if (m.kind === "radial") {
    node("ellipse", { cx: m.cx * r.width, cy: m.cy * r.height, rx: m.rx * r.width, ry: m.ry * r.height, ...line });
    handle(m.cx, m.cy, (mk, x, y) => ((mk.cx = x), (mk.cy = y)));
    handle(m.cx + m.rx, m.cy, (mk, x) => (mk.rx = Math.max(0.01, x - mk.cx)));
    handle(m.cx, m.cy + m.ry, (mk, _x, y) => (mk.ry = Math.max(0.01, y - mk.cy)));
  } else if (m.kind === "linear") {
    node("line", { x1: m.x0 * r.width, y1: m.y0 * r.height, x2: m.x1 * r.width, y2: m.y1 * r.height, ...line });
    handle(m.x0, m.y0, (mk, x, y) => ((mk.x0 = x), (mk.y0 = y)));
    handle(m.x1, m.y1, (mk, x, y) => ((mk.x1 = x), (mk.y1 = y)));
  }
}

export function init(ctx) {
  render(ctx);
  // Drag at window level: the overlay is rebuilt while dragging.
  window.addEventListener("pointermove", (e) => {
    if (!dragState) return;
    const r = $("dev-img").getBoundingClientRect();
    const nx = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    const ny = Math.min(1, Math.max(0, (e.clientY - r.top) / r.height));
    ctx.change((s) => dragState.onMove(s.local[selected].mask, nx, ny));
    drawOverlay(ctx);
  });
  window.addEventListener("pointerup", () => {
    if (!dragState) return;
    dragState = null;
    ctx.commit();
    render(ctx);
  });
}
