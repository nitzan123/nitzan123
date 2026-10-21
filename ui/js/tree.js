// Collapsible tree for the sidebar (folders, calendar) plus the pure
// builders that turn flat catalog data into tree nodes.
//
// Node: { key, label, count, title?, children: Node[], extra? }

import { el } from "./state.js";

/**
 * Renders `nodes` into `ul`. `expanded` (a Set of keys) is mutated as the
 * user opens and closes branches, so it survives re-renders.
 */
export function renderTree(ul, nodes, { expanded, activeKey, onSelect }) {
  const row = (n, depth) => {
    const open = expanded.has(n.key);
    const hasKids = n.children.length > 0;
    const toggle = (e) => {
      e?.stopPropagation();
      open ? expanded.delete(n.key) : expanded.add(n.key);
      renderTree(ul, nodes, { expanded, activeKey, onSelect });
      ul.querySelector(`[data-key="${CSS.escape(n.key)}"]`)?.focus();
    };
    const li = el("li", {
      class: "tree-row" + (n.key === activeKey ? " active" : ""),
      "data-key": n.key, tabindex: 0, role: "treeitem", title: n.title || n.label,
      "aria-expanded": hasKids ? String(open) : null, "aria-current": n.key === activeKey ? "true" : null,
      style: `padding-left:${4 + depth * 12}px`,
      onclick: () => onSelect(n),
      onkeydown: (e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") (e.preventDefault(), onSelect(n));
        else if (hasKids && ((e.key === "ArrowRight" && !open) || (e.key === "ArrowLeft" && open))) (e.preventDefault(), toggle());
      },
    },
    hasKids
      ? el("button", { type: "button", class: "twisty", tabindex: -1, "aria-label": open ? "Collapse" : "Expand", onclick: toggle }, open ? "▾" : "▸")
      : el("span", { class: "twisty" }),
    el("span", { class: "tree-label" }, n.label),
    n.count != null ? el("span", { class: "count" }, String(n.count)) : null,
    n.extra || null);
    return [li, ...(open ? n.children.flatMap((c) => row(c, depth + 1)) : [])];
  };
  ul.setAttribute("role", "tree");
  ul.replaceChildren(...nodes.flatMap((n) => row(n, 0)));
}

/**
 * Folder paths with photo counts → tree. Chains of folders without photos
 * of their own are collapsed ("/home/me/Pictures" shows as "Pictures"),
 * and counts include everything below.
 */
export function folderTree(folders) {
  const byPath = new Map();
  const node = (path, label) => {
    let n = byPath.get(path);
    if (!n) byPath.set(path, (n = { key: path, path, label, own: 0, count: 0, children: [] }));
    return n;
  };
  const roots = [];
  for (const f of folders) {
    // Prefixes at each separator: "/a/b" → "/a", "/a/b"; "C:\x" → "C:", "C:\x".
    const cuts = [];
    for (let i = 0; i < f.path.length; i++) if ((f.path[i] === "/" || f.path[i] === "\\") && i > 0) cuts.push(i);
    const prefixes = [...cuts.map((i) => f.path.slice(0, i)), f.path].filter((p, i, a) => p && a.indexOf(p) === i);
    let parent = null;
    for (const p of prefixes) {
      const isNew = !byPath.has(p);
      const n = node(p, p.split(/[\\/]/).filter(Boolean).pop() || p);
      if (isNew) (parent ? parent.children : roots).push(n);
      parent = n;
    }
    parent.own += f.count;
  }
  const total = (n) => (n.count = n.own + n.children.reduce((s, c) => s + total(c), 0));
  // Collapse photo-less single-child chains; nested chains keep the joined
  // relative path as their label ("2023/Tokyo").
  const squash = (n, parentPath = null) => {
    while (n.own === 0 && n.children.length === 1) n = { ...n.children[0] };
    if (parentPath) n.label = n.path.slice(parentPath.length + 1) || n.label;
    n.title = n.path;
    n.children = n.children.map((c) => squash(c, n.path)).sort((a, b) => a.label.localeCompare(b.label, undefined, { numeric: true }));
    return n;
  };
  roots.forEach(total);
  return roots.map((r) => squash(r));
}

const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/** `[{ day: "YYYY-MM-DD", count }]` → years › months › days, newest first. */
export function calendarTree(days) {
  const years = new Map();
  for (const { day, count } of days) {
    const [y, m] = day.split("-");
    if (!y || !m) continue;
    if (!years.has(y)) years.set(y, { key: y, label: y, count: 0, from: `${y}-01-01`, to: `${y}-12-31`, months: new Map() });
    const yr = years.get(y);
    yr.count += count;
    const mk = `${y}-${m}`;
    if (!yr.months.has(mk)) {
      yr.months.set(mk, { key: mk, label: MONTHS[Number(m) - 1] || m, count: 0, from: `${mk}-01`, to: `${mk}-31`, children: [] });
    }
    const mo = yr.months.get(mk);
    mo.count += count;
    mo.children.push({ key: day, label: String(Number(day.slice(8, 10))), count, from: day, to: day, children: [] });
  }
  return [...years.values()]
    .sort((a, b) => b.key.localeCompare(a.key))
    .map(({ months, ...y }) => ({ ...y, children: [...months.values()].sort((a, b) => b.key.localeCompare(a.key)) }));
}
