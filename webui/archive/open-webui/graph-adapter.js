export function graphToFlow(doc) {
const RESERVED = ["PICK", "EMPTY", "ERROR"];
function walk(states, prefix, out) {
  for (const [name, state] of Object.entries(states || {})) {
    const path = prefix ? prefix + "." + name : name;
    out.push({ path, name, state, parent: prefix });
    if (state && state.states) walk(state.states, path, out);
  }
  return out;
}

// `graph.py::resolve_target`, used to *draw* an edge rather than to
// judge one: `#id.a.b` is absolute, a bare name is a sibling. A target
// this cannot resolve gets no edge and is shown on the node as
// unresolved — which is a thing this file could not draw, not a verdict
// about the graph. `_lint_targets` is what decides that.
function resolve(fromPath, target, known) {
  if (typeof target !== "string" || target === "") return null;
  if (target.startsWith("#")) {
    const parts = target.slice(1).split(".").filter((p) => p !== "");
    if (!parts.length || parts[0] !== doc.id) return null;
    const path = parts.slice(1).join(".");
    return known.has(path) ? path : null;
  }
  const parent = fromPath.includes(".") ? fromPath.slice(0, fromPath.lastIndexOf(".")) : "";
  const path = parent ? parent + "." + target : target;
  return known.has(path) ? path : null;
}

/** `always` / `on` / `onDone` entries normalised, as `as_transition_list`. */
function asList(value) {
  if (typeof value === "string") return [{ target: value }];
  if (Array.isArray(value)) return value.map((v) => (typeof v === "string" ? { target: v } : v));
  if (value && typeof value === "object") return [value];
  return [];
}

// --- reading a graph into nodes and edges ---------------------------------

/** One guard, as one line. Shape only — never a judgement. */
function guardLine(guard) {
  if (!guard || typeof guard !== "object") return "(no guard)";
  const p = guard.params || {};
  switch (guard.type) {
    case "equals": case "contains":
      return `${guard.type} ${p.path} = ${JSON.stringify(p.value)}`;
    case "matches": return `matches ${p.path} ~ ${p.pattern}`;
    case "exists": return `exists ${p.path}`;
    case "count": {
      const bounds = [p.gte !== undefined ? `>= ${p.gte}` : null, p.lte !== undefined ? `<= ${p.lte}` : null];
      return `count ${p.path} ${bounds.filter(Boolean).join(" ")}`.trim();
    }
    case "entails": case "contradicts":
      return `${guard.type} "${p.hypothesis || ""}"`;
    case "not": return "not …";
    case "and": case "or": return `${guard.type} of ${(p.guards || []).length}`;
    default: return String(guard.type);
  }
}

/** One action, as one line. */
function actionLine(action) {
  if (!action || typeof action !== "object") return "(malformed)";
  const p = action.params || {};
  switch (action.type) {
    case "tool": return `tool ${p.name} → ${p.into || "obs"}`;
    case "assign": return `assign ${Object.keys(p).join(", ")}`;
    case "push": return `push ${p.path}`;
    case "inc": return `inc ${p.path}${p.by !== undefined ? " by " + p.by : ""}`;
    case "capture": return `capture ${p.path} → ${p.into}`;
    default: return String(action.type);
  }
}

/** The source word and its one-line detail, for the node's choose band. */
function chooseBand(state, problems) {
  const meta = state.meta || {};
  const choose = meta.choose;
  if (!choose) return null;
  const from = choose.from;
  let source = "?";
  let detail = "";
  let count = null;
  if (from === "transitions") {
    source = "transitions";
    const events = Object.keys(state.on || {}).filter((e) => !RESERVED.includes(e));
    count = events.length;
    detail = events.join(" / ");
  } else if (from && typeof from === "object") {
    if (from.refs) {
      source = "refs";
      const refs = from.refs;
      const roles = (refs.roles || ["link", "button"]).join("/");
      detail = refs.within ? `${roles} within ${refs.within}` : roles;
    } else if (from.menu) {
      source = "menu";
      count = from.menu.length + (choose.also ? choose.also.length : 0);
    } else if (from.lines) {
      source = "lines";
      detail = from.lines.of || "context.obs.text";
    }
  }
  if (choose.max !== undefined && count === null) count = choose.max;
  const floor = meta.floor !== undefined ? meta.floor : defaultOf("floor", 0.5);
  const picks = from === "transitions" ? true : Object.keys(state.on || {}).includes("PICK");
  return { source, detail, count, floor: String(floor), picks, problems };
}

function defaultOf(key, fallback) {
  const d = ((doc.meta || {}).jev || {}).defaults || {};
  return d[key] !== undefined ? d[key] : fallback;
}

/**
 * The whole translation: a graph document in, `{nodes, edges}` in
 * Svelte Flow's own shape out.
 *
 * A node is drawn as the four bands of one decision cycle, in the order
 * automation.md's "The model in one page" states them — observe, check,
 * choose, act. Each `always` row and each `on` event is its own source
 * handle, so an edge leaves the line of the graph that causes it.
 */
function toFlow(problemsByPath) {
  const all = walk(doc.states, "", []);
  const known = new Set(all.map((s) => s.path));
  const nodes = [];
  const edges = [];
  const placed = layout(all);
  const failure = new Set();
  const pending = asList((doc.on || {}).ERROR).map(t => t.target);
  while (pending.length) {
    const key = pending.shift();
    if (!doc.states[key] || key === doc.initial || failure.has(key)) continue;
    failure.add(key);
    for (const transition of Object.values(doc.states[key].on || {})) {
      for (const t of asList(transition)) if (t.target) pending.push(t.target);
    }
  }

  for (const { path, name, state, parent } of all) {
    const meta = state.meta || {};
    const compound = !!state.states;
    const problems = (problemsByPath.get(path) || []).length;
    const data = {
      label: name,
      failure: failure.has(path),
      kind: state.type || "atomic",
      note: state.description || "",
      observe: (state.entry || []).map(actionLine),
      check: [],
      choose: compound ? null : chooseBand(state, problems),
      act: [],
      problems,
      done: null,
    };

    asList(state.always).forEach((t, i) => {
      const to = resolve(path, t.target, known);
      data.check.push({
        id: `always-${i}`,
        text: guardLine(t.guard) + (t.target ? ` → ${to ? t.target : t.target + " (?)"}` : " (stay)"),
        to,
      });
      if (to) {
        edges.push(edge(path, `always-${i}`, to, guardLine(t.guard), true, i + 1));
      }
    });

    for (const [event, entry] of Object.entries(state.on || {})) {
      asList(entry).forEach((t, i) => {
        const to = resolve(path, t.target, known);
        const handle = `on-${event}-${i}`;
        data.act.push({
          id: handle,
          event,
          mark: t.target === undefined ? "internal" : t.reenter ? "↻" : "",
        });
        if (to) edges.push(edge(path, handle, to, event, false, null));
      });
    }

    if (compound) {
      const done = asList(state.onDone)[0];
      if (done) {
        const to = resolve(path, done.target, known);
        data.done = done.target || "(internal)";
        if (to) edges.push(edge(path, "onDone", to, "onDone", true, null));
      }
    }

    // A history state points at its fallback, which is a real edge and
    // the only way to see where a resumed phase lands when the parent
    // was never entered.
    if (state.type === "history" && state.target) {
      const to = resolve(path, state.target, known);
      if (to) edges.push(edge(path, "in", to, "history", true, null));
    }

    data.auto = placed.get(path + " auto") === true;
    const node = {
      id: path,
      type: compound ? "stateGroup" : "state",
      position: placed.get(path),
      data,
      draggable: true,
    };
    if (compound) {
      node.width = placed.get(path + " w") || 420;
      node.height = placed.get(path + " h") || 260;
    }
    if (parent) {
      node.parentId = parent;
      node.extent = "parent";
    }
    nodes.push(node);
  }

  // A parent must come before its children, or the canvas has nothing
  // to place them inside.
  nodes.sort((a, b) => a.id.split(".").length - b.id.split(".").length);
  return { nodes, edges };
}

function edge(from, handle, to, label, dashed, order) {
  return {
    id: `${from}|${handle}|${to}`,
    source: from,
    sourceHandle: handle,
    target: to,
    label: order ? `check ${order}` : label,
    pathOptions: { borderRadius: 0, offset: 32 },
    type: "transition",
    markerEnd: { type: "arrowclosed" },
    animated: false,
    class: dashed ? "jev-edge jev-edge--always" : "jev-edge",
  };
}

/**
 * Where each node goes.
 *
 * `meta.editor` first, always — that is the slot `schema.json` reserves
 * for exactly this (`"editor": { "type": "object" }`, under the graph's
 * `meta` and under every state's), and the runner never reads it. A
 * state with no saved position is laid out by depth and document order.
 *
 * Opening a graph never writes that computed layout back: open a file,
 * close it, and it is unchanged. The first *drag* does commit it — and
 * commits it for every node, not only the dragged one. See `moved`.
 */
function layout(all) {
  const at = new Map();
  const perParent = new Map();
  // Where a history state's position is kept. It cannot be kept on the
  // state; see `moved`.
  const aside = (((doc || {}).meta || {}).editor || {}).states || {};
  for (const { path, state, parent } of all) {
    const saved = state.type === "history"
      ? (aside[path] || {})
      : ((state.meta || {}).editor || {});
    if (typeof saved.x === "number" && typeof saved.y === "number") {
      at.set(path, { x: saved.x, y: saved.y });
      continue;
    }
    const key = parent || "";
    const n = perParent.get(key) || 0;
    perParent.set(key, n + 1);
    at.set(path, parent ? { x: 28, y: 64 + n * 190 } : { x: 40 + n * 330, y: 40 });
    // Nobody placed this one, so the canvas may restack it once it knows
    // how tall it turned out. A state whose position came out of
    // `meta.editor` above is never touched again.
    at.set(path + " auto", true);
  }
  return at;
}


return toFlow(new Map());
}
