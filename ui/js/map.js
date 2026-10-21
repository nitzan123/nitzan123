// Map view of the current results' GPS positions. Small slippy map with no
// dependencies: OpenStreetMap tiles come through prism://tile (fetched and
// cached by the backend), photos are clustered by screen distance.

import { urls } from "./api.js";
import { store, emit, el } from "./state.js";

const $ = (id) => document.getElementById(id);
const TILE = 256;
const MIN_Z = 2;
const MAX_Z = 18;
const CLUSTER_PX = 56;
const MAX_LAT = 85.0511;

// Centre of the view in world pixels at zoom `z`.
const view = { z: 3, cx: 0, cy: 0 };
let signature = "";
let tiles = new Map(); // "z/x/y" -> img
let raf = 0;
let popupFor = null;

const worldSize = (z) => TILE * 2 ** z;

function project(lat, lon, z) {
  const s = worldSize(z);
  const phi = (Math.max(-MAX_LAT, Math.min(MAX_LAT, lat)) * Math.PI) / 180;
  return [((lon + 180) / 360) * s, (0.5 - Math.log(Math.tan(Math.PI / 4 + phi / 2)) / (2 * Math.PI)) * s];
}

const located = () => store.assets.filter((a) => a.gps);

function size() {
  const m = $("map");
  return [m.clientWidth, m.clientHeight];
}

function clampCenter() {
  const [, h] = size();
  const s = worldSize(view.z);
  view.cx = ((view.cx % s) + s) % s;
  view.cy = s <= h ? s / 2 : Math.max(h / 2, Math.min(s - h / 2, view.cy));
}

function zoomTo(z, anchor) {
  z = Math.max(MIN_Z, Math.min(MAX_Z, z));
  if (z === view.z) return;
  const [w, h] = size();
  const [ax, ay] = anchor || [w / 2, h / 2];
  // Keep the world point under the anchor fixed.
  const wx = view.cx - w / 2 + ax;
  const wy = view.cy - h / 2 + ay;
  const f = 2 ** (z - view.z);
  view.cx = wx * f - ax + w / 2;
  view.cy = wy * f - ay + h / 2;
  view.z = z;
  closePopup();
  schedule();
}

/** Zooms and centres so every point is visible. */
function fit(points) {
  if (!points.length) return;
  const [w, h] = size();
  let z = MAX_Z - 2;
  for (; z >= MIN_Z; z--) {
    const ps = points.map((a) => project(a.gps[0], a.gps[1], z));
    const xs = ps.map((p) => p[0]);
    const ys = ps.map((p) => p[1]);
    const fits = Math.max(...xs) - Math.min(...xs) < w - 120 && Math.max(...ys) - Math.min(...ys) < h - 120;
    if (fits || z === MIN_Z) {
      view.cx = (Math.max(...xs) + Math.min(...xs)) / 2;
      view.cy = (Math.max(...ys) + Math.min(...ys)) / 2;
      break;
    }
  }
  view.z = points.length === 1 ? Math.min(z, 13) : z;
  if (points.length === 1) [view.cx, view.cy] = project(points[0].gps[0], points[0].gps[1], view.z);
}

function schedule() {
  cancelAnimationFrame(raf);
  raf = requestAnimationFrame(render);
}

function renderTiles() {
  const [w, h] = size();
  const n = 2 ** view.z;
  const left = view.cx - w / 2;
  const top = view.cy - h / 2;
  const keep = new Set();
  const layer = $("map-tiles");
  for (let ty = Math.floor(top / TILE); ty <= Math.floor((top + h) / TILE); ty++) {
    if (ty < 0 || ty >= n) continue;
    for (let tx = Math.floor(left / TILE); tx <= Math.floor((left + w) / TILE); tx++) {
      const wrapped = ((tx % n) + n) % n;
      const key = `${view.z}/${tx}/${ty}`;
      keep.add(key);
      let img = tiles.get(key);
      if (!img) {
        img = el("img", { class: "tile", alt: "", draggable: "false", src: urls.tile(view.z, wrapped, ty) });
        img.onerror = () => (img.style.visibility = "hidden");
        tiles.set(key, img);
        layer.append(img);
      }
      img.style.transform = `translate(${Math.round(tx * TILE - left)}px, ${Math.round(ty * TILE - top)}px)`;
    }
  }
  for (const [k, img] of tiles) if (!keep.has(k)) (img.remove(), tiles.delete(k));
}

function clusters() {
  const [w, h] = size();
  const s = worldSize(view.z);
  const left = view.cx - w / 2;
  const top = view.cy - h / 2;
  const cells = new Map();
  for (const a of located()) {
    let [x, y] = project(a.gps[0], a.gps[1], view.z);
    // Pick the world copy nearest the view (the map wraps horizontally).
    x += Math.round((view.cx - x) / s) * s;
    const sx = x - left;
    const sy = y - top;
    if (sx < -CLUSTER_PX || sy < -CLUSTER_PX || sx > w + CLUSTER_PX || sy > h + CLUSTER_PX) continue;
    const key = `${Math.floor(sx / CLUSTER_PX)},${Math.floor(sy / CLUSTER_PX)}`;
    const c = cells.get(key) || { assets: [], x: 0, y: 0 };
    c.assets.push(a);
    c.x += sx;
    c.y += sy;
    cells.set(key, c);
  }
  return [...cells.values()].map((c) => ({ ...c, x: c.x / c.assets.length, y: c.y / c.assets.length }));
}

function renderMarkers() {
  const layer = $("map-markers");
  layer.replaceChildren(...clusters().map((c) => {
    const a = c.assets[0];
    const selected = c.assets.some((x) => store.selected.has(x.id));
    return el("button", {
      type: "button",
      class: "marker" + (selected ? " selected" : ""),
      style: `transform: translate(${Math.round(c.x - 22)}px, ${Math.round(c.y - 22)}px); background-image: url("${urls.thumb(a)}")`,
      title: c.assets.length === 1 ? a.fileName : `${c.assets.length} photos`,
      "aria-label": c.assets.length === 1 ? a.fileName : `${c.assets.length} photos`,
      onclick: (e) => (e.stopPropagation(), openPopup(c)),
      ondblclick: (e) => (e.stopPropagation(), zoomToCluster(c)),
    }, c.assets.length > 1 ? el("span", { class: "badge" }, String(c.assets.length)) : null);
  }));
}

function render() {
  if ($("map").hidden) return;
  clampCenter();
  renderTiles();
  renderMarkers();
  const n = located().length;
  $("map-info").textContent = store.assets.length === 0
    ? "No photos in this view"
    : n === 0
      ? "None of these photos has a GPS location"
      : `${n} of ${store.assets.length} photos have a location`;
}

function openPopup(c) {
  popupFor = c.assets.map((a) => a.id).join(",");
  store.selected = new Set(c.assets.map((a) => a.id));
  emit("map-select", c.assets[0].id);
  const shown = c.assets.slice(0, 12);
  const box = $("map-popup");
  box.replaceChildren(...[
    el("div", { class: "map-popup-head" },
      el("span", {}, c.assets.length === 1 ? c.assets[0].fileName : `${c.assets.length} photos selected`),
      el("button", { type: "button", class: "icon-btn", "aria-label": "Close", onclick: closePopup }, "✕")),
    el("div", { class: "map-popup-thumbs" },
      ...shown.map((a) => el("img", {
        src: urls.thumb(a), alt: a.fileName, title: `${a.fileName} — open`, loading: "lazy", tabindex: 0,
        onclick: () => emit("map-open", a.id),
        onkeydown: (e) => e.key === "Enter" && emit("map-open", a.id),
      }))),
    c.assets.length > shown.length ? el("p", { class: "hint" }, `+${c.assets.length - shown.length} more (selected)`) : null,
    c.assets.length > 1 ? el("button", { type: "button", class: "btn small", onclick: () => zoomToCluster(c) }, "Zoom in") : null,
  ].filter(Boolean));
  box.style.left = `${Math.max(8, Math.min(size()[0] - 300, c.x - 140))}px`;
  box.style.top = `${Math.max(8, c.y + 30 > size()[1] - 200 ? c.y - 230 : c.y + 30)}px`;
  box.hidden = false;
  renderMarkers();
}

/** Fits the cluster's photos (at least two levels closer). */
function zoomToCluster(c) {
  const z = view.z;
  fit(c.assets);
  if (view.z < z + 2) {
    const zz = Math.min(MAX_Z, z + 2);
    const [x, y] = project(c.assets[0].gps[0], c.assets[0].gps[1], zz);
    Object.assign(view, { z: zz, cx: x, cy: y });
  }
  closePopup();
  schedule();
}

function closePopup() {
  popupFor = null;
  $("map-popup").hidden = true;
}

/** Shows or hides the map; refits when the located photos changed. */
export function show(on) {
  $("map").hidden = !on;
  if (!on) return closePopup();
  refresh(true);
}

export function refresh(allowFit = false) {
  if ($("map").hidden) return;
  const sig = located().map((a) => a.id).join(",");
  if (sig !== signature || allowFit) {
    if (sig !== signature) closePopup();
    signature = sig;
    fit(located());
  }
  if (popupFor && !store.selected.size) closePopup();
  schedule();
}

export function init() {
  const m = $("map");
  let drag = null;
  m.addEventListener("pointerdown", (e) => {
    if (e.target.closest(".marker, .map-popup, .map-controls")) return;
    drag = { x: e.clientX, y: e.clientY, cx: view.cx, cy: view.cy, moved: false };
    m.setPointerCapture(e.pointerId);
  });
  m.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const dx = e.clientX - drag.x;
    const dy = e.clientY - drag.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
    view.cx = drag.cx - dx;
    view.cy = drag.cy - dy;
    schedule();
  });
  const end = (e) => {
    if (drag && !drag.moved && !e.target.closest(".map-popup")) closePopup();
    drag = null;
  };
  m.addEventListener("pointerup", end);
  m.addEventListener("pointercancel", () => (drag = null));
  m.addEventListener("wheel", (e) => {
    e.preventDefault();
    const r = m.getBoundingClientRect();
    zoomTo(view.z + (e.deltaY < 0 ? 1 : -1), [e.clientX - r.left, e.clientY - r.top]);
  }, { passive: false });
  m.addEventListener("keydown", (e) => {
    if (e.target !== m) return;
    const step = 80;
    const k = { "+": () => zoomTo(view.z + 1), "=": () => zoomTo(view.z + 1), "-": () => zoomTo(view.z - 1),
      ArrowLeft: () => (view.cx -= step), ArrowRight: () => (view.cx += step), ArrowUp: () => (view.cy -= step), ArrowDown: () => (view.cy += step) }[e.key];
    if (k) (e.preventDefault(), e.stopPropagation(), k(), schedule());
  });
  $("map-zoom-in").onclick = () => zoomTo(view.z + 1);
  $("map-zoom-out").onclick = () => zoomTo(view.z - 1);
  $("map-fit").onclick = () => (fit(located()), closePopup(), schedule());
  new ResizeObserver(() => schedule()).observe(m);
}
