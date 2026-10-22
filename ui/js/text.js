// Text on photos ("subject, place, date"): the batch "Date & place…"
// dialog in Manage, the subject/place fields in the properties panel, and
// the full editor in Develop. The text is part of each photo's develop
// settings, so it shows on thumbnails and in exports right away.

import { api } from "./api.js";
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
      "The photo's own data comes first (camera date, GPS). Text you typed before is kept. ",
      "Photos without a place get a red 📍 so you can type it in. Place names come from OpenStreetMap ",
      "(GPS positions, rounded to ~100 m, are sent to openstreetmap.org). Originals are never changed."),
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
    if (res.needPlace) parts.push(`${res.needPlace} need a place (📍)`);
    if (res.noDate) parts.push(`${res.noDate} without a date`);
    if (res.lookupFailed) parts.push("place lookup failed for some (offline?) — run it again later");
    toast(parts.join(" · "), res.needPlace > 0 || res.lookupFailed);
  } catch (e) {
    toast(String(e), true);
  }
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
  if (n === 1 && t?.date) kids.push(el("p", { class: "hint" }, `Date: ${t.date}`));
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
    box.replaceChildren(
      datalist("dv-recent-subject", "subject"), datalist("dv-recent-place", "place"),
      field("subject", "What / where exactly (e.g. Vicuña)", "subject"),
      field("place", "Place (e.g. Lima)", "place"),
      field("date", "Date (e.g. 23.09.2026)"),
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
  api.listen("text-progress", ({ done, total }) => {
    if (total > 3 && (done % 5 === 0 || done === total)) toast(`Adding text ${done}/${total}…`);
  });
}
