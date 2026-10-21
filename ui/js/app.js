// Entry point: mode switching, global keyboard shortcuts, indexing progress.

import { api, urls } from "./api.js";
import { store, emit, on, toast } from "./state.js";
import * as manage from "./manage.js";
import * as view from "./view.js";
import * as develop from "./develop.js";

const MODES = ["manage", "view", "develop", "edit"];

export function setMode(mode) {
  if (!MODES.includes(mode)) return;
  if (mode !== "manage" && !store.current) {
    toast("Select a photo first");
    return;
  }
  const prev = store.mode;
  store.mode = mode;
  document.querySelectorAll(".modes button").forEach((b) => b.setAttribute("aria-selected", b.dataset.mode === mode));
  // Develop and Edit share one workspace; the right panel switches.
  const section = mode === "edit" ? "develop" : mode;
  document.querySelectorAll(".mode").forEach((s) => s.classList.toggle("active", s.id === `mode-${section}`));
  emit("mode", { mode, prev });
}

on("request-mode", setMode);

async function addFolder() {
  const path = await api.pickFolder("Add folder to catalog");
  if (!path) return;
  await api.addFolder(path, true).catch((e) => toast(String(e), true));
}

function wireProgress() {
  const box = document.getElementById("progress");
  const bar = box.querySelector(".progress-bar");
  const text = box.querySelector(".progress-text");
  let refreshTimer = null;
  api.listen("index-progress", (p) => {
    box.hidden = p.stage === "done";
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    bar.style.width = (p.stage === "thumbnails" ? pct : p.stage === "indexed" ? 5 : 2) + "%";
    text.textContent = p.stage === "scanning" ? "Scanning…" : p.stage === "thumbnails" ? `Thumbnails ${p.done}/${p.total}` : "Indexing…";
    // Refresh the library at milestones, throttled.
    if (p.stage === "indexed" || p.stage === "done") {
      clearTimeout(refreshTimer);
      refreshTimer = setTimeout(() => emit("library-changed"), 50);
    }
    if (p.stage === "done") toast(`Indexed ${p.total} photos`);
  });
  api.listen("index-error", (msg) => {
    box.hidden = true;
    toast(`Indexing failed: ${msg}`, true);
  });
}

function wireKeys() {
  document.addEventListener("keydown", (e) => {
    const t = e.target;
    // Ignore typing contexts only; sliders/checkboxes keep shortcuts working.
    const typing = t.matches?.("textarea, select, input:not([type=range]):not([type=checkbox]):not([type=radio])");
    if (typing || document.querySelector("dialog[open]")) return;
    // Arrow/Page keys on a focused slider adjust the slider, not the photo.
    if (t.matches?.("input[type=range]") && /^(Arrow|Page|Home|End)/.test(e.key)) return;
    // Let focused buttons/links/summaries handle their own Enter/Space.
    const activatable = t.matches?.("button, a, summary, [role=button]");
    const ctrl = e.ctrlKey || e.metaKey;
    const ids = store.selectedIds();

    if (!ctrl && /^[0-5]$/.test(e.key) && ids.length) {
      e.preventDefault();
      emit("set-rating", { ids, rating: Number(e.key) });
    } else if (!ctrl && /^[6-9]$/.test(e.key) && ids.length) {
      e.preventDefault();
      emit("set-label", { ids, label: Number(e.key) - 5 });
    } else if (!ctrl && e.key.toLowerCase() === "t" && ids.length) {
      emit("toggle-tagged", { ids });
    } else if (!ctrl && e.key.toLowerCase() === "d") {
      setMode("develop");
    } else if (!ctrl && e.key.toLowerCase() === "e") {
      setMode("edit");
    } else if (!ctrl && e.key.toLowerCase() === "g") {
      setMode("manage");
    } else if (e.key === "?") {
      document.getElementById("help-dialog").showModal();
    } else if (e.key === "Escape" && store.mode !== "manage") {
      setMode("manage");
    } else if ((e.key === "Enter" || e.key === " ") && !activatable && store.mode === "manage" && store.current) {
      e.preventDefault();
      setMode("view");
    } else {
      emit("key", e);
    }
  });
}

document.querySelectorAll(".modes button").forEach((b) => b.addEventListener("click", () => setMode(b.dataset.mode)));
document.getElementById("add-folder").addEventListener("click", addFolder);
document.getElementById("help-btn").addEventListener("click", () => document.getElementById("help-dialog").showModal());
document.getElementById("empty-add").addEventListener("click", addFolder);
document.getElementById("quick-search").addEventListener("input", (e) => {
  clearTimeout(e.target._t);
  e.target._t = setTimeout(() => emit("quick-search", e.target.value), 200);
});

// Edits change the rendered look: bust cached views/thumbs of that asset.
on("asset-developed", (id) => urls.invalidate(id));

wireProgress();
wireKeys();
manage.init();
view.init();
develop.init();
emit("library-changed");
