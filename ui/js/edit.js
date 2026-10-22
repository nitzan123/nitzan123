// Edit mode: masked local adjustments (radial, linear gradient, sky, brush,
// AI, whole image), one-click looks built from them, and heal/clone spot
// retouching. Everything lives inside the develop settings, so it stays
// non-destructive.

import { el, toast } from "./state.js";
import { api } from "./api.js";
import { aiReady, open as openAi } from "./ai.js";
import { slider } from "./controls.js";

const $ = (id) => document.getElementById(id);
const NS = "http://www.w3.org/2000/svg";
let selected = -1;

const ADJ = () => ({
  enabled: true, name: "", mask: { kind: "full" }, invert: false, lumaRange: null,
  exposure: 0, contrast: 0, saturation: 0, temperature: 0, tint: 0, blur: 0,
});

const MASKS = {
  brush: () => ({ kind: "brush", strokes: [] }),
  radial: () => ({ kind: "radial", cx: 0.5, cy: 0.5, rx: 0.25, ry: 0.25, feather: 50 }),
  linear: () => ({ kind: "linear", x0: 0.5, y0: 0, x1: 0.5, y1: 0.6 }),
  sky: () => ({ kind: "sky", sensitivity: 50 }),
  full: () => ({ kind: "full" }),
};
const KIND_NAMES = { brush: "Brush", radial: "Radial", linear: "Gradient", sky: "Sky (auto-detect)", full: "Whole image", bitmap: "AI mask" };

// Brush options for new strokes (size = radius as a fraction of the long edge).
const brush = { erase: false, size: 0.04, feather: 50, flow: 100, show: true };

// Retouch: active tool ("heal" | "clone" | null), options for new spots,
// selected spot, and the spots' positions on the developed preview.
let tool = null;
const spotOpts = { radius: 0.012, feather: 40 };
let spotSel = -1;
// items: { t, s, edge } — target, source and a point on the target's rim, normalised to the preview (null when cropped out)
let spotView = { sig: "", aspect: 1, aspectId: null, items: [] };
let hover = null; // pointer position over the photo (normalised), for the brush cursor

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
  spotSel = -1;
  tool = null;
  spotView = { sig: "", aspect: 1, aspectId: null, items: [] };
}

function add(ctx, partial, name) {
  ctx.change((s) => s.local.push({ ...ADJ(), ...partial, name }));
  ctx.commit();
  selected = ctx.dev.s.local.length - 1;
  tool = null;
  render(ctx);
  drawOverlay(ctx);
}

const toggle = (label, on, onclick, title) =>
  el("button", { type: "button", class: "btn small" + (on ? " active" : ""), "aria-pressed": String(on), title, onclick }, label);

// ------------------------------------------------------------------ panel

function retouchPanel(ctx) {
  const spots = ctx.dev.s.spots || [];
  if (spotSel >= spots.length) spotSel = spots.length - 1;
  const setTool = (t) => {
    tool = tool === t ? null : t;
    if (tool) selected = -1;
    render(ctx);
    drawOverlay(ctx);
  };
  const sp = spots[spotSel];
  const updSpot = (fn) => ctx.change((s) => fn(s.spots[spotSel]));
  return el("div", { class: "panel" },
    el("h3", {}, "Retouch"),
    el("div", { class: "btn-row" },
      toggle("🩹 Heal", tool === "heal", () => setTool("heal"), "Remove dust and blemishes, blending into the surroundings"),
      toggle("⧉ Clone", tool === "clone", () => setTool("clone"), "Copy pixels exactly from another spot")),
    tool
      ? el("div", {},
          el("p", { class: "hint" }, "Click a blemish to fix it (the best source is picked for you), or drag from the blemish to where to copy from."),
          slider({ label: "Size", min: 0.3, max: 10, step: 0.1, def: 1.2, value: spotOpts.radius * 100, onInput: (v) => (spotOpts.radius = v / 100, drawOverlay(ctx)) }),
          slider({ label: "Feather", min: 0, max: 100, def: 40, value: spotOpts.feather, onInput: (v) => (spotOpts.feather = v) }))
      : null,
    spots.length
      ? el("ul", { class: "spot-list" }, ...spots.map((p, i) => el("li", {
          class: i === spotSel ? "active" : "", tabindex: 0, role: "button",
          onclick: (e) => {
            if (e.target.closest("button,input")) return;
            spotSel = spotSel === i ? -1 : i;
            render(ctx);
            drawOverlay(ctx);
          },
        },
        el("input", { type: "checkbox", checked: p.enabled, title: "Enabled", onchange: (e) => { ctx.change((s) => (s.spots[i].enabled = e.target.checked)); ctx.commit(); } }),
        el("span", {}, `${p.mode === "clone" ? "Clone" : "Heal"} ${i + 1}`),
        el("button", { type: "button", class: "icon-btn", title: "Delete spot", "aria-label": `Delete spot ${i + 1}`, onclick: () => {
          ctx.change((s) => s.spots.splice(i, 1));
          ctx.commit();
          spotSel = -1;
          render(ctx);
          drawOverlay(ctx);
        } }, "✕"))))
      : null,
    sp
      ? el("div", { class: "local-body" },
          el("div", { class: "btn-row" },
            toggle("Heal", sp.mode !== "clone", () => { updSpot((p) => (p.mode = "heal")); ctx.commit(); render(ctx); }),
            toggle("Clone", sp.mode === "clone", () => { updSpot((p) => (p.mode = "clone")); ctx.commit(); render(ctx); })),
          slider({ label: "Size", min: 0.3, max: 10, step: 0.1, def: 2, value: sp.radius * 100, onInput: (v) => (updSpot((p) => (p.radius = v / 100)), drawOverlay(ctx)), onCommit: ctx.commit }),
          slider({ label: "Feather", min: 0, max: 100, def: 40, value: sp.feather, onInput: (v) => updSpot((p) => (p.feather = v)), onCommit: ctx.commit }),
          slider({ label: "Opacity", min: 0, max: 100, def: 100, value: sp.opacity, onInput: (v) => updSpot((p) => (p.opacity = v)), onCommit: ctx.commit }),
          el("p", { class: "hint" }, "Drag the circles on the photo to move the spot or its source."))
      : null,
    spots.length > 1
      ? el("button", { type: "button", class: "btn small", onclick: () => { ctx.change((s) => (s.spots = [])); ctx.commit(); spotSel = -1; render(ctx); drawOverlay(ctx); } }, "Remove all spots")
      : null,
  );
}

function brushControls(ctx, adj, i) {
  const opt = (label, key, min, max, step, def, scale = 1) =>
    slider({ label, min, max, step, def, value: brush[key] * scale, onInput: (v) => { brush[key] = v / scale; drawOverlay(ctx); } });
  return [
    el("div", { class: "btn-row" },
      toggle("🖌 Paint", !brush.erase, () => { brush.erase = false; render(ctx); }, "Paint the mask"),
      toggle("⌫ Erase", brush.erase, () => { brush.erase = true; render(ctx); }, "Erase from the mask (Alt while painting)"),
      toggle("👁 Show", brush.show, () => { brush.show = !brush.show; render(ctx); drawOverlay(ctx); }, "Show the painted area")),
    opt("Size", "size", 0.3, 20, 0.1, 4, 100),
    opt("Feather", "feather", 0, 100, 1, 50),
    opt("Flow", "flow", 1, 100, 1, 100),
    el("p", { class: "hint" }, `Paint on the photo. ${adj.mask.strokes.length} stroke${adj.mask.strokes.length === 1 ? "" : "s"}.`),
    adj.mask.strokes.length
      ? el("button", { type: "button", class: "btn small", onclick: () => {
          ctx.change((s) => (s.local[i].mask.strokes = []));
          ctx.commit();
          render(ctx);
          drawOverlay(ctx);
        } }, "Clear strokes")
      : null,
  ];
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
      brush: () => brushControls(ctx, adj, i),
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
      if (selected >= 0) tool = null;
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
    retouchPanel(ctx),
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
      locals.length ? items : el("p", { class: "muted" }, "None yet. Add one above; drag the handles on the photo to place it, or paint with Brush.")),
  );
}

const getter = (obj, path) => path.split(".").reduce((o, k) => o?.[k], obj) ?? 0;
function setter(obj, path, v) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((o, k) => o[k], obj)[last] = v;
}

// ------------------------------------------------------------- spot mapping

const spotSig = (s) => JSON.stringify([s.geometry, s.lens, s.spots]);

/** Refreshes where each spot (target, source, radius) sits on the preview. */
async function refreshSpotView(ctx) {
  const s = ctx.dev.s;
  const sig = spotSig(s);
  if (sig === spotView.sig || spotView.loading === sig) return;
  spotView.loading = sig;
  const id = ctx.dev.id;
  try {
    if (spotView.aspectId !== id) {
      // Learn the source aspect first: radii are relative to its long edge.
      spotView.aspect = (await api.mapPoints(id, s, [[0.5, 0.5]], false)).aspect;
      spotView.aspectId = id;
    }
    const kx = longOver(spotView.aspect, "x");
    const pts = (s.spots || []).flatMap((p) => [[p.x, p.y], [p.sx, p.sy], [p.x + p.radius * kx, p.y]]);
    const res = pts.length ? await api.mapPoints(id, s, pts, false) : { points: [] };
    if (id !== ctx.dev.id || spotSig(ctx.dev.s) !== sig) return;
    const items = (s.spots || []).map((_p, i) => {
      const [t, src, edge] = res.points.slice(i * 3, i * 3 + 3);
      return { t, s: src, edge };
    });
    Object.assign(spotView, { sig, items, loading: null });
    drawOverlay(ctx);
  } catch {
    spotView.loading = null;
  }
}

/** Long edge over width (axis "x") or height ("y") for a source aspect. */
const longOver = (aspect, axis) => (axis === "x" ? Math.max(aspect, 1) / aspect : Math.max(aspect, 1));

/** Display point (normalised) → source point, via the backend. */
async function toSource(ctx, points) {
  const res = await api.mapPoints(ctx.dev.id, ctx.dev.s, points, true);
  spotView.aspect = res.aspect;
  return res.points;
}

async function placeSpot(ctx, from, to) {
  const id = ctx.dev.id;
  const dragged = to && Math.hypot(to[0] - from[0], to[1] - from[1]) > 0.01;
  try {
    const [t, s] = await toSource(ctx, dragged ? [from, to] : [from]);
    if (!t || id !== ctx.dev.id) return;
    const src = dragged ? s : await api.spotSource(id, ctx.dev.s, t[0], t[1], spotOpts.radius);
    if (id !== ctx.dev.id) return;
    ctx.change((st) => {
      st.spots = st.spots || [];
      st.spots.push({ enabled: true, mode: tool, x: t[0], y: t[1], sx: src[0], sy: src[1], radius: spotOpts.radius, feather: spotOpts.feather, opacity: 100 });
    });
    ctx.commit();
    spotSel = ctx.dev.s.spots.length - 1;
    render(ctx);
    drawOverlay(ctx);
  } catch (e) {
    toast(String(e), true);
  }
}

// ------------------------------------------------------------- overlay

let dragState = null; // { onMove(nx, ny), ctx } for handles; { paint: true } for brush; { spotFrom } for retouch

export function drawOverlay(ctx) {
  const svg = $("mask-overlay");
  const img = $("dev-img");
  svg.replaceChildren();
  const adj = ctx.dev.s.local[selected];
  const mode = document.querySelector(".modes [aria-selected=true]")?.dataset.mode;
  const showSpots = tool || spotSel >= 0;
  if (mode !== "edit" || !img.naturalWidth || ctx.dev.before || (!adj && !showSpots)) return;
  const stage = $("dev-stage").getBoundingClientRect();
  const r = img.getBoundingClientRect();
  Object.assign(svg.style, { left: r.left - stage.left + "px", top: r.top - stage.top + "px", width: r.width + "px", height: r.height + "px" });
  svg.setAttribute("viewBox", `0 0 ${r.width} ${r.height}`);
  const node = (tag, attrs, parent = svg) => {
    const n = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
    parent.append(n);
    return n;
  };
  const handle = (x, y, onMove, cls = "handle") => {
    const h = node("circle", { cx: x * r.width, cy: y * r.height, r: 7, class: cls });
    h.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      dragState = { onMove, ctx };
    });
    return h;
  };
  const capture = (cursor) => {
    const c = node("rect", { x: 0, y: 0, width: r.width, height: r.height, class: "capture", style: `cursor:${cursor}` });
    c.addEventListener("pointerdown", (e) => onCapturePointerDown(e, ctx));
    return c;
  };
  const line = { stroke: "#fff", "stroke-width": 1.5, fill: "none", "stroke-dasharray": "5 4" };
  const long = Math.max(r.width, r.height);

  if (showSpots) {
    if (tool) capture("crosshair");
    refreshSpotView(ctx);
    (ctx.dev.s.spots || []).forEach((p, i) => {
      const v = spotView.items[i];
      if (!v?.t) return;
      const rad = v.edge ? Math.hypot((v.edge[0] - v.t[0]) * r.width, (v.edge[1] - v.t[1]) * r.height) : p.radius * long;
      const active = i === spotSel;
      const cls = "spot" + (active ? " active" : "") + (p.enabled ? "" : " off");
      const target = node("circle", { cx: v.t[0] * r.width, cy: v.t[1] * r.height, r: rad, class: cls });
      target.addEventListener("pointerdown", (e) => {
        e.preventDefault();
        e.stopPropagation();
        spotSel = i;
        render(ctx);
        dragState = { spot: i, which: "target", ctx };
        drawOverlay(ctx);
      });
      if (active && v.s) {
        node("line", { x1: v.t[0] * r.width, y1: v.t[1] * r.height, x2: v.s[0] * r.width, y2: v.s[1] * r.height, class: "spot-link" });
        const src = node("circle", { cx: v.s[0] * r.width, cy: v.s[1] * r.height, r: rad, class: "spot source" });
        src.addEventListener("pointerdown", (e) => {
          e.preventDefault();
          e.stopPropagation();
          dragState = { spot: i, which: "source", ctx };
        });
      }
    });
    if (tool && hover) node("circle", { cx: hover[0] * r.width, cy: hover[1] * r.height, r: spotOpts.radius * long, class: "brush-cursor" });
    return;
  }

  const m = adj.mask;
  if (m.kind === "brush") {
    if (brush.show && m.strokes.length) {
      // Paint strokes white and erase strokes black inside an SVG mask,
      // then tint through it: an accurate (hard-edged) picture of the mask.
      const defs = node("defs", {});
      const mask = node("mask", { id: "brush-mask", maskUnits: "userSpaceOnUse", x: 0, y: 0, width: r.width, height: r.height }, defs);
      node("rect", { x: 0, y: 0, width: r.width, height: r.height, fill: "#000" }, mask);
      for (const st of m.strokes) {
        const w = Math.max(1, st.size * long * 2);
        const color = st.erase ? "#000" : `rgba(255,255,255,${st.flow / 100})`;
        const pts = st.points.map(([x, y]) => `${x * r.width},${y * r.height}`).join(" ");
        if (st.points.length === 1) node("circle", { cx: st.points[0][0] * r.width, cy: st.points[0][1] * r.height, r: w / 2, fill: color }, mask);
        else node("polyline", { points: pts, fill: "none", stroke: color, "stroke-width": w, "stroke-linecap": "round", "stroke-linejoin": "round" }, mask);
      }
      node("rect", { x: 0, y: 0, width: r.width, height: r.height, class: "brush-tint", mask: "url(#brush-mask)" });
    }
    capture("none");
    if (hover) node("circle", { cx: hover[0] * r.width, cy: hover[1] * r.height, r: brush.size * long, class: "brush-cursor" + (brush.erase ? " erase" : "") });
  } else if (m.kind === "radial") {
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

function norm(e) {
  const r = $("dev-img").getBoundingClientRect();
  return [Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), Math.min(1, Math.max(0, (e.clientY - r.top) / r.height))];
}

function onCapturePointerDown(e, ctx) {
  if (e.button !== 0) return;
  e.preventDefault();
  const p = norm(e);
  if (tool) {
    dragState = { spotFrom: p, ctx };
    return;
  }
  const adj = ctx.dev.s.local[selected];
  if (adj?.mask.kind !== "brush") return;
  const erase = brush.erase || e.altKey;
  ctx.change((s) => s.local[selected].mask.strokes.push({ erase, size: brush.size, feather: brush.feather, flow: brush.flow, points: [p] }));
  dragState = { paint: true, last: p, ctx };
  drawOverlay(ctx);
}

let moveBusy = false;

async function moveSpot(ctx, i, which, p) {
  if (moveBusy) return;
  moveBusy = true;
  try {
    const [pt] = await toSource(ctx, [p]);
    if (pt && ctx.dev.s.spots[i]) {
      ctx.change((s) => {
        if (which === "target") [s.spots[i].x, s.spots[i].y] = pt;
        else [s.spots[i].sx, s.spots[i].sy] = pt;
      });
      drawOverlay(ctx);
    }
  } finally {
    moveBusy = false;
  }
}

export function init(ctx) {
  render(ctx);
  // Drag at window level: the overlay is rebuilt while dragging.
  window.addEventListener("pointermove", (e) => {
    const img = $("dev-img");
    const r = img.getBoundingClientRect();
    const inside = e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
    const wantsCursor = tool || ctx.dev.s.local[selected]?.mask.kind === "brush";
    if (wantsCursor && (inside || hover)) {
      hover = inside ? norm(e) : null;
      if (!dragState) drawOverlay(ctx);
    }
    if (!dragState) return;
    const p = norm(e);
    if (dragState.paint) {
      const [lx, ly] = dragState.last;
      if (Math.hypot(p[0] - lx, p[1] - ly) < 0.003) return;
      dragState.last = p;
      ctx.change((s) => s.local[selected].mask.strokes.at(-1).points.push(p));
      drawOverlay(ctx);
    } else if (dragState.spot != null) {
      moveSpot(ctx, dragState.spot, dragState.which, p);
    } else if (dragState.onMove) {
      ctx.change((s) => dragState.onMove(s.local[selected].mask, p[0], p[1]));
      drawOverlay(ctx);
    }
  });
  window.addEventListener("pointerup", (e) => {
    if (!dragState) return;
    const d = dragState;
    dragState = null;
    if (d.spotFrom) return placeSpot(ctx, d.spotFrom, norm(e));
    ctx.commit();
    render(ctx);
    drawOverlay(ctx);
  });
}
