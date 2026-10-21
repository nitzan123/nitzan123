// View mode: full-screen viewer with filmstrip and 1:1 zoom.

import { urls } from "./api.js";
import { store, on, el, fmt } from "./state.js";
import { setFocus } from "./manage.js";

const $ = (id) => document.getElementById(id);
const STRIP_RADIUS = 40;
let zoomed = false;
let drag = null;

export function renderFilmstrip(container) {
  const lo = Math.max(0, store.focus - STRIP_RADIUS);
  const hi = Math.min(store.assets.length, store.focus + STRIP_RADIUS + 1);
  const imgs = [];
  for (let i = lo; i < hi; i++) {
    const a = store.assets[i];
    imgs.push(el("img", { src: urls.thumb(a), class: i === store.focus ? "current" : "", title: a.fileName, onclick: () => go(i) }));
  }
  container.replaceChildren(...imgs);
  container.querySelector(".current")?.scrollIntoView({ inline: "center", block: "nearest" });
}

function go(i) {
  if (i < 0 || i >= store.assets.length) return;
  setFocus(i);
}

function show() {
  const a = store.current;
  if (!a) return;
  const img = $("view-img");
  zoomed = false;
  img.onload = null; // a pending zoom-load must not re-zoom the next photo
  img.classList.remove("zoomed");
  img.style.cssText = "";
  const max = Math.round(Math.max(window.innerWidth, window.innerHeight) * devicePixelRatio);
  img.src = urls.view(a, Math.min(max, 4096));
  updateInfo();
  renderFilmstrip($("view-strip"));
}

function updateInfo() {
  const a = store.current;
  if (!a) return;
  $("view-info").textContent = [
    `${store.focus + 1} / ${store.assets.length}`,
    a.fileName,
    a.rating ? fmt.stars(a.rating) : null,
    a.takenAt,
  ].filter(Boolean).join("   ·   ");
}

function toggleZoom(e) {
  const img = $("view-img");
  if (drag?.moved) return;
  zoomed = !zoomed;
  if (!zoomed) {
    img.classList.remove("zoomed");
    img.style.cssText = "";
    return;
  }
  const rect = img.getBoundingClientRect();
  const fx = (e.clientX - rect.left) / rect.width;
  const fy = (e.clientY - rect.top) / rect.height;
  img.onload = () => {
    img.onload = null;
    const v = $("viewer").getBoundingClientRect();
    const w = img.naturalWidth / devicePixelRatio;
    const h = img.naturalHeight / devicePixelRatio;
    img.classList.add("zoomed");
    img.style.cssText = `width:${w}px;height:${h}px;left:${v.width / 2 - fx * w}px;top:${v.height / 2 - fy * h}px`;
  };
  img.src = urls.view(store.current, 8192);
}

export function init() {
  $("view-prev").onclick = () => go(store.focus - 1);
  $("view-next").onclick = () => go(store.focus + 1);
  const img = $("view-img");
  img.addEventListener("pointerdown", (e) => {
    drag = { x: e.clientX, y: e.clientY, moved: false, left: img.offsetLeft, top: img.offsetTop };
  });
  window.addEventListener("pointermove", (e) => {
    if (!drag || !zoomed) return;
    const dx = e.clientX - drag.x;
    const dy = e.clientY - drag.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
    img.style.left = drag.left + dx + "px";
    img.style.top = drag.top + dy + "px";
  });
  window.addEventListener("pointerup", () => setTimeout(() => (drag = null), 0));
  img.addEventListener("click", toggleZoom);
  $("viewer").addEventListener("wheel", (e) => {
    if (store.mode !== "view" || zoomed) return;
    e.preventDefault();
    go(store.focus + (e.deltaY > 0 ? 1 : -1));
  }, { passive: false });

  on("mode", ({ mode }) => mode === "view" && show());
  on("focus-changed", () => store.mode === "view" && show());
  // Rating/label changes only touch the info line; keep zoom and pan.
  on("assets-updated", () => store.mode === "view" && updateInfo());
  on("key", (e) => {
    if (store.mode !== "view") return;
    if (e.key === "ArrowRight" || e.key === "PageDown") go(store.focus + 1);
    else if (e.key === "ArrowLeft" || e.key === "PageUp") go(store.focus - 1);
    else if (e.key === "z") toggleZoom({ clientX: innerWidth / 2, clientY: innerHeight / 2 });
  });
}
