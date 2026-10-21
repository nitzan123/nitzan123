// Reusable develop controls: sliders, collapsible sections, tone-curve
// editor and histogram drawing.

import { el } from "./state.js";

/**
 * Labelled range + number input. Double-click the label to reset.
 * `onInput(v)` fires continuously; `onCommit()` once a drag/edit ends.
 */
export function slider({ label, min, max, step = 1, value, def = 0, onInput, onCommit, color }) {
  const range = el("input", { type: "range", min, max, step, "aria-label": label });
  const num = el("input", { type: "number", min, max, step, "aria-label": `${label} value` });
  const lab = el("label", { title: "Double-click to reset", style: color ? `color:${color}` : "" }, label);
  const row = el("div", { class: "slider" }, lab, range, num);
  const decimals = String(step).includes(".") ? String(step).split(".")[1].length : 0;
  const set = (v, emit) => {
    v = Math.min(max, Math.max(min, Number(v)));
    range.value = v;
    num.value = v.toFixed(decimals);
    row.classList.toggle("changed", Math.abs(v - def) > 1e-9);
    if (emit) onInput(v);
  };
  range.addEventListener("input", () => set(range.value, true));
  range.addEventListener("change", () => onCommit?.());
  num.addEventListener("change", () => {
    set(num.value, true);
    onCommit?.();
  });
  lab.addEventListener("dblclick", () => {
    set(def, true);
    onCommit?.();
  });
  set(value ?? def, false);
  row.update = (v) => set(v, false);
  return row;
}

export function section(title, open, ...children) {
  const d = el("details", { class: "section" }, el("summary", {}, title), el("div", { class: "body" }, ...children));
  d.open = open;
  return d;
}

export function tabs(names, onSelect, initial = 0) {
  const bar = el("div", { class: "tabs", role: "tablist" });
  names.forEach((n, i) =>
    bar.append(el("button", { role: "tab", class: i === initial ? "active" : "", onclick: () => {
      bar.querySelectorAll("button").forEach((b, j) => b.classList.toggle("active", j === i));
      onSelect(i);
    } }, n)));
  return bar;
}

export function drawHistogram(canvas, hist) {
  const ctx = canvas.getContext("2d");
  const W = (canvas.width = canvas.clientWidth * devicePixelRatio);
  const H = (canvas.height = canvas.clientHeight * devicePixelRatio);
  ctx.clearRect(0, 0, W, H);
  if (!hist) return;
  const max = Math.max(1, ...hist.slice(4, 60), ...hist.slice(68, 124), ...hist.slice(132, 188));
  ctx.globalCompositeOperation = "lighter";
  ["#e5534b", "#57b65f", "#4b8fe5"].forEach((col, c) => {
    ctx.fillStyle = col + "aa";
    ctx.beginPath();
    ctx.moveTo(0, H);
    for (let i = 0; i < 64; i++) {
      const v = Math.min(1, hist[c * 64 + i] / max);
      ctx.lineTo((i / 63) * W, H - v * H * 0.95);
    }
    ctx.lineTo(W, H);
    ctx.fill();
  });
  ctx.globalCompositeOperation = "source-over";
}

/** Monotone-cubic preview identical in shape to the Rust LUT. */
function curveSamples(points, n = 128) {
  const p = [...points].sort((a, b) => a[0] - b[0]);
  const k = p.length;
  const d = p.slice(0, -1).map((q, i) => (p[i + 1][1] - q[1]) / Math.max(1e-6, p[i + 1][0] - q[0]));
  const m = p.map((_, i) => (i === 0 ? d[0] : i === k - 1 ? d[k - 2] : d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2));
  for (let i = 0; i < k - 1; i++) {
    if (Math.abs(d[i]) < 1e-9) { m[i] = m[i + 1] = 0; continue; }
    const a = m[i] / d[i], b = m[i + 1] / d[i], s = a * a + b * b;
    if (s > 9) { const t = 3 / Math.sqrt(s); m[i] = t * a * d[i]; m[i + 1] = t * b * d[i]; }
  }
  const out = [];
  for (let j = 0; j < n; j++) {
    const x = j / (n - 1);
    if (x <= p[0][0]) { out.push(p[0][1]); continue; }
    if (x >= p[k - 1][0]) { out.push(p[k - 1][1]); continue; }
    let i = 0;
    while (p[i + 1][0] < x) i++;
    const h = p[i + 1][0] - p[i][0], t = (x - p[i][0]) / h, t2 = t * t, t3 = t2 * t;
    out.push(Math.min(1, Math.max(0, (2 * t3 - 3 * t2 + 1) * p[i][1] + (t3 - 2 * t2 + t) * h * m[i] + (-2 * t3 + 3 * t2) * p[i + 1][1] + (t3 - t2) * h * m[i + 1])));
  }
  return out;
}

/**
 * Tone-curve editor. Drag points; click empty space to add; double-click a
 * point to remove it. Endpoints can move vertically/horizontally but stay.
 */
export function curveEditor({ points, onInput, onCommit }) {
  const canvas = el("canvas", { id: "curve", "aria-label": "Tone curve" });
  let pts = points.map((p) => [...p]);
  let dragging = -1;
  let hist = null;

  const toPx = ([x, y]) => [x * canvas.width, (1 - y) * canvas.height];
  const fromEvent = (e) => {
    const r = canvas.getBoundingClientRect();
    return [Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), Math.min(1, Math.max(0, 1 - (e.clientY - r.top) / r.height))];
  };
  const hit = (pos) => pts.findIndex((p) => Math.hypot(p[0] - pos[0], p[1] - pos[1]) < 0.04);

  function draw() {
    const W = (canvas.width = canvas.clientWidth * devicePixelRatio || 280);
    const H = (canvas.height = canvas.clientWidth * devicePixelRatio || 280);
    const ctx = canvas.getContext("2d");
    ctx.clearRect(0, 0, W, H);
    if (hist) {
      ctx.fillStyle = "#ffffff14";
      const max = Math.max(1, ...hist);
      for (let i = 0; i < 64; i++) {
        const h = (hist[i] / max) * H;
        ctx.fillRect((i / 64) * W, H - h, W / 64 + 1, h);
      }
    }
    ctx.strokeStyle = "#ffffff18";
    for (let i = 1; i < 4; i++) {
      ctx.beginPath(); ctx.moveTo((i / 4) * W, 0); ctx.lineTo((i / 4) * W, H); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(0, (i / 4) * H); ctx.lineTo(W, (i / 4) * H); ctx.stroke();
    }
    ctx.strokeStyle = "#ffffff40";
    ctx.beginPath(); ctx.moveTo(0, H); ctx.lineTo(W, 0); ctx.stroke();
    const s = curveSamples(pts);
    ctx.strokeStyle = "#e6e8ec";
    ctx.lineWidth = 2 * devicePixelRatio;
    ctx.beginPath();
    s.forEach((y, i) => (i ? ctx.lineTo : ctx.moveTo).call(ctx, (i / (s.length - 1)) * W, (1 - y) * H));
    ctx.stroke();
    ctx.fillStyle = "#fff";
    for (const p of pts) {
      const [x, y] = toPx(p);
      ctx.beginPath(); ctx.arc(x, y, 5 * devicePixelRatio, 0, Math.PI * 2); ctx.fill();
    }
  }

  const emitPts = () => onInput(pts.map((p) => [Number(p[0].toFixed(4)), Number(p[1].toFixed(4))]));

  canvas.addEventListener("pointerdown", (e) => {
    const pos = fromEvent(e);
    dragging = hit(pos);
    if (dragging < 0) {
      pts.push(pos);
      pts.sort((a, b) => a[0] - b[0]);
      dragging = pts.indexOf(pos);
      emitPts();
    }
    canvas.setPointerCapture(e.pointerId);
    draw();
  });
  canvas.addEventListener("pointermove", (e) => {
    if (dragging < 0) return;
    const [x, y] = fromEvent(e);
    const lo = dragging === 0 ? 0 : pts[dragging - 1][0] + 0.01;
    const hi = dragging === pts.length - 1 ? 1 : pts[dragging + 1][0] - 0.01;
    pts[dragging] = [Math.min(hi, Math.max(lo, x)), y];
    emitPts();
    draw();
  });
  canvas.addEventListener("pointerup", () => {
    if (dragging >= 0) onCommit();
    dragging = -1;
  });
  canvas.addEventListener("dblclick", (e) => {
    const i = hit(fromEvent(e));
    if (i > 0 && i < pts.length - 1) {
      pts.splice(i, 1);
      emitPts();
      onCommit();
      draw();
    }
  });

  const wrap = el("div", { class: "curve-wrap" }, canvas);
  wrap.update = (p) => { pts = p.map((q) => [...q]); draw(); };
  wrap.setHistogram = (h) => {
    // Luminance-ish histogram from the RGB bins.
    hist = h ? Array.from({ length: 64 }, (_, i) => h[i] * 0.3 + h[64 + i] * 0.6 + h[128 + i] * 0.1) : null;
    draw();
  };
  requestAnimationFrame(draw);
  return wrap;
}
