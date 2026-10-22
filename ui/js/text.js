// Text on photos ("subject, place, date"): the batch "Date & place…"
// dialog in Manage, the subject/place fields in the properties panel, and
// the full editor in Develop. The text is part of each photo's develop
// settings, so it shows on thumbnails and in exports right away.

import { api, urls } from "./api.js";
import { store, emit, toast, el } from "./state.js";

const $ = (id) => document.getElementById(id);
const PREFS_KEY = "prism.text";
const RECENT_KEY = "prism.text.recent";

export const POSITIONS = [
  ["bottomLeft", "Bottom left"], ["bottomCenter", "Bottom centre"], ["bottomRight", "Bottom right"],
  ["topLeft", "Top left"], ["topRight", "Top right"],
];
const DATE_FORMATS = [["dotted", "23.09.2026"], ["numeric", "23/09/2026"], ["longDate", "23 Sep 2026"], ["long", "23 Sep 2026, 14:03"], ["iso", "2026-09-23 14:03"]];
const COLORS = [["auto", "Automatic"], ["black", "Black"], ["white", "White"]];

const DEFAULTS = {
  date: true, dateFormat: "dotted", place: true, placeLanguage: "", placeDetail: "city", useFileDate: false,
  position: "bottomLeft", size: 2.5, color: "auto", italic: true, bold: false,
};

function prefs() {
  try {
    return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(PREFS_KEY) || "{}") };
  } catch {
    return { ...DEFAULTS };
  }
}

function savePrefs(p) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(p));
  } catch {
    /* storage unavailable */
  }
}

/** Recently typed subjects/places, for autocomplete. */
function recent() {
  try {
    return { subject: [], place: [], ...JSON.parse(localStorage.getItem(RECENT_KEY) || "{}") };
  } catch {
    return { subject: [], place: [] };
  }
}

function remember(kind, value) {
  const v = (value || "").trim();
  if (!v) return;
  const r = recent();
  r[kind] = [v, ...r[kind].filter((x) => x !== v)].slice(0, 40);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(r));
  } catch {
    /* storage unavailable */
  }
}

const datalist = (id, kind) => el("datalist", { id }, ...recent()[kind].map((v) => el("option", { value: v })));

const select = (id, options, value) =>
  el("select", { id }, ...options.map(([v, label]) => el("option", { value: v, selected: v === value }, label)));

/** Tell other views that these photos' develop settings changed. */
function changed(ids) {
  emit("text-changed", ids);
  ids.forEach((id) => emit("asset-developed", id));
  emit("library-changed");
}

// ------------------------------------------------------------ batch dialog

async function openDialog() {
  const ids = store.selectedInOrder();
  if (!ids.length) return toast("Select photos first (Ctrl+A for all)");
  const p = prefs();
  const dlg = $("text-dialog");
  $("text-form").replaceChildren(
    el("p", { class: "muted" }, `${ids.length} photo${ids.length === 1 ? "" : "s"} · ${"what, place, date"} — e.g. “Vicuña, Chinchero, 01.10.2026”`),
    el("div", { class: "cap-grid" },
      el("label", { class: "check" }, el("input", { type: "checkbox", id: "tx-date", checked: p.date }), "Date"),
      select("tx-date-format", DATE_FORMATS, p.dateFormat),
      el("label", { class: "check" }, el("input", { type: "checkbox", id: "tx-place", checked: p.place }), "Place (from GPS)"),
      select("tx-place-detail", [["city", "Town only"], ["cityCountry", "Town, country"]], p.placeDetail),
      el("label", { for: "tx-lang" }, "Place names"),
      select("tx-lang", [["", "Local language"], ["en", "English"], ["he", "Hebrew"], ["es", "Spanish"]], p.placeLanguage),
      el("label", { for: "tx-position" }, "Position"),
      select("tx-position", POSITIONS, p.position),
      el("label", { for: "tx-color" }, "Colour"),
      select("tx-color", COLORS, p.color),
      el("label", { for: "tx-size" }, "Text size"),
      el("input", { id: "tx-size", type: "range", min: 1.5, max: 6, step: 0.25, value: p.size }),
      el("label", { class: "check" }, el("input", { type: "checkbox", id: "tx-italic", checked: p.italic }), "Italic"),
      el("label", { class: "check" }, el("input", { type: "checkbox", id: "tx-bold", checked: p.bold }), "Bold")),
    el("label", { class: "check" }, el("input", { type: "checkbox", id: "tx-file-date", checked: p.useFileDate }),
      "No date in the photo? Use the file's date"),
    el("p", { class: "hint" },
      "The photo's own data comes first (camera date, GPS). Photos without GPS get a place worked out from ",
      "photos with GPS taken at about the same time (also from a phone — the camera's clock offset is detected) ",
      "or from imported Google location history; those are marked “? check”. Photos with no place at all get a red 📍. ",
      "Text you typed or confirmed is kept. Place names come from OpenStreetMap (GPS positions, rounded to ~100 m, ",
      "are sent to openstreetmap.org). Originals are never changed."),
  );
  dlg.returnValue = "";
  dlg.showModal();
  const ok = await new Promise((r) => dlg.addEventListener("close", () => r(dlg.returnValue === "ok"), { once: true }));
  const v = {
    date: $("tx-date").checked, dateFormat: $("tx-date-format").value,
    place: $("tx-place").checked, placeDetail: $("tx-place-detail").value, placeLanguage: $("tx-lang").value,
    position: $("tx-position").value, color: $("tx-color").value, size: Number($("tx-size").value),
    italic: $("tx-italic").checked, bold: $("tx-bold").checked, useFileDate: $("tx-file-date").checked,
  };
  savePrefs(v);
  if (!ok) return;
  await apply(ids, v);
}

async function apply(ids, v) {
  toast(`Adding text to ${ids.length} photo${ids.length === 1 ? "" : "s"}…`);
  try {
    const res = await api.textApply(ids, {
      style: { position: v.position, size: v.size, color: v.color, italic: v.italic, bold: v.bold },
      date: v.date, dateFormat: v.dateFormat, place: v.place, placeLanguage: v.placeLanguage,
      placeDetail: v.placeDetail, useFileDate: v.useFileDate,
    });
    changed(ids);
    const parts = [`Text added to ${res.applied}`];
    if (res.estimated) parts.push(`${res.estimated} places estimated (? check)`);
    if (res.needPlace) parts.push(`${res.needPlace} without a place (📍)`);
    if (res.dateToCheck) parts.push(`${res.dateToCheck} dates from file names (? check)`);
    if (res.noDate) parts.push(`${res.noDate} without a date`);
    if (res.lookupFailed) parts.push("place lookup failed for some (offline?) — run it again later");
    parts.push(...res.notes);
    toast(parts.join(" · "), res.needPlace > 0 || res.estimated > 0 || res.lookupFailed);
    return res;
  } catch (e) {
    toast(String(e), true);
  }
}

async function confirm(ids) {
  try {
    await api.textConfirm(ids);
    changed(ids);
    toast("Confirmed ✓");
  } catch (e) {
    toast(String(e), true);
  }
}

// ---------------------------------------------------------- review screen

const DAY = (t) => (t || "").slice(0, 10);
const HM = (t) => (t || "").slice(11, 16);
const secs = (t) => (t ? Date.parse(t.replace(" ", "T") + "Z") / 1000 : NaN);

/** Photos needing review, grouped by time: a new group after 90 min. */
function groups(assets) {
  const sorted = [...assets].sort((a, b) => (a.takenAt || "~").localeCompare(b.takenAt || "~"));
  const out = [];
  for (const a of sorted) {
    const last = out.at(-1);
    const prev = last?.items.at(-1);
    const gap = prev ? secs(a.takenAt) - secs(prev.takenAt) : Infinity;
    if (last && (gap <= 90 * 60 || (!a.takenAt && !prev.takenAt))) last.items.push(a);
    else out.push({ items: [a] });
  }
  return out;
}

async function historyBox(onImported) {
  const h = await api.locationHistory().catch(() => ({ points: 0 }));
  const status = h.points
    ? `Location history: ${h.points.toLocaleString()} points, ${h.from} → ${h.to}.`
    : "No location history imported.";
  return el("div", { class: "history" },
    el("b", {}, "Google location history (optional) "), status, " ",
    el("button", { type: "button", class: "btn small", onclick: async () => {
      const path = await api.pickFile("Google location history", "Location history", ["json"]);
      if (!path) return;
      toast("Reading location history…");
      try {
        const r = await api.importLocationHistory(path);
        toast(`Imported ${r.points.toLocaleString()} points (${r.from} → ${r.to})`);
        onImported();
      } catch (e) {
        toast(String(e), true);
      }
    } }, h.points ? "Replace…" : "Import…"),
    el("details", {},
      el("summary", {}, "How to get it"),
      el("p", { class: "hint" },
        "If Location / Timeline was on in her phone during the trip, Google Maps kept her route. ",
        "Android: Settings → Location → Location services → Timeline → Export Timeline data. ",
        "iPhone: Google Maps → your picture → Settings → Personal content → Export Timeline data. ",
        "Copy the .json file to this computer and import it here. Only times and positions are read; ",
        "nothing is uploaded.")));
}

/** The "Review places" screen: one row per group of photos taken together. */
export async function openReview() {
  const dlg = $("review-dialog");
  const body = $("review-body");
  const render = async () => {
    const todo = store.assets.filter((a) => a.needsPlace || a.toCheck);
    const gs = groups(todo);
    const again = async () => {
      await apply(store.assets.filter((a) => a.needsPlace || a.toCheck).map((a) => a.id), prefs());
      setTimeout(render, 800);
    };
    const box = await historyBox(again);
    box.append(el("div", { class: "row", style: "margin-top:6px" },
      el("button", { type: "button", class: "btn small", onclick: again, title: "Look for places again, e.g. after adding phone photos or location history" }, "🔄 Find places again")));
    body.replaceChildren(
      el("p", { class: "muted" }, todo.length
        ? `${todo.length} photo${todo.length === 1 ? "" : "s"} in ${gs.length} group${gs.length === 1 ? "" : "s"}. Type the place once per group, or press ✓ if the estimate is right.`
        : "Nothing to review here ✓"),
      box,
      ...gs.map((g) => groupRow(g, render)),
    );
  };
  await render();
  dlg.returnValue = "";
  dlg.showModal();
}

function groupRow(g, rerender) {
  const ids = g.items.map((a) => a.id);
  const first = g.items[0].takenAt;
  const last = g.items.at(-1).takenAt;
  const when = first ? `${DAY(first)} · ${HM(first)}${HM(last) !== HM(first) ? `–${HM(last)}` : ""}` : "No date";
  const missing = g.items.filter((a) => a.needsPlace).length;
  const row = el("div", { class: "group" });
  const place = el("input", { type: "text", list: "rv-recent-place", placeholder: "Place for these photos (e.g. Chinchero)" });
  const subject = el("input", { type: "text", list: "rv-recent-subject", placeholder: "What / where exactly (optional)" });
  const status = el("div", { class: "status" }, missing ? el("span", { class: "bad" }, `📍 ${missing} without a place`) : null);
  // Fill in what the photos currently say (estimates, notes).
  Promise.all(g.items.slice(0, 40).map((a) => api.developGet(a.id).catch(() => null))).then((all) => {
    const texts = all.map((s) => s?.text).filter(Boolean);
    const places = [...new Set(texts.map((t) => t.place).filter(Boolean))];
    const notes = [...new Set(texts.filter((t) => t.placeNote).map((t) => t.placeNote))].slice(0, 2);
    const fileDates = texts.filter((t) => t.dateFrom === "fileName" || t.dateFrom === "fileDate").length;
    if (places.length === 1) place.value = places[0];
    const subj = [...new Set(texts.map((t) => t.subject).filter(Boolean))];
    if (subj.length === 1) subject.value = subj[0];
    status.append(
      places.length ? el("span", { class: "warn" }, `${missing ? " · " : ""}≈ ${places.join(" / ")}${notes.length ? ` (${notes.join("; ")})` : ""}`) : "",
      fileDates ? el("span", { class: "warn" }, ` · ${fileDates} date${fileDates === 1 ? "" : "s"} from the file name`) : "",
    );
  });
  const save = async () => {
    try {
      const p = place.value.trim();
      const sub = subject.value.trim();
      if (p || sub) await api.textEdit(ids, sub || null, p || null);
      await api.textConfirm(ids);
      remember("place", p);
      remember("subject", sub);
      changed(ids);
      row.classList.add("done");
      setTimeout(rerender, 700);
    } catch (e) {
      toast(String(e), true);
    }
  };
  place.onkeydown = subject.onkeydown = (e) => e.key === "Enter" && (e.preventDefault(), save());
  row.append(
    el("div", { class: "group-head" }, el("b", {}, when), el("span", { class: "muted" }, `${g.items.length} photo${g.items.length === 1 ? "" : "s"}`)),
    el("div", { class: "thumbs" }, ...g.items.slice(0, 10).map((a) => el("img", {
      src: urls.thumb(a), alt: a.fileName, title: `${a.fileName} — click to look closer`, loading: "lazy",
      onclick: () => { $("review-dialog").close(); emit("map-open", a.id); },
    }))),
    status,
    datalist("rv-recent-place", "place"), datalist("rv-recent-subject", "subject"),
    el("div", { class: "row" }, place, subject,
      el("button", { type: "button", class: "btn small primary", onclick: save }, "✓ Save")),
  );
  return row;
}

// ------------------------------------------------------ properties panel

/** Subject/place fields for the selected photos (Manage → right panel). */
export async function propsSection(asset, ids) {
  const s = await api.developGet(asset.id).catch(() => null);
  const t = s?.text;
  const n = ids.length;
  const subject = el("input", { type: "text", list: "tx-recent-subject", placeholder: n > 1 ? `Subject for ${n} photos` : "What / where exactly (e.g. Vicuña)", value: n > 1 ? "" : t?.subject || "" });
  const place = el("input", { type: "text", list: "tx-recent-place", placeholder: n > 1 ? `Place for ${n} photos` : "Place (e.g. Lima)", value: n > 1 ? "" : t?.place || "", class: asset.needsPlace && n === 1 ? "needs" : "" });
  const save = async (field, input) => {
    const value = input.value.trim();
    if (n > 1 && !value) return;
    try {
      await api.textEdit(ids, field === "subject" ? value : null, field === "place" ? value : null);
      remember(field, value);
      changed(ids);
      toast(n > 1 ? `Saved for ${n} photos` : "Saved");
    } catch (e) {
      toast(String(e), true);
    }
  };
  subject.onchange = () => save("subject", subject);
  place.onchange = () => save("place", place);
  const kids = [
    el("h3", {}, "Text on photo"),
    datalist("tx-recent-subject", "subject"), datalist("tx-recent-place", "place"),
    subject, place,
  ];
  const NOTES = { nearby: "estimated from a nearby photo", phone: "estimated from a phone photo", timeline: "estimated from your location history" };
  if (n === 1 && t?.place && NOTES[t.placeFrom]) {
    kids.push(el("p", { class: "est-note" }, `≈ ${t.placeNote || NOTES[t.placeFrom]}`,
      el("button", { class: "btn small", onclick: () => confirm(ids) }, "✓ Correct")));
  }
  if (n === 1 && t?.date) {
    const dnote = { fileName: "from the file name — for WhatsApp that's when it was sent", fileDate: "from the file's date — may be the copy date" }[t.dateFrom];
    kids.push(el("p", { class: dnote ? "est-note" : "hint" }, `Date: ${t.date}${dnote ? ` (≈ ${dnote})` : ""}`,
      dnote ? el("button", { class: "btn small", onclick: () => confirm(ids) }, "✓ Correct") : null));
  }
  if (n > 1 && store.assets.some((a) => ids.includes(a.id) && a.toCheck)) {
    kids.push(el("button", { class: "btn small", title: "Accept the estimated places and dates of the selected photos", onclick: () => confirm(ids) }, `✓ Estimates correct for ${n}`));
  }
  if (n === 1 && t?.place) {
    kids.push(el("button", { class: "btn small", title: "Change this place name on every photo that has it", onclick: async () => {
      const to = place.value.trim();
      if (!to || to === t.place) return toast("Type the new name in the Place box first");
      const done = await api.replacePlace(t.place, to).catch((e) => (toast(String(e), true), null));
      if (done) {
        remember("place", to);
        changed(done);
        toast(`“${t.place}” → “${to}” on ${done.length} photo${done.length === 1 ? "" : "s"}`);
      }
    } }, `Rename “${t.place}” everywhere`));
  }
  if (!t) kids.push(el("button", { class: "btn small", onclick: () => apply(ids, prefs()) }, n > 1 ? `Add date & place to ${n}` : "Add date & place"));
  else kids.push(el("button", { class: "btn small", onclick: async () => {
    await api.textRemove(ids).catch((e) => toast(String(e), true));
    changed(ids);
  } }, n > 1 ? `Remove text from ${n}` : "Remove text"));
  return el("div", { class: "text-props" }, ...kids);
}

// ---------------------------------------------------------- Develop panel

/**
 * Full editor for the current photo's text in Develop. `ctx` is the develop
 * context (dev, change, commit). Returns { el, refresh }.
 */
export function developSection(ctx) {
  const box = el("div", {});
  const refresh = () => {
    const t = ctx.dev.s.text;
    if (!t) {
      box.replaceChildren(
        el("p", { class: "hint" }, "No text on this photo."),
        el("button", { class: "btn small", onclick: async () => {
          const id = ctx.dev.id;
          await apply([id], prefs());
          const fresh = await api.developGet(id).catch(() => null);
          if (fresh && id === ctx.dev.id) {
            ctx.change((s) => (s.text = fresh.text));
            ctx.commit();
            refresh();
          }
        } }, "Add date & place"));
      return;
    }
    const field = (key, placeholder, kind) => {
      const i = el("input", { type: "text", value: t[key] || "", placeholder, list: kind ? `dv-recent-${kind}` : null, class: key === "place" && !t.place ? "needs" : "" });
      i.oninput = () => ctx.change((s) => (s.text[key] = i.value));
      i.onchange = () => {
        ctx.commit();
        if (kind) remember(kind, i.value);
      };
      return i;
    };
    const pick = (key, options) => {
      const s = select(null, options, t[key]);
      s.onchange = () => {
        ctx.change((st) => (st.text[key] = s.value));
        ctx.commit();
      };
      return s;
    };
    const check = (key, label) => el("label", { class: "check" },
      el("input", { type: "checkbox", checked: !!t[key], onchange: (e) => {
        ctx.change((s) => (s.text[key] = e.target.checked));
        ctx.commit();
      } }), label);
    const size = el("input", { type: "range", min: 1.5, max: 6, step: 0.25, value: t.size });
    size.oninput = () => ctx.change((s) => (s.text.size = Number(size.value)));
    size.onchange = ctx.commit;
    const est = { nearby: 1, phone: 1, timeline: 1 }[t.placeFrom];
    const placeField = field("place", "Place (e.g. Lima)", "place");
    placeField.addEventListener("change", () => {
      ctx.change((s) => ((s.text.placeFrom = "typed"), (s.text.placeNote = "")));
      ctx.commit();
    });
    box.replaceChildren(
      datalist("dv-recent-subject", "subject"), datalist("dv-recent-place", "place"),
      field("subject", "What / where exactly (e.g. Vicuña)", "subject"),
      placeField,
      est ? el("p", { class: "est-note" }, `≈ ${t.placeNote || "estimated"}`, el("button", { class: "btn small", onclick: () => {
        ctx.change((s) => (s.text.placeFrom = "typed"));
        ctx.commit();
        refresh();
      } }, "✓ Correct")) : null,
      (() => {
        const f = field("date", "Date (e.g. 23.09.2026)");
        f.addEventListener("change", () => {
          ctx.change((s) => (s.text.dateFrom = "typed"));
          ctx.commit();
        });
        return f;
      })(),
      el("div", { class: "cap-grid" },
        el("label", {}, "Position"), pick("position", POSITIONS),
        el("label", {}, "Colour"), pick("color", COLORS),
        el("label", {}, "Size"), size,
        check("italic", "Italic"), check("bold", "Bold")),
      el("button", { class: "btn small", onclick: () => {
        ctx.change((s) => (s.text = null));
        ctx.commit();
        refresh();
      } }, "Remove text"),
    );
  };
  refresh();
  return { el: box, refresh };
}

export function init() {
  $("manage-text").onclick = openDialog;
  $("review-dialog").addEventListener("close", () => emit("library-changed"));
  api.listen("text-progress", ({ done, total }) => {
    if (total > 3 && (done % 5 === 0 || done === total)) toast(`Adding text ${done}/${total}…`);
  });
}
