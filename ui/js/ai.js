// AI panel: model download, library analysis with progress, status.
// Everything runs on this computer; only the one-time model download
// touches the network.

import { api } from "./api.js";
import { emit, toast, el } from "./state.js";

const $ = (id) => document.getElementById(id);
let status = null;

export const aiReady = () => !!status?.installed;
export const aiAnalyzed = () => (status?.counts?.analyzed ?? 0) > 0;

const mb = (b) => `${Math.round(b / 1e6)} MB`;

export async function refreshStatus() {
  status = await api.aiStatus().catch(() => null);
  render();
  emit("ai-status", status);
  return status;
}

function render() {
  const box = $("ai-body");
  if (!box || !status) return;
  const c = status.counts;
  const parts = [];
  if (!status.installed) {
    parts.push(
      el("p", {}, "Find people, search photos by describing them (\"dog on the beach\"), get keyword suggestions and one-click AI masks."),
      el("p", { class: "hint" },
        `Downloads ${mb(status.downloadBytes)} of open models once (YuNet & SFace faces, TinyCLIP, U²-Netp; MIT/Apache licences). `
        + "Analysis runs entirely on this computer — your photos are never uploaded."),
      el("button", { type: "button", class: "btn primary", id: "ai-download", onclick: download }, `Download AI models (${mb(status.downloadBytes)})`),
    );
  } else {
    parts.push(
      el("dl", { class: "ai-stats" },
        el("dt", {}, "Analysed"), el("dd", {}, `${c.analyzed} / ${c.total} photos`),
        el("dt", {}, "Faces found"), el("dd", {}, String(c.faces)),
        el("dt", {}, "People"), el("dd", {}, String(c.people))),
      status.running
        ? el("button", { type: "button", class: "btn", onclick: () => api.aiCancel() }, "Stop")
        : el("button", { type: "button", class: "btn primary", onclick: analyze, disabled: c.analyzed >= c.total }, c.analyzed >= c.total ? "Library analysed" : `Analyse ${c.total - c.analyzed} photos`),
      el("p", { class: "hint" }, "New and edited photos can be analysed any time; results are kept in the catalog."),
    );
  }
  parts.push(el("div", { class: "progress wide", id: "ai-progress", hidden: true }, el("div", { class: "progress-bar" }), el("span", { class: "progress-text" })));
  box.replaceChildren(...parts);
}

function progress(text, frac) {
  const p = $("ai-progress");
  if (!p) return;
  p.hidden = frac == null;
  p.querySelector(".progress-bar").style.width = Math.round((frac ?? 0) * 100) + "%";
  p.querySelector(".progress-text").textContent = text;
}

async function download() {
  $("ai-download").disabled = true;
  const stop = await api.listen("ai-download-progress", ([d, t]) => progress(`Downloading ${mb(d)} / ${mb(t)}`, d / t));
  try {
    await api.aiDownload();
    toast("AI models installed");
  } catch (e) {
    toast(`Download failed: ${e}`, true);
  }
  stop();
  await refreshStatus();
}

async function analyze() {
  try {
    await api.aiAnalyze();
    await refreshStatus();
  } catch (e) {
    toast(String(e), true);
  }
}

export function open() {
  refreshStatus();
  $("ai-dialog").showModal();
}

export function init() {
  $("ai-btn").onclick = open;
  api.listen("ai-progress", (p) => {
    const label = { loading: "Loading models…", analyzing: `Analysing ${p.done} / ${p.total}`, people: "Grouping faces into people…", done: "Done" }[p.stage];
    progress(label, p.stage === "done" ? null : p.total ? p.done / p.total : 0);
    const chip = $("ai-btn");
    chip.classList.toggle("busy", p.stage !== "done");
    chip.title = p.stage === "done" ? "AI" : label;
    if (p.stage === "done") {
      toast(`AI analysis finished (${p.total} photos)`);
      refreshStatus();
      emit("library-changed");
    }
  });
  api.listen("ai-error", (e) => {
    toast(`AI analysis failed: ${e}`, true);
    $("ai-btn").classList.remove("busy");
    refreshStatus();
  });
  refreshStatus();
}
