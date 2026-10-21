// Manage mode: library navigation, filters, virtualised thumbnail grid and
// the properties panel.

import { api, urls } from "./api.js";
import { store, emit, on, toast, promptText, el, fmt, LABELS } from "./state.js";
import { openExportDialog } from "./export.js";
import { aiAnalyzed, open as openAi } from "./ai.js";

let smartMode = false;

const $ = (id) => document.getElementById(id);
const META_H = 34;

let thumbSize = 190;
let layout = { cols: 1, cellW: 190, cellH: 224 };
const nodes = new Map(); // asset id -> cell element (only visible ones)
let anchor = -1; // shift-click range anchor

// ------------------------------------------------------------------ query

export async function reload({ keepFocus = true } = {}) {
  const focusedId = keepFocus ? store.current?.id : null;
  try {
    store.assets = store.smart
      ? (await api.smartSearch(store.smart, 300)).map((r) => r.asset)
      : await api.search(store.query);
  } catch (e) {
    toast(String(e), true);
    return;
  }
  const ids = new Set(store.assets.map((a) => a.id));
  store.selected = new Set([...store.selected].filter((id) => ids.has(id)));
  anchor = -1; // indices changed; a stale anchor would select the wrong range
  store.focus = focusedId != null ? store.assets.findIndex((a) => a.id === focusedId) : -1;
  if (store.focus < 0 && store.assets.length) store.focus = 0;
  $("count").textContent = `${store.assets.length} photo${store.assets.length === 1 ? "" : "s"}`;
  $("manage-export").disabled = store.assets.length === 0;
  $("breadcrumb").textContent = store.scope;
  $("empty").hidden = store.assets.length > 0 || !!hasFilters();
  relayout(true);
  renderProps();
  emit("assets-changed");
}

function hasFilters() {
  const q = store.query;
  return q.folder || q.text || q.minRating || q.colorLabel != null || q.tagged || q.keyword || q.collectionId || q.dateFrom || q.dateTo || q.camera || q.developed;
}

function setScope(scope, patch) {
  store.smart = null;
  const { sort, descending, text } = store.query;
  store.query = { sort, descending, text, ...patch };
  store.scope = scope;
  syncFilterUi();
  refreshSidebar();
  return reload({ keepFocus: false });
}

function patchQuery(patch) {
  Object.assign(store.query, patch);
  for (const k of Object.keys(store.query)) if (store.query[k] === undefined) delete store.query[k];
  syncFilterUi();
  reload();
}

// ---------------------------------------------------------------- sidebar

export async function refreshSidebar() {
  const [folders, collections, keywords, saved, people] = await Promise.all([
    api.folders(), api.collections(), api.keywords(), api.savedSearches(), api.persons().catch(() => []),
  ]).catch((e) => (toast(String(e), true), [[], [], [], [], []]));

  $("people-panel").hidden = people.length === 0;
  $("people-list").replaceChildren(...people.map((p) => {
    const label = p.name || "Unnamed";
    const open = () => setScope(`Person · ${label}`, { personId: p.id });
    const rename = async () => {
      const name = await promptText("Who is this?", p.name || "");
      if (!name) return;
      try {
        const id = await api.namePerson(p.id, name);
        if (store.query.personId === p.id) store.query.personId = id;
      } catch (e) {
        toast(String(e), true);
      }
      refreshSidebar();
      renderProps();
    };
    return el("li", {
      class: store.query.personId === p.id ? "active" : "", tabindex: 0, role: "button",
      title: `${label} · ${p.photos} photo${p.photos === 1 ? "" : "s"} — double-click to name`,
      onclick: open, ondblclick: rename,
      onkeydown: (e) => (e.key === "Enter" ? open() : e.key === "F2" && rename()),
    }, el("img", { src: urls.face(p.coverFace), alt: "", loading: "lazy" }),
      el("span", { class: "pname" + (p.name ? "" : " unnamed") }, `${label} (${p.photos})`));
  }));

  const q = store.query;
  const item = (label, count, active, onclick, extra, title = label) =>
    el("li", {
      class: active ? "active" : "", onclick, title, tabindex: 0, role: "button", "aria-current": active ? "true" : null,
      onkeydown: (e) => (e.key === "Enter" || e.key === " ") && e.target === e.currentTarget && (e.preventDefault(), onclick()),
    }, el("span", {}, label), count != null ? el("span", { class: "count" }, String(count)) : null, extra);

  const total = folders.reduce((n, f) => n + f.count, 0);
  $("folder-list").replaceChildren(
    item("All photos", total, !q.folder && !q.collectionId && !q.keyword, () => setScope("All photos", {})),
    ...folders.map((f) =>
      item(shortPath(f.path), f.count, q.folder === f.path, () => setScope(f.path, { folder: f.path }),
        el("button", { class: "icon-btn del", title: "Rescan", onclick: (e) => (e.stopPropagation(), api.rescanFolder(f.path)) }, "⟳"),
        f.path)),
  );
  $("collection-list").replaceChildren(
    ...collections.map((c) => item(c.name, c.count, q.collectionId === c.id, () => setScope(`Collection · ${c.name}`, { collectionId: c.id }))),
  );
  $("keyword-list").replaceChildren(
    ...keywords.map((k) => item(k.name, k.count, q.keyword === k.name, () => setScope(`Keyword · ${k.name}`, { keyword: k.name }))),
  );
  $("saved-list").replaceChildren(
    ...saved.map((s) =>
      item(s.name, null, false, () => setScope(`Search · ${s.name}`, { ...s.query }),
        el("button", { class: "icon-btn del", title: "Delete", onclick: async (e) => {
          e.stopPropagation();
          await api.deleteSavedSearch(s.id);
          refreshSidebar();
        } }, "✕"))),
  );
  $("keyword-list").dataset.all = JSON.stringify(keywords.map((k) => k.name));
  $("collection-list").dataset.all = JSON.stringify(collections);
}

/** Last two path segments, e.g. "2024/Iceland". */
const shortPath = (p) => p.split(/[\\/]/).filter(Boolean).slice(-2).join("/");

function buildFilters() {
  const rating = $("rating-filter");
  rating.replaceChildren(
    ...[0, 1, 2, 3, 4, 5].map((r) =>
      el("button", { class: "chip", "data-r": r, onclick: () => patchQuery({ minRating: r || undefined }) }, r ? `${r}★+` : "Any")),
  );
  const labels = $("label-filter");
  labels.replaceChildren(
    el("button", { class: "chip", "data-l": "any", onclick: () => patchQuery({ colorLabel: undefined }) }, "Any"),
    el("button", { class: "swatch none", "data-l": "0", title: "No label", onclick: () => patchQuery({ colorLabel: 0 }) }),
    ...LABELS.map((l) =>
      el("button", { class: "swatch", "data-l": l.id, title: l.name, style: `background:${l.color}`, onclick: () => patchQuery({ colorLabel: l.id }) })),
  );
  $("tagged-filter").onchange = (e) => patchQuery({ tagged: e.target.checked || undefined });
  $("developed-filter").onchange = (e) => patchQuery({ developed: e.target.checked || undefined });
  $("date-from").onchange = (e) => patchQuery({ dateFrom: e.target.value || undefined });
  $("date-to").onchange = (e) => patchQuery({ dateTo: e.target.value || undefined });
  $("camera-filter").oninput = (e) => {
    clearTimeout(e.target._t);
    e.target._t = setTimeout(() => patchQuery({ camera: e.target.value || undefined }), 250);
  };
  $("clear-filters").onclick = () =>
    patchQuery({ minRating: undefined, colorLabel: undefined, tagged: undefined, developed: undefined, dateFrom: undefined, dateTo: undefined, camera: undefined });
  $("save-search").onclick = async () => {
    const name = await promptText("Save current search as");
    if (!name) return;
    await api.saveSearch(name, store.query).catch((e) => toast(String(e), true));
    refreshSidebar();
  };
  $("new-collection").onclick = async () => {
    const name = await promptText("New collection name");
    if (!name) return;
    try {
      const id = await api.createCollection(name);
      const ids = store.selectedIds();
      if (ids.length) await api.addToCollection(id, ids);
      toast(ids.length ? `Created "${name}" with ${ids.length} photo(s)` : `Created "${name}"`);
    } catch (e) {
      toast(String(e), true);
    }
    refreshSidebar();
  };
  $("manage-export").onclick = () => openExportDialog();
  $("sort-key").onchange = (e) => patchQuery({ sort: e.target.value });
  $("sort-dir").onclick = () => patchQuery({ descending: !store.query.descending });
  $("thumb-size").oninput = (e) => {
    thumbSize = Number(e.target.value);
    relayout(true);
  };
}

function syncFilterUi() {
  const q = store.query;
  document.querySelectorAll("#rating-filter .chip").forEach((c) => c.classList.toggle("active", Number(c.dataset.r) === (q.minRating || 0)));
  document.querySelectorAll("#label-filter [data-l]").forEach((c) =>
    c.classList.toggle("active", c.dataset.l === (q.colorLabel == null ? "any" : String(q.colorLabel))));
  $("tagged-filter").checked = !!q.tagged;
  $("developed-filter").checked = !!q.developed;
  $("date-from").value = q.dateFrom || "";
  $("date-to").value = q.dateTo || "";
  $("camera-filter").value = q.camera || "";
  $("sort-key").value = q.sort || "name";
  $("sort-dir").textContent = q.descending ? "↓" : "↑";
}

// ------------------------------------------------------------------- grid

function relayout(force = false) {
  const grid = $("grid");
  const width = grid.clientWidth - 12;
  const cols = Math.max(1, Math.floor(width / thumbSize));
  const cellW = Math.floor(width / cols);
  const cellH = Math.round(cellW * 0.82) + META_H;
  const changed = cols !== layout.cols || cellW !== layout.cellW;
  layout = { cols, cellW, cellH };
  $("grid-inner").style.height = Math.ceil(store.assets.length / cols) * cellH + 12 + "px";
  if (changed || force) {
    nodes.forEach((n) => n.remove());
    nodes.clear();
  }
  paint();
}

function paint() {
  const grid = $("grid");
  const { cols, cellW, cellH } = layout;
  const first = Math.max(0, Math.floor(grid.scrollTop / cellH) - 2) * cols;
  const last = Math.min(store.assets.length, (Math.ceil((grid.scrollTop + grid.clientHeight) / cellH) + 2) * cols);
  const keep = new Set();
  for (let i = first; i < last; i++) {
    const a = store.assets[i];
    keep.add(a.id);
    let n = nodes.get(a.id);
    if (!n) {
      n = cell(a);
      nodes.set(a.id, n);
      $("grid-inner").append(n);
    }
    n.dataset.index = i;
    n.style.cssText = `left:${6 + (i % cols) * cellW}px;top:${6 + Math.floor(i / cols) * cellH}px;width:${cellW}px;height:${cellH}px`;
    n.classList.toggle("selected", store.selected.has(a.id));
    n.classList.toggle("focus", i === store.focus);
  }
  for (const [id, n] of nodes) if (!keep.has(id)) (n.remove(), nodes.delete(id));
}

function cell(a) {
  const label = LABELS.find((l) => l.id === a.colorLabel);
  return el("div", { class: "cell", "data-id": a.id },
    el("div", { class: "img" },
      el("img", { src: urls.thumb(a), loading: "lazy", alt: "", draggable: "false" }),
      a.tagged ? el("span", { class: "tagmark", title: "Tagged" }, "✓") : null,
      a.developed ? el("span", { class: "devmark", title: "Developed" }, "◐") : null),
    el("div", { class: "meta" },
      el("div", { class: "name" }, a.fileName),
      el("div", { class: "badges" },
        a.rating ? el("span", { class: "stars" }, "★".repeat(a.rating)) : null,
        label ? el("span", { class: "dot", style: `background:${label.color}`, title: label.name }) : null,
        el("span", {}, a.ext.toUpperCase()))));
}

function refreshCell(a) {
  const n = nodes.get(a.id);
  if (!n) return;
  const fresh = cell(a);
  fresh.style.cssText = n.style.cssText;
  fresh.dataset.index = n.dataset.index;
  fresh.className = n.className;
  n.replaceWith(fresh);
  nodes.set(a.id, fresh);
}

function onGridClick(e) {
  const c = e.target.closest(".cell");
  if (!c) {
    store.selected.clear();
    paint();
    return;
  }
  const i = Number(c.dataset.index);
  const id = store.assets[i].id;
  if (e.shiftKey && anchor >= 0) {
    const [a, b] = [Math.min(anchor, i), Math.max(anchor, i)];
    if (!(e.ctrlKey || e.metaKey)) store.selected.clear();
    for (let k = a; k <= b; k++) store.selected.add(store.assets[k].id);
  } else if (e.ctrlKey || e.metaKey) {
    store.selected.has(id) ? store.selected.delete(id) : store.selected.add(id);
    anchor = i;
  } else {
    store.selected = new Set([id]);
    anchor = i;
  }
  setFocus(i, false);
}

export function setFocus(i, select = true) {
  if (i < 0 || i >= store.assets.length) return;
  store.focus = i;
  if (select) store.selected = new Set([store.assets[i].id]);
  const grid = $("grid");
  const top = Math.floor(i / layout.cols) * layout.cellH;
  if (top < grid.scrollTop) grid.scrollTop = top;
  else if (top + layout.cellH > grid.scrollTop + grid.clientHeight) grid.scrollTop = top + layout.cellH - grid.clientHeight + 12;
  paint();
  renderProps();
  emit("focus-changed");
}

// ------------------------------------------------------------- properties

let propsToken = 0;

async function renderProps() {
  const box = $("props");
  const a = store.current;
  const n = store.selected.size;
  if (!a) return box.replaceChildren(el("p", { class: "muted" }, "Select a photo."));
  const token = ++propsToken;
  const det = await api.details(a.id).catch(() => null);
  if (!det || token !== propsToken) return;
  const asset = det.asset;
  store.replaceAsset(asset);
  const ids = () => store.selectedIds();

  const stars = el("div", { class: "stars-input", role: "radiogroup", "aria-label": "Rating" },
    ...[1, 2, 3, 4, 5].map((r) =>
      el("button", { class: r <= asset.rating ? "on" : "", title: `${r} star${r > 1 ? "s" : ""}`, onclick: () => emit("set-rating", { ids: ids(), rating: asset.rating === r ? 0 : r }) }, "★")));
  const labels = el("div", { class: "filter-row" },
    el("button", { class: "swatch none" + (asset.colorLabel === 0 ? " active" : ""), title: "No label", onclick: () => emit("set-label", { ids: ids(), label: 0 }) }),
    ...LABELS.map((l) =>
      el("button", { class: "swatch" + (asset.colorLabel === l.id ? " active" : ""), title: `${l.name} (${l.id + 5})`, style: `background:${l.color}`, onclick: () => emit("set-label", { ids: ids(), label: l.id }) })));
  const tagged = el("label", { class: "check" },
    el("input", { type: "checkbox", checked: asset.tagged, onchange: (e) => emit("set-tagged", { ids: ids(), tagged: e.target.checked }) }), "Tagged (T)");

  const caption = el("textarea", { placeholder: "Caption", "aria-label": "Caption" });
  caption.value = asset.caption || "";
  caption.onchange = () => api.setCaption(asset.id, caption.value).then(() => toast("Caption saved"));

  const allKw = JSON.parse($("keyword-list").dataset.all || "[]");
  const kwInput = el("input", { type: "text", placeholder: "Add keyword ↵", list: "kw-suggest", "aria-label": "Add keyword" });
  kwInput.onkeydown = async (e) => {
    if (e.key !== "Enter" || !kwInput.value.trim()) return;
    try {
      await api.addKeyword(ids(), kwInput.value.trim());
    } catch (err) {
      return toast(String(err), true);
    }
    refreshSidebar();
    renderProps();
  };
  const kwList = el("div", { class: "kw-list" },
    ...det.keywords.map((k) =>
      el("span", { class: "kw" }, k, el("button", { title: "Remove", onclick: async () => {
        try {
          await api.removeKeyword(ids(), k);
        } catch (err) {
          return toast(String(err), true);
        }
        refreshSidebar();
        renderProps();
      } }, "✕"))));

  const cols = JSON.parse($("collection-list").dataset.all || "[]");
  const colSelect = el("select", { "aria-label": "Add to collection", onchange: async (e) => {
    const id = Number(e.target.value);
    if (!id) return;
    await api.addToCollection(id, ids());
    toast("Added to collection");
    refreshSidebar();
    e.target.value = "";
  } }, el("option", { value: "" }, "Add to collection…"), ...cols.map((c) => el("option", { value: c.id }, c.name)));

  const exif = [
    ["Size", asset.width && `${asset.width} × ${asset.height}`],
    ["File", fmt.bytes(asset.size)],
    ["Taken", asset.takenAt],
    ["Camera", [asset.cameraMake, asset.cameraModel].filter(Boolean).join(" ")],
    ["Lens", asset.lens],
    ["Exposure", [asset.fNumber && `ƒ/${asset.fNumber.toFixed(1)}`, fmt.shutter(asset.exposureTime), asset.iso && `ISO ${asset.iso}`].filter(Boolean).join("  ")],
    ["Focal", asset.focalLength && `${Math.round(asset.focalLength)} mm`],
    ["GPS", det.gps && `${det.gps[0].toFixed(5)}, ${det.gps[1].toFixed(5)}`],
    ["Path", asset.path],
  ].filter(([, v]) => v);

  // AI: suggested keywords and recognised people.
  const aiBox = el("div", {});
  api.aiDetails(asset.id).then((ai) => {
    if (token !== propsToken) return;
    const have = new Set(det.keywords.map((k) => k.toLowerCase()));
    const sugg = ai.tags.filter(([t]) => !have.has(t.toLowerCase()));
    const parts = [];
    if (sugg.length) {
      parts.push(el("div", { class: "kw-list", title: "AI suggestions — click to add" },
        ...sugg.map(([t, p]) => el("button", { class: "chip suggest", onclick: async () => {
          await api.addKeyword(ids(), t).catch((e) => toast(String(e), true));
          refreshSidebar();
          renderProps();
        } }, `+ ${t} ${Math.round(p * 100)}%`))));
    }
    if (ai.faces.length) {
      parts.push(el("h3", {}, "People"), el("div", { class: "face-row" },
        ...ai.faces.map((f) => el("button", { title: "Click to name", onclick: async () => {
          const name = await promptText("Who is this?", f.personName || "");
          if (!name) return;
          await api.setFaceName(f.id, name).catch((e) => toast(String(e), true));
          refreshSidebar();
          renderProps();
        } }, el("img", { src: urls.face(f.id), alt: "" }), f.personName || "Unnamed"))));
    }
    aiBox.replaceChildren(...parts);
  }).catch(() => {});

  const similar = el("div", { class: "similar" });
  const simBtn = el("button", { class: "btn small", onclick: async () => {
    const hits = await api.similar(asset.id, 12);
    similar.replaceChildren(
      ...(hits.length ? hits.map((h) => el("img", { src: urls.thumb(h.asset), title: `${h.asset.fileName} · distance ${h.distance}`, onclick: () => revealAsset(h.asset) })) : [el("span", { class: "muted" }, "No near-duplicates")]),
    );
  } }, "Find similar");

  box.replaceChildren(
    el("img", { class: "preview", src: urls.thumb(asset), alt: "" }),
    el("div", { class: "breadcrumb" }, n > 1 ? `${n} selected` : asset.fileName),
    stars, labels, tagged,
    el("h3", {}, "Caption"), caption,
    el("h3", {}, "Keywords"), kwList, kwInput,
    aiBox,
    el("datalist", { id: "kw-suggest" }, ...allKw.map((k) => el("option", { value: k }))),
    el("div", { class: "btn-row" }, colSelect),
    el("dl", {}, ...exif.flatMap(([k, v]) => [el("dt", {}, k), el("dd", {}, String(v))])),
    el("div", { class: "btn-row" },
      el("button", { class: "btn small primary", onclick: () => emit("request-mode", "develop") }, "Develop"),
      el("button", { class: "btn small", onclick: () => emit("request-mode", "view") }, "View"),
      simBtn),
    similar,
  );
}

async function revealAsset(a) {
  if (!store.assets.some((x) => x.id === a.id)) await setScope(a.folder, { folder: a.folder });
  setFocus(store.assets.findIndex((x) => x.id === a.id));
}

// ------------------------------------------------------- attribute updates

/** Runs `call`, then refreshes the affected cells. With `patch` (e.g.
 * `{ rating: 4 }`) assets are updated locally — no per-photo round trip,
 * so rating 5,000 selected photos stays instant. */
async function updateAssets(ids, call, patch = null) {
  try {
    await call();
  } catch (e) {
    return toast(String(e), true);
  }
  if (patch) {
    const set = new Set(ids);
    for (const a of store.assets) {
      if (!set.has(a.id)) continue;
      Object.assign(a, patch);
      refreshCell(a);
    }
  } else {
    for (const id of ids) {
      const d = await api.details(id).catch(() => null);
      if (!d) continue;
      store.replaceAsset(d.asset);
      refreshCell(d.asset);
    }
  }
  paint();
  renderProps();
  emit("assets-updated", ids);
}

function moveFocus(e) {
  const { cols } = layout;
  const steps = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: cols, ArrowUp: -cols, Home: -1e9, End: 1e9 };
  if (!(e.key in steps) || !store.assets.length) return false;
  e.preventDefault();
  const next = Math.max(0, Math.min(store.assets.length - 1, store.focus + steps[e.key]));
  if (e.shiftKey) {
    store.selected.add(store.assets[next].id);
    setFocus(next, false);
  } else setFocus(next);
  return true;
}

export function init() {
  buildFilters();
  syncFilterUi();
  $("grid").addEventListener("scroll", () => requestAnimationFrame(paint));
  $("grid").addEventListener("click", onGridClick);
  $("grid").addEventListener("dblclick", (e) => e.target.closest(".cell") && emit("request-mode", "view"));
  new ResizeObserver(() => relayout()).observe($("grid"));

  on("library-changed", () => {
    refreshSidebar();
    reload();
  });
  on("quick-search", (text) => {
    if (smartMode && text.trim()) {
      store.smart = text.trim();
      store.scope = `Smart search · “${store.smart}”`;
      return reload({ keepFocus: false });
    }
    if (store.smart) {
      store.smart = null;
      store.scope = "All photos";
    }
    patchQuery({ text: text || undefined });
  });
  $("smart-toggle").onclick = () => {
    if (!aiAnalyzed()) {
      toast("Smart search needs the AI models and an analysed library");
      return openAi();
    }
    smartMode = !smartMode;
    $("smart-toggle").setAttribute("aria-pressed", smartMode);
    $("quick-search").placeholder = smartMode ? "Describe the photo… e.g. “sunset over the sea”" : "Search name, caption, keyword…";
    const q = $("quick-search").value;
    if (q) emit("quick-search", q);
  };
  on("set-rating", ({ ids, rating }) => updateAssets(ids, () => api.setRating(ids, rating), { rating }));
  on("set-label", ({ ids, label }) => updateAssets(ids, () => api.setColorLabel(ids, label), { colorLabel: label }));
  on("set-tagged", ({ ids, tagged }) => updateAssets(ids, () => api.setTagged(ids, tagged), { tagged }));
  on("toggle-tagged", ({ ids }) => {
    const tagged = !store.assets.find((a) => a.id === ids[0])?.tagged;
    updateAssets(ids, () => api.setTagged(ids, tagged), { tagged });
  });
  on("asset-developed", (id) => updateAssets([id], async () => {}));
  on("key", (e) => {
    if (store.mode !== "manage") return;
    if (moveFocus(e)) return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      store.selected = new Set(store.assets.map((a) => a.id));
      paint();
      renderProps();
    }
  });
  on("mode", ({ mode }) => {
    if (mode === "manage") {
      relayout(true);
      if (store.focus >= 0) setFocus(store.focus, false);
    }
  });
}
