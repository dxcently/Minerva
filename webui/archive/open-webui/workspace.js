import { mountCanvas } from './jev-flow.js';
import { graphToFlow } from './graph-adapter.js';

/* mw:pure-start */
/* DOM-free helpers. webui/tests/*.mjs extracts this block and exercises it directly. */
const transitionsOf = value => (Array.isArray(value) ? value : value ? [value] : []).map(t => typeof t === 'string' ? { target: t } : t);
const scoreLabel = value => typeof value === 'number' ? (value * 100).toFixed(1) + '%' : 'unscored';
function nodeAtPath(doc, path) {
  let node = doc;
  for (const part of String(path).split('.')) {
    if (!node || !node.states) return undefined;
    node = node.states[part];
  }
  return node;
}
function resolveTargetRef(doc, ownerPath, target) {
  if (typeof target !== 'string' || !target) return null;
  const id = doc && doc.id;
  if (id && target.startsWith('#' + id + '.')) return target.slice(String(id).length + 2);
  const parts = String(ownerPath || '').split('.').slice(0, -1);
  return parts.length ? parts.join('.') + '.' + target : target;
}
function removeState(doc, path) {
  const removed = String(path);
  if (!removed || !nodeAtPath(doc, removed)) return false;
  const keep = (value, ownerPath) => transitionsOf(value).filter(t => {
    const resolved = resolveTargetRef(doc, ownerPath, t && t.target);
    return resolved !== removed && !(typeof resolved === 'string' && resolved.startsWith(removed + '.'));
  });
  const prune = (owner, prefix = '') => {
    for (const key of ['always', 'onDone']) if (owner[key]) {
      const rows = keep(owner[key], prefix);
      if (rows.length) owner[key] = rows; else delete owner[key];
    }
    for (const [event, value] of Object.entries(owner.on || {})) {
      const rows = keep(value, prefix);
      if (rows.length) owner.on[event] = rows; else delete owner.on[event];
    }
    for (const [name, state] of Object.entries(owner.states || {})) {
      const child = prefix ? prefix + '.' + name : name;
      if (child === removed) delete owner.states[name]; else prune(state, child);
    }
    if (owner.initial && !owner.states?.[owner.initial]) {
      const next = Object.keys(owner.states || {}).find(name => owner.states[name]?.type !== 'history');
      if (next) owner.initial = next; else delete owner.initial;
    }
  };
  prune(doc);
  return true;
}
function pushDraft(history, future, previous, next) {
  if (!previous || previous === next) return false;
  history.push(previous);
  if (history.length > 100) history.shift();
  future.length = 0;
  return true;
}
const FINISHED_RUN_STATUS = ['reached', 'finished', 'done', 'completed', 'success', 'succeeded', 'exhausted'];
const STOPPED_RUN_STATUS = ['stopped', 'cancelled', 'canceled', 'aborted'];
const FAILED_RUN_STATUS = ['failed', 'error', 'errored', 'orphaned'];
function runStatusFor(run) {
  if (!run) return { key: 'none', label: 'No run' };
  const raw = String(run.status || run.outcome || '').toLowerCase();
  if (FAILED_RUN_STATUS.includes(raw)) return { key: 'failed', label: 'Failed' };
  if (STOPPED_RUN_STATUS.includes(raw)) return { key: 'stopped', label: 'Stopped' };
  if (FINISHED_RUN_STATUS.includes(raw)) return { key: 'finished', label: 'Finished' + (raw && raw !== 'finished' ? ' · ' + raw : '') };
  if (run.escalation) return { key: 'waiting', label: 'Waiting for human' };
  return { key: 'running', label: 'Running' };
}
function isLiveRun(run) {
  const key = runStatusFor(run).key;
  return key === 'running' || key === 'waiting';
}
function latestOrder(run) {
  const orders = Array.isArray(run?.orders) ? run.orders : [];
  if (!orders.length) return null;
  const live = orders.filter(order => String(order?.status || '') !== 'superseded');
  return (live.length ? live : orders).at(-1);
}
function orderStatusLabel(order) {
  const status = String(order?.status || '').toLowerCase();
  if (status === 'pending') return 'Pending · acknowledged, not yet applied';
  if (status === 'applied') return 'Applied by the run';
  if (status === 'superseded') return 'Superseded';
  return status ? status[0].toUpperCase() + status.slice(1) : 'Recorded';
}
/* mw:pure-end */

const el = (tag, text, cls) => {
  const node = document.createElement(tag);
  if (text) node.textContent = text;
  if (cls) node.className = cls;
  return node;
};
/* Icon paths only; no text glyphs pretending to be icons. */
const ICONS = {
  panel: '<path d="M3 4h18v16H3zM15 4v16M18 8v8"/>',
  close: '<path d="M5 5l14 14M19 5L5 19"/>',
  expand: '<path d="M4 9V4h5M20 15v5h-5M20 9V4h-5M4 15v5h5"/>',
  save: '<path d="M4 4h12l4 4v12H4zM8 4v6h8V4M8 20v-6h8v6"/>',
  run: '<path d="M6 4l14 8-14 8z"/>',
  undo: '<path d="M8 7L3 12l5 5M3 12h11a5 5 0 0 1 0 10"/>',
  redo: '<path d="M16 7l5 5-5 5M21 12H10a5 5 0 0 0 0 10"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  copy: '<path d="M9 9h11v11H9zM4 15V4h11"/>',
  trash: '<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>',
  start: '<path d="M6 4l12 8-12 8zM20 4v16"/>',
  import: '<path d="M12 3v11M8 10l4 4 4-4M4 19h16"/>',
  export: '<path d="M12 15V4M8 8l4-4 4 4M4 19h16"/>',
  refresh: '<path d="M20 12a8 8 0 1 1-3-6.2M20 4v4h-4"/>',
  menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
  order: '<path d="M4 5h16M4 10h10M4 15h13M4 20h7"/>',
  node: '<path d="M5 5h6v6H5zM13 13h6v6h-6zM11 8h4v7"/>',
};
const svg = (name, size = 16) => {
  const node = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  node.setAttribute('viewBox', '0 0 24 24'); node.setAttribute('width', size); node.setAttribute('height', size);
  node.setAttribute('fill', 'none'); node.setAttribute('stroke', 'currentColor');
  node.setAttribute('stroke-width', '1.7'); node.setAttribute('stroke-linecap', 'round');
  node.setAttribute('aria-hidden', 'true'); node.setAttribute('focusable', 'false');
  node.innerHTML = ICONS[name] || '';
  return node;
};
const button = (text, action, opts = {}) => {
  const node = el('button', '', opts.cls);
  node.type = opts.type || 'button';
  if (opts.variant) node.classList.add('mw-btn-' + opts.variant);
  if (opts.icon) node.append(svg(opts.icon));
  if (text) node.append(el('span', text, 'mw-label'));
  if (opts.title) node.title = opts.title;
  if (opts.ariaLabel) node.setAttribute('aria-label', opts.ariaLabel);
  if (opts.disabled) node.disabled = true;
  if (action) node.onclick = action;
  return node;
};
const setButtonLabel = (node, label) => {
  const span = node.querySelector('.mw-label');
  if (span) span.textContent = label; else node.textContent = label;
};
const iconButton = (name, label, action, opts = {}) => button('', action, {
  ...opts, icon: name, title: opts.title || label, ariaLabel: label,
  cls: (opts.cls ? opts.cls + ' ' : '') + 'mw-icon-button',
});
const field = (label, control, hint) => {
  const wrap = el('label', '', 'mw-field');
  wrap.append(el('span', label, 'mw-field-label'), control);
  if (hint) wrap.append(el('small', hint, 'mw-field-hint'));
  return wrap;
};
const textArea = (label, value, opts = {}) => {
  const node = el('textarea', '', opts.cls);
  node.rows = opts.rows || 3;
  node.value = value ?? '';
  node.spellcheck = false;
  node.setAttribute('aria-label', label);
  if (opts.placeholder) node.placeholder = opts.placeholder;
  if (opts.maxLength) node.maxLength = opts.maxLength;
  return node;
};
const textInput = (label, value, opts = {}) => {
  const node = el('input', '', opts.cls);
  node.type = opts.type || 'text';
  if (opts.type === 'number') { node.min = String(opts.min ?? 0); node.max = String(opts.max ?? 1); node.step = String(opts.step ?? 0.01); }
  if (value != null) node.value = value;
  node.setAttribute('aria-label', label);
  if (opts.placeholder) node.placeholder = opts.placeholder;
  return node;
};
const actionRow = (...controls) => {
  const row = el('div', '', 'mw-actions');
  row.append(...controls.filter(Boolean));
  return row;
};
const emptyState = (text, ...controls) => {
  const box = el('div', '', 'mw-empty');
  box.append(el('p', text));
  if (controls.length) box.append(actionRow(...controls));
  return box;
};
/* Secondary graph-level actions live behind one accessible disclosure, not a wall of buttons. */
const actionMenu = (label, items) => {
  const details = el('details', '', 'mw-menu');
  const summary = el('summary');
  summary.append(svg('menu'), el('span', label, 'mw-label'));
  details.append(summary, ...items);
  details.addEventListener('click', event => { if (event.target.closest('.mw-menu-item')) details.open = false; });
  return details;
};
const menuItem = (text, action, opts = {}) => button(text, action, { ...opts, cls: 'mw-menu-item' });

const panel = el('aside', '', 'minerva-workspace');
panel.id = 'minerva-workspace'; panel.hidden = true; panel.setAttribute('aria-label', 'Activity panel');
const header = el('header');
const message = el('p', '', 'mw-message'); message.setAttribute('role', 'status');
const content = el('div', '', 'mw-content');
let tab = 'graph', canvas, timer, current = '', baseline = '', dirty = false, graphRows = [], activeRun;
let generation = 0, refresh = async () => {};
let orderDraft = '', orderRequestId = null, orderRequestBody = '', controlJob = null;
function panelHeader(full) {
  const title = el('b', full ? (tab === 'graph' ? 'Graph editor' : 'Extensions') : 'Live activity');
  header.replaceChildren(title);
  if (!full) {
    const tabs = el('div', '', 'mw-tabs');
    tabs.setAttribute('role', 'tablist'); tabs.setAttribute('aria-label', 'Panel views');
    for (const [key, label] of [['graph', 'Graph'], ['files', 'Files'], ['activity', 'Activity']]) {
      const control = button(label, () => open(key), { cls: 'mw-tab' });
      control.setAttribute('role', 'tab'); control.setAttribute('aria-selected', String(tab === key));
      if (tab === key) control.classList.add('mw-active');
      tabs.append(control);
    }
    header.append(tabs, iconButton('expand', 'Expand to full graph editor', () => open('graph', { full: true })));
  }
  header.append(iconButton('close', full ? 'Close graph editor' : 'Close activity panel', close));
}
panel.append(header, message, content); document.body.append(panel);
const launch = iconButton('panel', 'Open activity panel', () => panel.hidden ? open('graph') : close());
launch.id = 'minerva-workspace-toggle'; launch.setAttribute('aria-expanded', 'false');
document.body.append(launch);
function placeLauncher() {
  const controls = document.querySelector('#chat-container button[aria-label="Controls"]');
  launch.hidden = !controls;
  if (controls && controls.nextElementSibling !== launch) controls.after(launch);
}
document.addEventListener('click', event => {
  if (event.target.closest('#chat-container button[aria-label="Controls"]') && !panel.hidden) {
    close();
    if (!panel.hidden) { event.preventDefault(); event.stopImmediatePropagation(); }
  }
}, true);
let contentHost;
function fitPanel() {
  if (panel.hidden) return;
  const main = document.querySelector('#main-content');
  const host = main && getComputedStyle(main).display === 'contents' ? [...main.children].find(e => e.getBoundingClientRect().width > 0) : main;
  if (host && host !== contentHost) { contentHost = host; hostObserver.observe(host); }
  const rect = host?.getBoundingClientRect();
  const left = Math.max(0, Math.round(rect?.left || 0));
  const top = Math.max(0, Math.round(rect?.top || 0));
  const available = innerWidth - left;
  const full = panel.classList.contains('mw-full');
  const width = Math.min(560, available >= 920 ? Math.max(360, Math.round(available * .4)) : available);
  const reserve = !full && available >= 920 && host?.id === 'chat-container' ? width : 0;
  document.body.style.setProperty('--mw-panel-reserve', reserve + 'px');
  document.body.style.setProperty('--mw-chat-width', (available - reserve) + 'px');
  document.body.classList.toggle('minerva-panel-docked', reserve > 0);
  panel.style.left = (full ? left : innerWidth - width) + 'px';
  panel.style.right = '0px';
  panel.style.top = top + 'px';
}
const hostObserver = new ResizeObserver(fitPanel);
window.addEventListener('resize', fitPanel);
document.addEventListener('transitionend', fitPanel);
for (const file of ['jev-flow.css', 'workspace.css']) {
  const link = el('link'); link.rel = 'stylesheet'; link.href = '/static/' + file; document.head.append(link);
}
async function api(path, data) {
  const options = {headers: {Authorization: 'Bearer ' + (localStorage.getItem('token') || '')}, cache: 'no-store', signal: AbortSignal.timeout(20000)};
  if (data) { options.method = 'POST'; options.headers['Content-Type'] = 'application/json'; options.body = JSON.stringify(data); }
  const response = await fetch('/api/v1/minerva/workspace/' + path, options);
  const result = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(result.detail || `Request failed (${response.status})`);
  return result;
}
function chatId() { return location.pathname.match(/^\/c\/([^/]+)/)?.[1] || sessionStorage.getItem('minerva-run-session'); }
async function sendTool(tool, input) {
  let chat = chatId();
  if (!chat) { chat = 'minerva-run-' + crypto.randomUUID(); sessionStorage.setItem('minerva-run-session', chat); }
  return api('tool', {chat, tool, input});
}
/* Destructive or run-ending actions ask first and report the server error; an icon or tooltip is never the only meaning. */
const ask = (question, action) => {
  if (!confirm(question)) return;
  try {
    const result = action();
    if (result && typeof result.catch === 'function') result.catch(error => { message.textContent = error.message; });
  } catch (error) { message.textContent = error.message; }
};
function close() {
  if (dirty && !confirm('Discard unsaved graph edits?')) return;
  dirty = false; panel.hidden = true; launch.setAttribute('aria-expanded', 'false');
  document.body.classList.remove('minerva-panel-open', 'minerva-panel-docked');
  generation++; clearTimeout(timer); canvas?.destroy(); canvas = null;
}
async function poll(epoch) {
  if (epoch !== generation || panel.hidden) return;
  try { await refresh(); } catch (error) { if (epoch === generation) message.textContent = error.message; }
  if (epoch === generation && !panel.hidden) timer = setTimeout(() => poll(epoch), 2000);
}
async function open(next, payload) {
  if (dirty && !confirm('Discard unsaved graph edits?')) return;
  dirty = false; clearTimeout(timer); generation++; canvas?.destroy(); canvas = null;
  const full = next === 'extensions' || payload?.full === true;
  document.querySelector('#controls-container button[aria-label="Close"]')?.click();
  panel.classList.toggle('mw-full', full); panel.classList.toggle('mw-editing', full && next === 'graph');
  panel.classList.remove('mw-wide');
  tab = next; panel.hidden = false; document.body.classList.add('minerva-panel-open');
  panelHeader(full); fitPanel(); launch.setAttribute('aria-expanded', 'true');
  content.replaceChildren(); message.textContent = ''; refresh = async () => {};
  try {
    if (tab === 'graph') await graphs(payload);
    if (tab === 'files') await files(payload?.path || '', payload);
    if (tab === 'activity') await activity();
    if (tab === 'extensions') await extensions();
  } catch (error) { message.textContent = error.message; }
  poll(generation);
}
async function graphs(payload) {
  const epoch = generation;
  const alive = () => epoch === generation && surface.isConnected;
  const editing = panel.classList.contains('mw-editing');
  const toolbar = el('div', '', 'mw-toolbar');
  const graphSelect = el('select', '', 'mw-graph-select'); graphSelect.setAttribute('aria-label', 'Graph');
  const surface = el('div', '', 'mw-canvas'); surface.tabIndex = 0;
  surface.setAttribute('aria-label', 'Graph canvas. Drag from a node handle to connect. Delete removes the selected node.');
  const liveStatus = el('p', 'Waiting for run status', 'mw-live-status'); liveStatus.setAttribute('aria-live', 'polite');
  const editor = textArea('Jev graph JSON', '', { rows: 12, cls: 'mw-json' });
  const jsonFold = el('details', '', 'mw-fold');
  jsonFold.append(el('summary', 'Graph JSON (advanced)'), field('Jev graph JSON', editor));
  const runInput = textArea('Run input JSON', '{}', { rows: 3, cls: 'mw-json' });
  const runFold = el('details', '', 'mw-fold');
  runFold.append(el('summary', 'Run input JSON'), field('Run input JSON', runInput, 'Passed to jev_run as input.'));
  const palette = el('div', '', 'mw-palette mw-editor-only');
  const inspector = el('div', '', 'mw-inspector');
  inspector.append(el('h3', 'Properties'), el('p', 'Select a node to edit its behavior.', 'mw-note'));
  const quick = el('details', '', 'mw-quick');
  quick.append(el('summary', 'Quick edit selected node'));
  const quickFields = el('div', '', 'mw-quick-fields');
  quickFields.append(el('p', 'Select a node to edit its description or confidence threshold.', 'mw-note'));
  quick.append(quickFields);
  let pickedPath = null, history = [], future = [], lastDraft = '', restoring = false;
  let runSignature = '', decisionSignature = '', orderLocal = false;
  const openRuns = new Set();

  function nodeAt(doc, path) { return nodeAtPath(doc, path); }
  function syncHistoryButtons() {
    undoButton.disabled = !history.length; redoButton.disabled = !future.length;
  }
  function restore(from, to) {
    if (!from.length) return;
    to.push(editor.value);
    editor.value = from.pop();
    restoring = true; dirty = editor.value !== baseline; draw(); restoring = false;
    inspect(pickedPath || null);
  }
  function noteDraft(value) { if (!restoring) pushDraft(history, future, lastDraft, value); lastDraft = value; }
  function update(doc) { editor.value = JSON.stringify(doc, null, 2); dirty = editor.value !== baseline; draw(); }
  function draftDoc() { return JSON.parse(editor.value); }
  function setSelectionEnabled(enabled) {
    duplicateButton.disabled = !enabled; deleteNodeButton.disabled = !enabled; startButton.disabled = !enabled;
  }
  const duplicateButton = button('Duplicate', () => duplicateSelected(), { icon: 'copy', disabled: true, title: 'Duplicate the selected node' });
  const deleteNodeButton = button('Delete node', () => deleteSelected(), { icon: 'trash', variant: 'danger', disabled: true, title: 'Delete the selected node and its transitions' });
  const startButton = button('Set as start', () => setStart(), { icon: 'start', disabled: true, title: 'Make the selected node the graph start state' });
  const selectionActions = actionRow(duplicateButton, deleteNodeButton, startButton);

  function duplicateSelected() {
    const id = pickedPath;
    if (!id) { message.textContent = 'Select a node first.'; return; }
    try {
      const doc = draftDoc(); if (!nodeAt(doc, id)) throw new Error('Selection changed. Select the node again.');
      const parts = id.split('.'), name = parts.pop();
      const parent = parts.length ? nodeAt(doc, parts.join('.')) : doc;
      let n = 1; while (parent.states[name + n]) n++;
      const clone = structuredClone(parent.states[name]);
      delete clone.always; delete clone.on; delete clone.onDone;
      if (clone.meta?.editor) { clone.meta.editor.x += 40; clone.meta.editor.y += 40; }
      parent.states[name + n] = clone;
      update(doc); inspect([...parts, name + n].join('.'));
      message.textContent = 'Node duplicated with no transitions; connect it from the previous node.';
    } catch (error) { message.textContent = error.message; }
  }
  function setStart() {
    const id = pickedPath;
    if (!id) { message.textContent = 'Select a node first.'; return; }
    try {
      const doc = draftDoc(); const parts = id.split('.'), name = parts.pop();
      const parent = parts.length ? nodeAt(doc, parts.join('.')) : doc;
      if (!parent?.states?.[name]) throw new Error('Selection changed. Select the node again.');
      if (parent.states[name].type === 'history') { message.textContent = 'A history node cannot be the start.'; return; }
      parent.initial = name; update(doc); inspect(id); message.textContent = `Start state is now ${id}.`;
    } catch (error) { message.textContent = error.message; }
  }
  function inspect(id) {
    pickedPath = id || null; inspector.replaceChildren(); quickFields.replaceChildren();
    if (!pickedPath) {
      inspector.append(el('h3', 'Properties'), el('p', 'Select a node on the canvas to edit its behavior.', 'mw-note'), selectionActions);
      quickFields.append(el('p', 'Select a node to edit its description or confidence threshold.', 'mw-note'));
      setSelectionEnabled(false);
      return;
    }
    let doc;
    try { doc = draftDoc(); } catch (error) { pickedPath = null; message.textContent = error.message; return; }
    const parts = pickedPath.split('.');
    const state = nodeAt(doc, pickedPath);
    if (!state) {
      pickedPath = null;
      inspector.append(el('p', 'Select a node on the canvas to edit its behavior.', 'mw-note'), selectionActions);
      setSelectionEnabled(false); return;
    }
    setSelectionEnabled(true);
    const description = textInput('Node description', state.description || '');
    const floorValue = state.meta?.floor ?? doc.meta?.jev?.defaults?.floor ?? 0.5;
    const floor = textInput('Decision confidence threshold (0-1)', floorValue, { type: 'number', min: 0, max: 1 });
    function quickDraft() {
      const value = Number(floor.value);
      if (!Number.isFinite(value) || value < 0 || value > 1) return;
      state.description = description.value; state.meta = {...state.meta, floor: value};
      editor.value = JSON.stringify(doc, null, 2); dirty = true; draw();
    }
    description.oninput = quickDraft; floor.oninput = quickDraft;
    quickFields.append(el('b', pickedPath), field('Description', description), field('Confidence threshold (0-1)', floor),
      actionRow(
        button('Save', save, { icon: 'save', variant: 'primary', title: 'Save the graph (Ctrl+S)' }),
        button('Copy context for chat', () => navigator.clipboard.writeText(
          `Edit extensions/jev/graphs/${current}.json, selected state ${pickedPath}. Preserve unrelated nodes. The sidebar watches this saved graph.\n\nCurrent graph:\n${editor.value}`))));
    inspector.append(el('h3', pickedPath), selectionActions);
    const properties = el('div', '', 'mw-properties');
    const editDescription = textArea('Description', state.description || '', { rows: 3 });
    const editFloor = textInput('Confidence threshold', floorValue, { type: 'number', min: 0, max: 1 });
    properties.append(field('Description', editDescription), field('Confidence threshold (0-1)', editFloor, 'Used when the model picks among transitions.'));
    const action = state.entry?.find(a => a.type === 'tool');
    let toolName, toolInput;
    if (action) {
      toolName = textInput('Tool name', action.params.name);
      toolInput = textArea('Tool input JSON', JSON.stringify(action.params.input || {}, null, 2), { rows: 6, cls: 'mw-json' });
      properties.append(field('Tool', toolName), field('Input JSON', toolInput));
    }
    properties.append(actionRow(button('Apply properties', () => {
      try {
        const fresh = draftDoc(), target = nodeAt(fresh, pickedPath);
        const value = Number(editFloor.value);
        if (!Number.isFinite(value) || value < 0 || value > 1) throw new Error('Threshold must be between 0 and 1');
        target.description = editDescription.value; target.meta = {...target.meta, floor: value};
        if (action) {
          const entry = target.entry.find(a => a.type === 'tool');
          entry.params.name = toolName.value;
          entry.params.input = JSON.parse(toolInput.value);
        }
        update(fresh); inspect(pickedPath); message.textContent = 'Properties applied to the draft. Save to keep them.';
      } catch (error) { message.textContent = error.message; }
    }, { variant: 'primary', title: 'Apply these properties to the draft node' })));
    inspector.append(properties);
    const behavior = textArea('Selected node behavior JSON', JSON.stringify(state, null, 2), { rows: 12, cls: 'mw-json' });
    const advanced = el('details', '', 'mw-fold');
    advanced.append(el('summary', 'Advanced behavior JSON'), field('Selected node behavior JSON', behavior),
      actionRow(button('Apply behavior', () => {
        try {
          const fresh = draftDoc();
          const parts2 = pickedPath.split('.'); let parent = fresh;
          for (const part of parts2.slice(0, -1)) parent = parent.states[part];
          parent.states[parts2.at(-1)] = JSON.parse(behavior.value);
          update(fresh); inspect(pickedPath); message.textContent = 'Behavior replaced in the draft. Save to keep it.';
        } catch (error) { message.textContent = error.message; }
      }, { variant: 'primary', title: 'Replace this draft node with the JSON above' })));
    inspector.append(advanced);
    const transitionList = el('div', '', 'mw-transitions');
    for (const [key, value] of Object.entries({always: state.always, onDone: state.onDone, ...Object.fromEntries(Object.entries(state.on || {}).map(([event, rows]) => ['on:' + event, rows]))})) {
      transitionsOf(value).forEach((transition, index) => {
        const line = el('div', '', 'mw-transition');
        line.append(el('p', `${key} ${index + 1} → ${transition.target || '(internal)'}`),
          button('Disconnect', () => {
            try {
              const fresh = draftDoc(), target = nodeAt(fresh, pickedPath);
              const container = key.startsWith('on:') ? target.on : target;
              const fieldName = key.startsWith('on:') ? key.slice(3) : key;
              const entries = transitionsOf(container[fieldName]);
              entries.splice(index, 1);
              if (entries.length) container[fieldName] = entries; else delete container[fieldName];
              update(fresh); inspect(pickedPath);
            } catch (error) { message.textContent = error.message; }
          }, { icon: 'trash', variant: 'danger', title: 'Delete this transition' }));
        transitionList.append(line);
      });
    }
    if (transitionList.children.length) inspector.append(el('h3', 'Recorded transitions'), transitionList);
    const next = el('select'); next.setAttribute('aria-label', 'Next node');
    next.append(el('option', 'Choose next node'));
    for (const name of Object.keys(doc.states)) { const option = el('option', name); option.value = name; next.append(option); }
    inspector.append(el('h3', 'Connect'), field('Next node', next),
      actionRow(button('Add next transition', () => {
        try {
          if (!doc.states[next.value]) { message.textContent = 'Choose a next node first.'; return; }
          const target = nodeAt(doc, pickedPath);
          target.always = [...transitionsOf(target.always), { target: '#' + doc.id + '.' + next.value }];
          update(doc); inspect(pickedPath); message.textContent = 'Transition added in the draft. Save to keep it.';
        } catch (error) { message.textContent = error.message; }
      }, { icon: 'plus', title: 'Add a transition to the chosen node' })));
  }
  function deleteSelected() {
    if (!pickedPath) { message.textContent = 'Select a node first.'; return; }
    const removed = pickedPath;
    try {
      const doc = draftDoc();
      if (!removeState(doc, removed)) throw new Error('Selection changed. Select the node again.');
      pickedPath = null; update(doc); inspect(null); message.textContent = 'Node and attached transitions deleted from the draft. Start falls back to the first remaining state. Undo restores everything.';
    } catch (error) { message.textContent = error.message; }
  }
  function addNode(kind, position) {
    try {
      const doc = draftDoc();
      let n = 1; while (doc.states[kind + n]) n++;
      const id = kind + n;
      doc.states[id] = kind === 'final' ? {type: 'final'} : kind === 'decision' ?
        {description: 'Choose the next step', meta: {choose: {from: 'transitions'}}, on: {NEXT: {}}} :
        {description: kind === 'jev' ? 'Run a child graph' : 'Read a workspace file',
          entry: [{type: 'tool', params: {name: kind === 'jev' ? 'jev_run' : 'read', input: kind === 'jev' ? {graph: 'child-graph', input: {}} : {path: 'README.md'}, into: 'obs'}}]};
      if (!doc.initial) doc.initial = id;
      if (position) doc.states[id].meta = {...doc.states[id].meta, editor: position};
      update(doc); inspect(id);
      message.textContent = 'Node added with no transitions. Configure its behavior, then connect it from the previous node.';
    } catch (error) { message.textContent = error.message; }
  }
  palette.append(el('b', 'Add node'));
  for (const [kind, label] of [['tool', 'Tool call'], ['decision', 'Decision'], ['jev', 'Child Jev'], ['final', 'Final']]) {
    const item = button(label, () => addNode(kind), { icon: 'plus', title: `Add a ${label.toLowerCase()} node without connecting it` });
    item.draggable = true;
    item.ondragstart = event => event.dataTransfer.setData('application/x-minerva-node', kind);
    palette.append(item);
  }
  surface.ondragover = event => { if (event.dataTransfer.types.includes('application/x-minerva-node')) event.preventDefault(); };
  surface.ondrop = event => {
    event.preventDefault();
    const kind = event.dataTransfer.getData('application/x-minerva-node');
    if (!['tool', 'decision', 'jev', 'final'].includes(kind)) return;
    const rect = surface.getBoundingClientRect();
    const viewport = surface.querySelector('.svelte-flow__viewport');
    const matrix = new DOMMatrix(viewport ? getComputedStyle(viewport).transform : undefined);
    const point = new DOMPoint(event.clientX - rect.left, event.clientY - rect.top).matrixTransform(matrix.inverse());
    addNode(kind, {x: Math.round(point.x), y: Math.round(point.y)});
  };
  const ordersSection = el('section', '', 'mw-orders');
  ordersSection.setAttribute('aria-label', 'Standing order');
  ordersSection.append(el('h3', 'Standing order'));
  const orderText = textArea('Standing order', orderDraft, {rows: 2, maxLength: 2000, placeholder: 'Prioritize exposed services; keep this run read-only'});
  const approvalView = el('div');
  const orderStatus = el('p', '', 'mw-order-status'); orderStatus.setAttribute('role', 'status');
  const orderSend = button('Send order', sendOrder, { variant: 'primary', title: 'Send this standing order to the live run' });
  const orderClear = button('Clear', () => { orderText.value = ''; orderDraft = ''; syncOrderControls(); orderText.focus(); }, { disabled: true, title: 'Clear the typed text' });
  ordersSection.append(
    field('Standing order', orderText, 'Guides the next decision. Stop ends the run. Guidance only — it never changes graph permissions or thresholds.'),
    actionRow(orderSend, orderClear), orderStatus, approvalView);
  function liveRun() { return activeRun && isLiveRun(activeRun) ? activeRun : null; }
  function syncOrderControls() {
    const run = liveRun();
    orderSend.disabled = !run;
    orderClear.disabled = !orderText.value;
    orderText.disabled = !run && !orderDraft;
    orderSend.title = run
      ? (orderText.value ? 'Send this standing order to the live run' : 'Send an empty order to clear the standing order on the run')
      : 'Start a run before sending a standing order';
  }
  orderText.oninput = () => { orderDraft = orderText.value; syncOrderControls(); };
  async function sendOrder() {
    const run = liveRun();
    if (!run) { orderStatus.textContent = 'No live run: there is nothing to guide. Start a run first.'; return; }
    const text = orderText.value.slice(0, 2000);
    const body = JSON.stringify([run.run,text]);
    if(body !== orderRequestBody) { orderRequestId = null; orderRequestBody = body; }
    orderRequestId ||= crypto.randomUUID();
    const request = orderRequestId;
    orderSend.disabled = true; setButtonLabel(orderSend, 'Sending…');
    try {
      const result = await sendTool('jev_order', {run: run.run, text, id: request});
      if(result.job) controlJob={chat:result.chat_id,job:result.job};
      if (result.error) throw new Error(result.error);
      if (!ordersSection.isConnected) return;
      orderLocal = true;
      orderStatus.textContent = `${text ? 'Order' : 'Clear order'} ${request.slice(0, 8)} submitted for ${run.run}${result.job ? ` (job ${result.job})` : ''}. Not applied yet — the run records the status below once it picks it up. The text stays in the box so you can resend; Clear empties it.`;
      orderRequestId = null;
    } catch (error) {
      if (!ordersSection.isConnected) return;
      orderStatus.textContent = `${error.message} Request ${request.slice(0, 8)} kept; sending again reuses the same id.`;
    } finally {
      setButtonLabel(orderSend, 'Send order'); syncOrderControls();
    }
  }
  const decisionView = el('section', '', 'mw-decision-now');
  decisionView.hidden = true;
  function decisionBlock(decision) {
    const box = el('div', '', 'mw-decision-block');
    const head = el('div', '', 'mw-decision-head');
    head.append(el('b', decision.state || 'decision'), el('span', decision.chosen || 'waiting', 'mw-chosen'));
    box.append(head);
    for (const option of decision.options || []) {
      const chosen = option === decision.chosen || (option.label && option.label === decision.chosen);
      const row = el('div', '', chosen ? 'mw-score mw-score-chosen' : 'mw-score');
      row.append(el('span', option.label ?? String(option), 'mw-score-label'), el('span', scoreLabel(option.score), 'mw-score-value'));
      if (typeof option.score === 'number') {
        const bar = el('progress'); bar.max = 1; bar.value = option.score;
        bar.setAttribute('aria-label', `${option.label} model score`);
        row.append(bar);
      }
      box.append(row);
    }
    const scored = (decision.options || []).some(option => typeof option?.score === 'number');
    const detail = value => typeof value === 'number' ? scoreLabel(value) : 'not recorded';
    box.append(el('p', scored
      ? `Model scores · source ${decision.source ?? 'model'} · threshold ${detail(decision.floor)} · margin ${detail(decision.gap)} / required ${detail(decision.required_margin)}. No calibrated statistical interval is available.`
      : `Rule or forced decision — unscored. Source ${decision.source ?? 'rule'}. No model score or confidence interval applies.`, 'mw-note'));
    return box;
  }
  function statusChip(run) {
    const status = runStatusFor(run);
    return el('span', status.label, 'mw-status mw-status-' + status.key);
  }
  const runView = el('div', '', 'mw-runs');
  function draw() {
    try {
      const doc = draftDoc();
      if (!doc.states) throw new Error('Graph needs states');
      noteDraft(editor.value);
      const flow = graphToFlow(doc);
      flow.nodes.forEach(node => { node.data.compact = true; });
      if (canvas) canvas.setGraph(flow);
      else canvas = mountCanvas(surface, flow, {
        onmoved: positions => {
          if (!panel.classList.contains('mw-editing')) return;
          try {
            const fresh = draftDoc();
            for (const [id, position] of Object.entries(positions)) {
              const node = nodeAt(fresh, id); if (!node) continue;
              if (node.type === 'history') {
                fresh.meta ||= {}; fresh.meta.editor ||= {}; fresh.meta.editor.states ||= {};
                fresh.meta.editor.states[id] = position;
              } else node.meta = {...node.meta, editor: position};
            }
            update(fresh);
          } catch (error) { message.textContent = error.message; }
        },
        onconnected: edge => {
          if (!panel.classList.contains('mw-editing')) return;
          try {
            const fresh = draftDoc(), node = nodeAt(fresh, edge.source);
            if (!node) return;
            const target = '#' + fresh.id + '.' + edge.target;
            const handle = edge.sourceHandle || '';
            if (handle.startsWith('always-')) { const rows = transitionsOf(node.always); rows[Number(handle.slice(7))].target = target; node.always = rows; }
            else if (handle === 'onDone') { const rows = transitionsOf(node.onDone); rows[0].target = target; node.onDone = rows; }
            else if (handle.startsWith('on-')) {
              const match = handle.match(/^on-(.*)-(\d+)$/);
              const rows = transitionsOf(node.on[match[1]]); rows[Number(match[2])].target = target; node.on[match[1]] = rows;
            } else node.always = [...transitionsOf(node.always), { target }];
            update(fresh); inspect(edge.source);
            message.textContent = 'Transition recorded in the draft. Save to keep it.';
          } catch (error) { message.textContent = error.message; }
        },
        onpicked: id => { inspect(id); message.textContent = id ? `State: ${id}` : ''; },
      });
      canvas?.setRun(activeRun);
      current = doc.id;
      message.textContent = dirty ? 'Unsaved edits — Save to keep this graph' : 'Watching saved graph';
      syncHistoryButtons();
    } catch (error) { message.textContent = 'JSON: ' + error.message; }
  }
  function select(row) {
    history = []; future = []; lastDraft = '';
    baseline = row.json; editor.value = row.json; dirty = false; current = row.id;
    graphSelect.value = row.id; pickedPath = null;
    runSignature = ''; decisionSignature = ''; orderLocal = false;
    quickFields.replaceChildren(el('p', 'Select a node to edit its description or confidence threshold.', 'mw-note'));
    draw(); inspect(null); syncHistoryButtons();
  }
  graphSelect.onchange = () => {
    if (dirty && !confirm('Discard unsaved graph edits?')) { graphSelect.value = current; return; }
    const row = graphRows.find(r => r.id === graphSelect.value);
    if (row) select(row);
  };
  editor.oninput = () => { dirty = editor.value !== baseline; draw(); };
  async function save() {
    const savingId=current, savingGraph=editor.value, savingBaseline=baseline;
    try {
      const lint = await api('graphs/lint', {id: savingId, graph: savingGraph});
      if (!alive()) return;
      if (lint.errors?.length) throw new Error(lint.errors.join('\n'));
      if (lint.lint === 'unavailable' || lint.ok === false) throw new Error(lint.why || lint.error || 'Graph could not be checked');
      const result = await api('graphs/save', {id: savingId, graph: savingGraph, expected: savingBaseline});
      if (!alive() || current !== savingId) return;
      if (result.error) throw new Error(result.error);
      baseline = savingGraph; dirty = editor.value !== savingGraph;
      await syncGraphs();
      if (alive()) message.textContent = dirty ? 'Saved earlier draft; newer edits are unsaved.' : 'Saved';
    } catch (error) { if (alive()) message.textContent = error.message; }
  }
  const saveButton = button('Save', save, { icon: 'save', variant: 'primary', title: 'Save graph (Ctrl+S)' });
  const runButton = button('Run Jev', async () => {
    if (dirty) { message.textContent = 'Save the graph before running it.'; return; }
    if (!current) { message.textContent = 'Choose or save a graph before running it.'; return; }
    let input;
    try { input = JSON.parse(runInput.value || '{}'); }
    catch (error) { message.textContent = 'Run input is not valid JSON: ' + error.message; return; }
    try { await runTool('jev_run', {graph: current, input}); }
    catch (error) { message.textContent = error.message; }
  }, { icon: 'run', variant: 'primary', title: 'Run the saved graph with the JSON input below' });
  const undoButton = iconButton('undo', 'Undo (Ctrl+Z)', () => restore(history, future), {disabled: true});
  const redoButton = iconButton('redo', 'Redo (Ctrl+Shift+Z)', () => restore(future, history), {disabled: true});
  const upload = el('input'); upload.type = 'file'; upload.accept = '.json,application/json'; upload.hidden = true;
  upload.onchange = async () => {
    const file = upload.files?.[0]; if (!file) return;
    try {
      if (file.size > 1048576) throw new Error('Graph import is limited to 1 MiB');
      const text = await file.text(), doc = JSON.parse(text);
      if (!doc.id || !doc.states) throw new Error('Choose a Jev graph JSON file');
      if (dirty && !confirm('Discard unsaved edits?')) return;
      const row = graphRows.find(r => r.id === doc.id);
      baseline = row?.json || ''; editor.value = text; dirty = true; draw();
      message.textContent = `Imported ${doc.id} as a draft. Save to write it to ${row ? 'the existing graph' : 'a new graph'}.`;
    } catch (error) { message.textContent = error.message; }
    finally { upload.value = ''; }
  };
  const exportGraph = () => {
    try {
      const doc = draftDoc();
      const url = URL.createObjectURL(new Blob([JSON.stringify(doc, null, 2)], {type: 'application/json'}));
      const link = el('a'); link.href = url; link.download = (doc.id || 'graph') + '.json'; link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      message.textContent = 'Exported the current draft.';
    } catch (error) { message.textContent = error.message; }
  };
  const newGraph = () => {
    if (dirty && !confirm('Discard unsaved edits?')) return;
    const id = prompt('Graph name (lowercase letters, numbers and hyphens)');
    if (!id || !/^[a-z][a-z0-9-]*$/.test(id)) return;
    if (graphRows.some(r => r.id === id)) { message.textContent = 'That graph already exists.'; return; }
    baseline = ''; pickedPath = null;
    editor.value = JSON.stringify({id, version: '1', initial: 'start', context: {}, states: {start: {description: 'Connect this step to begin'}, done: {type: 'final'}}}, null, 2);
    dirty = true; draw(); inspect(null);
    message.textContent = 'New unsaved graph. Add nodes and Save to create it.';
  };
  const reloadGraph = async () => {
    if (dirty && !confirm('Discard unsaved graph edits?')) return;
    dirty = false; baseline = ''; await syncGraphs();
  };
  const deleteGraph = async () => {
    if (!baseline) { message.textContent = 'This graph has not been saved.'; return; }
    if (!confirm(`Archive ${current}? Existing runs are kept.`)) return;
    try {
      const result = await api('graphs/delete', {id: current, graph: baseline, expected: baseline});
      if (!alive()) return;
      if (result.error) throw new Error(result.error);
      baseline = ''; current = ''; dirty = false; editor.value = '';
      canvas?.destroy(); canvas = null;
      await syncGraphs();
      if (alive()) message.textContent = 'Graph archived';
    } catch (error) { if (alive()) message.textContent = error.message; }
  };
  const graphMenu = actionMenu('Graph actions', [
    menuItem('New graph', newGraph, {icon: 'plus'}),
    menuItem('Import graph file…', () => upload.click(), {icon: 'import'}),
    menuItem('Export draft as JSON', exportGraph, {icon: 'export'}),
    menuItem('Reload from disk', reloadGraph, {icon: 'refresh', title: 'Discard the draft and read the saved graph again'}),
    menuItem('Archive graph', deleteGraph, {icon: 'trash', variant: 'danger', title: 'Archive this graph. Existing runs are kept.'}),
  ]);
  toolbar.append(graphSelect);
  if (editing) toolbar.append(saveButton, runButton, undoButton, redoButton, graphMenu, upload);
  const layout = el('div', '', 'mw-editor-layout');
  const center = el('div', '', 'mw-editor-center');
  const rail = el('div', '', 'mw-inspector-rail');
  const hint = el('p', 'Drag nodes to move · Drag from a handle to connect · Delete removes the selected node · Ctrl+Z / Ctrl+Shift+Z undo · Ctrl+S saves', 'mw-hint mw-editor-only');
  if (editing) {
    center.append(surface, quick);
    rail.append(ordersSection, decisionView, inspector, jsonFold, runFold);
    layout.append(center, rail);
    content.append(toolbar, palette, liveStatus, layout, hint, runView);
  } else {
    content.append(toolbar, liveStatus, surface, ordersSection, decisionView, runView);
  }
  const shortcuts = event => {
    if (panel.hidden) return;
    const typing = event.target.closest('input,textarea,select,[contenteditable=true]');
    if (editing && !typing && ['Delete', 'Backspace'].includes(event.key)) { event.preventDefault(); deleteSelected(); return; }
    if (!surface.isConnected) return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') { event.preventDefault(); save(); return; }
    if ((event.ctrlKey || event.metaKey) && !typing && event.key.toLowerCase() === 'z') {
      event.preventDefault();
      event.shiftKey ? restore(future, history) : restore(history, future);
    }
  };
  panel.onkeydown = shortcuts;
  async function syncGraphs() {
    const store = await api('graphs');
    if (epoch !== generation || !surface.isConnected) return;
    if (store.wired === false) throw new Error(store.why || 'Graph service unavailable');
    graphRows = (store.graphs || []).filter(row => row.json);
    const names = graphRows.map(row => row.id).join('|');
    if (graphSelect.dataset.names !== names) {
      graphSelect.replaceChildren(...graphRows.map(row => { const option = el('option', row.id); option.value = row.id; return option; }));
      graphSelect.dataset.names = names;
    }
    const row = graphRows.find(r => r.id === current) || (!dirty ? graphRows[0] : null);
    if (row && !dirty && (!canvas || row.json !== baseline)) select(row);
    else if (row && dirty && row.json !== baseline) message.textContent = 'Graph changed on disk. Your draft is preserved; reload to use the chat edit.';
    if (!dirty && !graphRows.length) {
      graphSelect.replaceChildren(el('option', 'No graphs yet'));
      message.textContent = 'No Jev graphs found. Use Graph actions → New graph to create one.';
    }
    graphSelect.value = current;
  }
  refresh = async () => {
    await syncGraphs();
    if (epoch !== generation || !surface.isConnected) return;
    const data = await api('runs');
    if (epoch !== generation || !surface.isConnected) return;
    if (data.service === 'down' || data.wired === false) { liveStatus.replaceChildren(el('span', data.why || 'Jev service unavailable', 'mw-status mw-status-failed')); return; }
    const rows = (data.runs || []).map(row => ({...row, run: row.id || row.run})).filter(row => String(row.graph).split('@')[0] === current);
    activeRun = rows.find(row => row.run === activeRun?.run) || rows[0];
    canvas?.setRun(activeRun);
    const status = runStatusFor(activeRun);
    liveStatus.replaceChildren(statusChip(activeRun), el('span', activeRun ? `${activeRun.state || ''} · step ${activeRun.step ?? '—'}` : 'No runs recorded for this graph yet.', 'mw-live-detail'));
    const last = activeRun?.decisions?.at(-1);
    const decisionKey = last ? JSON.stringify([last.state, last.chosen, (last.options || []).map(o => o.label + scoreLabel(o.score)), last.source]) : '';
    if (decisionKey !== decisionSignature) {
      decisionSignature = decisionKey;
      decisionView.hidden = !last;
      if (last) decisionView.replaceChildren(el('h3', 'Latest decision for ' + (activeRun.run || '').slice(0, 8)), decisionBlock(last));
    }
    const order = latestOrder(activeRun);
    if (order) { orderLocal = false; orderStatus.textContent = `Standing order ${String(order.id || '').slice(0, 8)}: ${orderStatusLabel(order)}${order.text ? ' · ' + order.text : ' (empty order clears the standing order)'}`; }
    else if (!orderLocal) orderStatus.textContent = isLiveRun(activeRun) ? 'No standing order recorded for this run yet.' : 'No live run. Orders guide the next decision of a run in progress; Stop ends the run.';
    syncOrderControls();
    if(controlJob) {
      const job=controlJob, activity=await api('activity?chat='+encodeURIComponent(job.chat));
      if(!alive() || controlJob!==job) return;
      approvalView.replaceChildren();
      if(activity.pending_question) {
        const question=activity.pending_question;
        const answer=async approved=>{try {await api('answer',{chat:job.chat,question,approved});approvalView.replaceChildren();}catch(error){orderStatus.textContent=error.message;}};
        approvalView.append(el('h4','Approval required'),el('pre',question),actionRow(button('Approve',()=>answer(true)),button('Decline',()=>answer(false))));
      } else if(!activity.running) {
        const result=(activity.activity||[]).find(row=>row.kind==='tool_result' && row.call_id===job.job);
        if(result?.is_error) orderStatus.textContent='Action failed: '+result.output;
        controlJob=null;
      }
    }
    const signature = JSON.stringify(rows);
    if (signature !== runSignature) {
      runSignature = signature;
      runView.replaceChildren(el('h3', 'Recorded runs'));
      if (!rows.length) runView.append(emptyState('No runs recorded for this graph yet. Save the graph, then use Run Jev to start one.'));
      for (const run of rows) {
        const card = el('details', '', 'mw-run');
        card.open = openRuns.has(run.run) || !!run.escalation;
        card.ontoggle = () => card.open ? openRuns.add(run.run) : openRuns.delete(run.run);
        const summary = el('summary');
        summary.append(el('span', run.run, 'mw-run-id'), statusChip(run));
        card.append(summary);
        const stop = isLiveRun(run) ? button('Stop', () => ask(`Stop Jev run ${run.run}? The run ends and stays recorded.`, () => runTool('jev_stop', {run: run.run, reason: 'Stopped from workspace panel'})), { icon: 'trash', variant: 'danger', title: 'End this run' }) : null;
        card.append(actionRow(button('Watch', () => {
          activeRun = run; canvas?.setRun(run); liveStatus.replaceChildren(statusChip(run));
        }, { title: 'Show this run on the canvas and in the decision panel' }), stop));
        const evidence = run.evidence || {};
        const trace = evidence.trace || [];
        if (trace.length) {
          const timeline = el('div', '', 'mw-timeline');
          timeline.append(el('h4', 'Execution history'), el('p', 'Recorded events, in execution order. Expand a step to inspect its inputs and observations.', 'mw-note'));
          if (trace[0].sequence > 1) timeline.append(el('p', 'Earlier events are outside the retained 500-event window.', 'mw-note'));
          for (const entry of trace) {
            const step = el('details', '', 'mw-fold');
            const label = entry.kind === 'transition' ? `${entry.source} → ${entry.target}` : `${entry.state || 'run'} · ${entry.tool || entry.kind.replaceAll('_', ' ')}`;
            step.append(el('summary', `${entry.sequence}. ${label} · ${entry.elapsed_s}s`), el('pre', JSON.stringify(entry, null, 2)));
            timeline.append(step);
          }
          card.append(timeline);
        } else card.append(el('p', 'This backend did not provide an execution history for this run. A final state alone cannot explain how it got there.', 'mw-note'));
        if (evidence.counts) card.append(el('p', `${evidence.counts.actions || 0} tool actions · ${evidence.counts.chooser_calls || 0} model decisions`, 'mw-note'));
        for (const [label, value] of [['Output', evidence.output], ['Error', evidence.error], ['Reason', evidence.reason], ['Budget limit', evidence.exhausted]]) {
          if (value != null) { const detail = el('details', '', 'mw-fold'); detail.append(el('summary', label), el('pre', typeof value === 'string' ? value : JSON.stringify(value, null, 2))); card.append(detail); }
        }
        for (const decision of run.decisions || []) card.append(decisionBlock(decision));
        const orders = Array.isArray(run.orders) ? run.orders : [];
        if (orders.length) {
          const box = el('div', '', 'mw-order-list');
          box.append(el('h4', 'Standing orders recorded by the run'));
          for (const entry of orders) box.append(el('p', `${String(entry.id || '').slice(0, 8)}: ${orderStatusLabel(entry)} · ${entry.text || '(empty)'}`));
          box.append(el('p', 'Pending means the backend acknowledged the order; it is not applied until the run records it as applied.', 'mw-note'));
          card.append(box);
        }
        const hops = Array.isArray(run.transitions) ? run.transitions : [];
        if (hops.length) {
          const box = el('div', '', 'mw-hop-list');
          box.append(el('h4', 'Transitions recorded by the run'));
          for (const hop of hops) box.append(el('p', typeof hop === 'string' ? hop : `${hop.source ?? hop.from ?? '?'} → ${hop.to ?? hop.target ?? '?'}`));
          card.append(box);
        }
        if (isLiveRun(run) && !run.escalation) card.append(el('p', 'Running. Only transitions the run actually recorded are listed above.', 'mw-note'));
        if (run.escalation) {
          const box = el('div', '', 'mw-escalation');
          box.append(el('h4', 'Waiting for human'), el('p', run.escalation.question || run.escalation.why || 'Decision required'),
            el('p', 'This run stays parked until you answer here or in chat.', 'mw-note'));
          for (const option of run.escalation.options || []) box.append(button(option.label, () => ask(`Answer this parked decision with "${option.label}"?`, () => runTool('jev_resume', {run: run.run, request: run.escalation.request, pick: option.index, by: 'operator'})), { title: `Resume the parked run with ${option.label}` }));
          box.append(button('Ask in chat', () => {
            message.textContent = `Ask your chat model to inspect Jev run ${run.run}, decision ${run.escalation.request || run.escalation.step}, and resume only that decision. The run remains parked until answered.`;
          }));
          card.append(box);
        }
        runView.append(card);
      }
    }
    syncOrderControls();
  };
  await syncGraphs();
  if (payload?.text) { editor.value = payload.text; dirty = true; draw(); jsonFold.open = true; }
  syncOrderControls();
  if (!editing && !graphRows.length) content.append(emptyState('No Jev graphs yet. Expand to the full editor and use Graph actions → New graph.'));
}
async function runTool(tool, input) {
  const result = await sendTool(tool, input);
  if(result.job) controlJob={chat:result.chat_id,job:result.job};
  message.textContent = result.error || (result.job ? `Started ${result.job}. Follow Activity for tool calls and approvals.` : JSON.stringify(result));
  return result;
}
function markdown(text) {
  const root = el('div', '', 'mw-markdown'); let code = null;
  for (const line of text.split('\n')) {
    if (line.startsWith('```')) { if (code) code = null; else { code = el('code'); const pre = el('pre'); pre.append(code); root.append(pre); } continue; }
    if (code) { code.append(document.createTextNode(line + '\n')); continue; }
    const heading = line.match(/^(#{1,6})\s+(.*)/);
    root.append(heading ? el('h' + heading[1].length, heading[2]) : el('p', line || ' '));
  }
  return root;
}
async function files(path, payload) {
  const nav = el('div', '', 'mw-toolbar');
  const location = textInput('Workspace-relative file or folder', path, {placeholder: 'Workspace-relative file or folder'});
  const show = async () => { try { await render(location.value); } catch (error) { message.textContent = error.message; } };
  nav.append(field('Path', location), button('Open', show, {variant: 'primary'}), button('Root', () => render('')));
  content.append(nav);
  const view = el('div'); content.append(view);
  let viewedPath = null, textBefore = null;
  async function render(name) {
    const data = await api('files?path=' + encodeURIComponent(name));
    if (!view.isConnected) return;
    viewedPath = name; location.value = name;
    if (data.entries) {
      textBefore = null; view.replaceChildren();
      if (name) view.append(button('Parent folder', () => render(name.split('/').slice(0, -1).join('/'))));
      if (!data.entries.length) view.append(emptyState('This folder is empty.'));
      for (const entry of data.entries) view.append(button((entry.directory ? 'Folder: ' : '') + entry.name, () => render(entry.path)));
    } else if (data.text !== textBefore) {
      textBefore = data.text;
      if (data.markdown && data.html) { const rendered = el('div', '', 'mw-markdown'); rendered.innerHTML = data.html; view.replaceChildren(rendered); }
      else view.replaceChildren(data.markdown ? markdown(data.text) : el('pre', data.text));
    }
  }
  if (payload?.text) view.replaceChildren(payload.markdown ? markdown(payload.text) : el('pre', payload.text));
  else await render(path);
  refresh = async () => { if (viewedPath !== null && textBefore !== null) await render(viewedPath); };
}
async function activity() {
  const list = el('div');
  content.append(el('p', 'Recorded tool calls, results and decision events for this chat.', 'mw-note'), list);
  let signature = '';
  refresh = async () => {
    const chat = chatId(); if (!chat) throw new Error('Open a saved chat to inspect its agent activity.');
    const data = await api('activity?chat=' + encodeURIComponent(chat));
    if (!list.isConnected) return;
    const next = JSON.stringify(data); if (signature === next) return; signature = next;
    list.replaceChildren();
    if (data.pending_question) list.append(el('h3', 'Approval required — answer in chat'), el('pre', data.pending_question));
    const rows = data.activity || data.events || data.records || [];
    for (const row of rows) {
      const entry = el('details', '', 'mw-fold');
      entry.append(el('summary', row.tool || row.kind || row.type || 'Event'), el('pre', JSON.stringify(row, null, 2)));
      list.append(entry);
    }
    if (!rows.length) list.append(emptyState(data.error || 'No recorded activity yet.'));
  };
}
window.addEventListener('minerva-panel', event => open(event.detail?.tab || 'graph', event.detail));
document.addEventListener('keydown', event => { if (event.key === 'Escape' && !panel.hidden && !dirty) close(); });
function codePreviews() {
  for (const pre of document.querySelectorAll('#chat-container pre:not([data-minerva-preview])')) {
    pre.dataset.minervaPreview = 'yes';
    pre.after(button('Open in side panel', () => {
      const text = (pre.querySelector('code') || pre).textContent;
      let isGraph = false; try { isGraph = !!JSON.parse(text).states; } catch {}
      open(isGraph ? 'graph' : 'files', {text});
    }));
  }
}
let previewQueued = false;
function workspaceLabels() {
  const label = document.querySelector('#main-content a[href="/workspace/models"] span');
  if (label && label.textContent !== 'Model presets') label.textContent = 'Model presets';
  if (location.pathname === '/workspace/models') {
    const search = document.querySelector('#workspace-container input[placeholder="Search Models"]');
    if (search) { search.placeholder = 'Search model presets'; search.setAttribute('aria-label', 'Search model presets'); }
  }
}
new MutationObserver(() => { if (previewQueued) return; previewQueued = true; requestAnimationFrame(() => { previewQueued = false; codePreviews(); workspaceLabels(); placeLauncher(); fitPanel(); }); }).observe(document.body, {childList: true, subtree: true});
codePreviews();
workspaceLabels();
placeLauncher();

async function extensions() {
  const list = el('div'); content.append(el('h2', 'Extensions'), list);
  let signature = '', busy = false;
  refresh = async () => {
    if (busy) return;
    const data = await api('extensions');
    if (!list.isConnected) return;
    const next = JSON.stringify(data); if (signature === next) return; signature = next;
    list.replaceChildren();
    for (const row of data.extensions || []) {
      const section = el('section');
      section.append(el('h3', row.name || row.dir), el('p', row.description),
        el('p', `${row.enabled ? 'Enabled' : 'Disabled'} · ${row.service?.says || 'No background service'}`));
      const controls = el('div', '', 'mw-actions');
      const names = [row.enabled ? 'disable' : 'enable'];
      if (row.enabled && row.service) names.push('start', 'stop', 'restart');
      for (const action of names) controls.append(button(action, async () => {
        busy = true; for (const control of controls.children) control.disabled = true;
        try {
          const result = await api('extensions/' + action, {name: row.name || row.dir});
          message.textContent = result.error || result.extension?.error || 'Extension updated';
        } catch (error) { message.textContent = error.message; }
        finally { busy = false; signature = ''; for (const control of controls.children) control.disabled = false; }
        try { await refresh(); } catch (error) { message.textContent = error.message; }
      }, { variant: ['stop', 'disable'].includes(action) ? 'danger' : null, title: `${action} ${row.name || row.dir}` }));
      section.append(controls);
      for (const error of [row.error, row.load_error]) if (error) section.append(el('p', error, 'mw-note'));
      const tools = el('details', '', 'mw-fold');
      tools.append(el('summary', `${row.tools?.length || 0} tools`));
      for (const tool of row.tools || []) tools.append(el('h4', tool.name), el('p', tool.description), el('p', `Approval: ${tool.approval}`));
      section.append(tools);
      if (row.service?.log_tail) section.append(el('pre', row.service.log_tail));
      list.append(section);
    }
    if (!data.extensions?.length) list.append(emptyState('No extensions available.'));
  };
}
function openLocation() {
  const target = location.hash.slice(1);
  if (target === 'minerva-graph' || target === 'minerva-extensions') open(target.slice(8), {full: true});
  else if (!panel.hidden && !dirty) close();
}
window.addEventListener('hashchange', openLocation);
openLocation();
