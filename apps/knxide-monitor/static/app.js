'use strict';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

/** Build an element. Text goes in as text nodes, never as HTML. */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === null || v === undefined || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, '');
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

function svg(path, cls = '') {
  const el = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  el.setAttribute('viewBox', '0 0 24 24');
  if (cls) el.setAttribute('class', cls);
  el.innerHTML = path; // static markup from this file only
  return el;
}

const ICONS = {
  chevron: '<path d="m9 6 6 6-6 6"/>',
  ok: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
  err: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.8v5M12 16.2v.1"/>',
  warn: '<path d="M12 3.8 21 19.5H3z"/><path d="M12 10v4.2M12 16.9v.1"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.2M12 7.9v.1"/>',
  check: '<circle cx="12" cy="12" r="8.5"/><path d="m8.2 12.3 2.6 2.6 5-5.4"/>',
  pending: '<path d="M12 3.5a8.5 8.5 0 1 1-8.5 8.5"/>',
};

const pad = (n, w = 2) => String(n).padStart(w, '0');
function clock(ms, withMs) {
  const d = new Date(ms);
  const base = `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  return withMs ? [base, `.${pad(d.getMilliseconds(), 3)}`] : base;
}

function store(key, value) {
  try {
    if (value === undefined) return localStorage.getItem(key);
    localStorage.setItem(key, value);
  } catch { /* private mode etc. */ }
  return null;
}

function debounce(fn, ms) {
  let timer;
  return (...args) => { clearTimeout(timer); timer = setTimeout(() => fn(...args), ms); };
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

const MAX_ROWS = 1000;
const SERVICES = ['write', 'read', 'response', 'other'];

const S = {
  conn: { status: 'disconnected' },
  project: { file: null, entries: [] },
  byAddr: new Map(),
  values: new Map(), // group address -> last write/response telegram
  transport: 'udp',
  paused: false,
  pausedQueue: [],
  filter: '',
  services: new Set(SERVICES),
  total: 0,
  selected: null,
  dptAuto: true,
  controlKind: null,
  devices: new Map(), // physical address -> { address, frames, first_seen, last_seen, groups: Map }
  devEls: new Map(),
  openDevices: new Set(),
  issues: [],
  confirming: null,
  nextId: 1,
  pending: new Map(), // request id -> activity element
  rate: new Array(60).fill(0),
  rateNow: 0,
};

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  drawSpark();
}
applyTheme(store('knxide-theme') || (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'));
$('#theme-btn').addEventListener('click', () => {
  const next = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light';
  store('knxide-theme', next);
  applyTheme(next);
});

// ---------------------------------------------------------------------------
// Server link
// ---------------------------------------------------------------------------

let socket = null;
let retryMs = 500;
let everOpened = false;

function openSocket() {
  const scheme = location.protocol === 'https:' ? 'wss://' : 'ws://';
  socket = new WebSocket(scheme + location.host + '/ws');
  socket.addEventListener('open', () => {
    everOpened = true;
    retryMs = 500;
    $('#link-banner').hidden = true;
  });
  socket.addEventListener('message', (e) => {
    let msg;
    try { msg = JSON.parse(e.data); } catch { return; }
    onMessage(msg);
  });
  socket.addEventListener('close', () => {
    if (everOpened) $('#link-banner').hidden = false;
    setTimeout(openSocket, retryMs);
    retryMs = Math.min(retryMs * 2, 5000);
  });
}

function sendMsg(msg) {
  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(msg));
    return true;
  }
  toast('Not connected to the knxide-monitor server', 'err');
  return false;
}

function onMessage(msg) {
  switch (msg.type) {
    case 'hello':
      setProject(msg.project);
      setConn(msg.state);
      resetLog();
      setDevices(msg.devices || []);
      setIssues(msg.issues || []);
      // History is already counted in the device snapshot.
      for (const t of msg.history) addTelegram(t, false);
      break;
    case 'state':
      setConn(msg.state);
      break;
    case 'telegram':
      addTelegram(msg.telegram, true);
      break;
    case 'project':
      setProject(msg.project);
      break;
    case 'health':
      setIssues(msg.issues);
      break;
    case 'result':
      settle(msg.id, msg.ok, msg.message);
      break;
  }
}

// ---------------------------------------------------------------------------
// Connection bar
// ---------------------------------------------------------------------------

const STATUS_LABEL = {
  disconnected: 'Disconnected',
  connecting: 'Connecting…',
  connected: 'Connected',
  reconnecting: 'Reconnecting…',
};

function setConn(state) {
  const previous = S.conn.status;
  S.conn = state;
  const el = $('#status');
  el.dataset.status = state.status;
  el.classList.toggle('error', Boolean(state.error) && state.status === 'disconnected');
  $('.status-label', el).textContent = STATUS_LABEL[state.status] || state.status;
  let detail = '';
  if (state.status === 'connected') detail = `${state.address ?? ''} via ${state.gateway ?? ''}`.trim();
  else if (state.error) detail = state.error;
  else if (state.status === 'connecting') detail = state.gateway ?? '';
  $('.status-detail', el).textContent = detail;
  el.title = state.error || detail;

  const gw = $('#gateway');
  if (!gw.value && state.gateway) gw.value = state.gateway;
  setTransport(state.transport || S.transport);

  const btn = $('#connect-btn');
  const active = state.status !== 'disconnected';
  btn.textContent = active ? 'Disconnect' : 'Connect';
  btn.classList.toggle('primary', !active);
  btn.classList.toggle('danger', active);
  updateSendEnabled();
  updateEmpty();
  if (previous !== state.status) renderDevices();

  if (previous !== 'disconnected' && state.status === 'disconnected' && state.error) {
    toast(`Connection failed: ${state.error}`, 'err');
  }
}

function setTransport(t) {
  S.transport = t;
  for (const b of $$('.seg-btn')) {
    const on = b.dataset.transport === t;
    b.classList.toggle('on', on);
    b.setAttribute('aria-checked', String(on));
  }
}

$('#gateway').value = store('knxide-gateway') || '';
for (const b of $$('.seg-btn')) b.addEventListener('click', () => setTransport(b.dataset.transport));
$('#connect-form').addEventListener('submit', (e) => {
  e.preventDefault();
  if (S.conn.status !== 'disconnected') {
    sendMsg({ type: 'disconnect' });
    return;
  }
  const gateway = $('#gateway').value.trim();
  if (!gateway) {
    $('#gateway').focus();
    toast('Enter the IP address of your KNX IP interface', 'err');
    return;
  }
  store('knxide-gateway', gateway);
  sendMsg({ type: 'connect', gateway, transport: S.transport });
});

// ---------------------------------------------------------------------------
// Project and group address tree
// ---------------------------------------------------------------------------

function setProject(project) {
  S.project = project;
  S.byAddr = new Map(project.entries.map((e) => [e.address, e]));
  $('#ga-count').textContent = project.entries.length;
  $('#project-file').textContent = project.file ? `${project.file} · drop another export to replace it` : '';
  const options = $('#ga-options');
  options.replaceChildren(...project.entries.map((e) => h('option', { value: e.address }, e.name)));
  renderTree();
  onAddressChange();
  renderDevices();
}

function matchesQuery(entry, q) {
  if (!q) return true;
  return (
    entry.address.includes(q) ||
    entry.name.toLowerCase().includes(q) ||
    (entry.dpt || '').includes(q) ||
    entry.path.some((p) => p.toLowerCase().includes(q))
  );
}

function renderTree() {
  const tree = $('#ga-tree');
  const q = $('#ga-search').value.trim().toLowerCase();
  const entries = S.project.entries.filter((e) => matchesQuery(e, q));

  if (S.project.entries.length === 0) {
    tree.replaceChildren(h('div', { class: 'tree-empty' },
      h('strong', {}, 'No project loaded'),
      'Drop an ETS group address export (XML or CSV) anywhere on the page, or start the server with --project.'));
    return;
  }
  if (entries.length === 0) {
    tree.replaceChildren(h('div', { class: 'tree-empty' }, 'Nothing matches.'));
    return;
  }

  // Group by main range, then middle range.
  const groups = new Map();
  for (const e of entries) {
    const main = e.path[0] ?? 'Ungrouped';
    const middle = e.path[1] ?? '';
    if (!groups.has(main)) groups.set(main, new Map());
    const mids = groups.get(main);
    if (!mids.has(middle)) mids.set(middle, []);
    mids.get(middle).push(e);
  }
  const openAll = Boolean(q);
  const opened = new Set(JSON.parse(store('knxide-open') || '[]'));
  const remember = (key) => (ev) => {
    if (openAll) return;
    if (ev.target.open) opened.add(key); else opened.delete(key);
    store('knxide-open', JSON.stringify([...opened]));
  };

  const frag = document.createDocumentFragment();
  for (const [main, mids] of groups) {
    const count = [...mids.values()].reduce((n, list) => n + list.length, 0);
    const body = h('div', { class: 'range-body' });
    for (const [middle, list] of mids) {
      const items = list.map(gaButton);
      if (!middle) { body.append(...items); continue; }
      const key = `${main}/${middle}`;
      body.append(h('details', { class: 'range', open: openAll || opened.has(key), ontoggle: remember(key) },
        h('summary', {}, svg(ICONS.chevron, 'chev'), h('span', {}, middle), h('span', { class: 'range-count' }, list.length)),
        h('div', { class: 'range-body' }, items)));
    }
    frag.append(h('details', { class: 'range', open: openAll || opened.has(main), ontoggle: remember(main) },
      h('summary', {}, svg(ICONS.chevron, 'chev'), h('span', {}, main), h('span', { class: 'range-count' }, count)),
      body));
  }
  const top = tree.scrollTop;
  tree.replaceChildren(frag);
  tree.scrollTop = top;
  markGroups();
}

function gaButton(e) {
  const last = S.values.get(e.address);
  return h('button', {
    class: 'ga' + (S.selected === e.address ? ' selected' : ''),
    type: 'button',
    title: [e.name, e.description].filter(Boolean).join('\n'),
    dataset: { ga: e.address },
    onclick: () => selectAddress(e.address, true),
  },
  h('span', { class: 'ga-name' }, e.name || e.address),
  h('span', { class: 'ga-meta' }, h('span', { class: 'ga-addr' }, e.address),
    e.dpt ? h('span', {
      class: 'dpt-chip' + (e.overridden ? ' overridden' : ''),
      title: e.overridden ? `Overridden here; ETS says ${e.project_dpt ?? 'no DPT'}` : null,
    }, e.dpt) : null),
  h('span', { class: 'ga-value' }, last ? shownValue(last) : ''));
}

function shownValue(t) {
  return t.value ?? t.raw ?? '';
}

$('#ga-search').addEventListener('input', debounce(renderTree, 80));

function updateTreeValue(t) {
  const btn = $(`.ga[data-ga="${CSS.escape(t.destination)}"]`);
  if (!btn) return;
  const v = $('.ga-value', btn);
  v.textContent = shownValue(t);
  v.classList.remove('flash');
  void v.offsetWidth; // restart the animation
  v.classList.add('flash');
}

// ---------------------------------------------------------------------------
// Monitor
// ---------------------------------------------------------------------------

function resetLog() {
  $('#log').replaceChildren();
  S.values.clear();
  S.pausedQueue = [];
  S.total = 0;
  $('#tg-count').textContent = '0';
  updatePausedBadge();
  updateEmpty();
}

function addTelegram(t, live) {
  S.total += 1;
  $('#tg-count').textContent = S.total.toLocaleString();
  if (live) {
    S.rate[S.rate.length - 1] += 1;
    noteDevice(t);
  }
  if (t.group && (t.service === 'write' || t.service === 'response')) {
    S.values.set(t.destination, t);
    updateTreeValue(t);
    if (t.destination === S.selected) {
      renderSendInfo();
      // Buttons that show the current state follow it; inputs are left alone while typing.
      if (S.controlKind === 'bool' || S.controlKind === 'hvac_mode') renderControl();
    }
  }
  if (S.paused) {
    S.pausedQueue.push(t);
    if (S.pausedQueue.length > MAX_ROWS) S.pausedQueue.shift();
    updatePausedBadge();
    return;
  }
  renderRow(t, live);
}

function renderRow(t, animate) {
  const [hms, ms] = clock(t.time, true);
  const search = [t.source, t.destination, t.name, t.value, t.raw, t.dpt].filter(Boolean).join(' ').toLowerCase();
  const value = t.warning
    ? h('span', { class: 'val warn', title: t.warning }, '⚠ ', t.value ?? t.raw ?? '')
    : t.value
      ? h('span', { class: 'val' }, t.value)
      : h('span', { class: 'val none' }, t.service === 'read' ? '—' : (t.raw ?? '—'));
  const row = h('div', {
    class: 'row' + (animate ? ' new' : '') + (t.own ? ' own' : ''),
    role: 'row',
    title: [t.name, t.dpt && `DPT ${t.dpt}`, t.own && 'sent from this console'].filter(Boolean).join(' · '),
    dataset: { service: t.service, search, ga: t.group ? t.destination : '' },
    onclick: () => { if (t.group) selectAddress(t.destination, true, true); },
  },
  h('span', { class: 't' }, hms, h('b', {}, ms)),
  h('span', { class: 'src' }, t.source),
  h('span', { class: 'dst' }, h('span', { class: 'addr' }, t.destination), t.name ? h('span', { class: 'name' }, t.name) : null),
  h('span', { class: `svc ${t.service}` }, t.service),
  value,
  h('span', { class: 'raw' }, t.value ? (t.raw ?? '') : ''));
  row.hidden = !rowMatches(row);
  const log = $('#log');
  log.prepend(row);
  while (log.childElementCount > MAX_ROWS) log.lastElementChild.remove();
  updateEmpty();
}

function rowMatches(row) {
  return S.services.has(row.dataset.service) && (!S.filter || row.dataset.search.includes(S.filter));
}

function applyFilter() {
  S.filter = $('#filter').value.trim().toLowerCase();
  for (const row of $('#log').children) row.hidden = !rowMatches(row);
  updateEmpty();
}

function updateEmpty() {
  const log = $('#log');
  const anyVisible = Array.prototype.some.call(log.children, (r) => !r.hidden);
  const empty = $('#log-empty');
  empty.hidden = anyVisible;
  log.hidden = !anyVisible;
  const [title, text] =
    log.childElementCount > 0 ? ['Nothing matches', 'No telegram passes the current filter.']
    : S.conn.status === 'connected' ? ['Listening…', 'Telegrams appear here as soon as something happens on the bus.']
    : ['Waiting for the bus', 'Connect to a gateway and telegrams show up here as they happen.'];
  $('.empty-title', empty).textContent = title;
  $('.empty-text', empty).textContent = text;
}

$('#filter').addEventListener('input', debounce(applyFilter, 60));
$('#filter').addEventListener('keydown', (e) => {
  if (e.key === 'Escape') { e.target.value = ''; applyFilter(); e.target.blur(); }
});
for (const chip of $$('.chip')) {
  chip.addEventListener('click', () => {
    const s = chip.dataset.service;
    if (S.services.has(s)) S.services.delete(s); else S.services.add(s);
    chip.classList.toggle('on', S.services.has(s));
    applyFilter();
  });
}

function updatePausedBadge() {
  const badge = $('#paused-count');
  badge.hidden = !S.paused || S.pausedQueue.length === 0;
  badge.textContent = S.pausedQueue.length;
}

$('#pause-btn').addEventListener('click', () => {
  S.paused = !S.paused;
  const btn = $('#pause-btn');
  btn.classList.toggle('paused', S.paused);
  $('span:not(.badge)', btn).textContent = S.paused ? 'Resume' : 'Pause';
  if (!S.paused) {
    for (const t of S.pausedQueue) renderRow(t, false);
    S.pausedQueue = [];
  }
  updatePausedBadge();
});

$('#clear-btn').addEventListener('click', () => {
  $('#log').replaceChildren();
  S.pausedQueue = [];
  updatePausedBadge();
  updateEmpty();
});

document.addEventListener('keydown', (e) => {
  const typing = ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement?.tagName);
  if (e.key === '/' && !typing) { e.preventDefault(); $('#filter').focus(); }
});

// ---------------------------------------------------------------------------
// Bus load sparkline
// ---------------------------------------------------------------------------

function drawSpark() {
  const canvas = $('#spark');
  if (!canvas) return;
  const dpr = window.devicePixelRatio || 1;
  const w = 160, hgt = 34;
  if (canvas.width !== w * dpr) { canvas.width = w * dpr; canvas.height = hgt * dpr; }
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, hgt);
  const css = getComputedStyle(document.documentElement);
  const accent = css.getPropertyValue('--accent').trim() || '#34d399';
  const faint = css.getPropertyValue('--border').trim() || '#222';
  const data = S.rate.slice(0, -1); // completed seconds only
  const max = Math.max(4, ...data);
  const step = w / (data.length - 1);
  const y = (v) => hgt - 3 - (v / max) * (hgt - 8);

  ctx.strokeStyle = faint;
  ctx.lineWidth = 1;
  ctx.beginPath(); ctx.moveTo(0, hgt - 2.5); ctx.lineTo(w, hgt - 2.5); ctx.stroke();

  const grad = ctx.createLinearGradient(0, 0, 0, hgt);
  grad.addColorStop(0, accent + '55');
  grad.addColorStop(1, accent + '00');
  ctx.beginPath();
  data.forEach((v, i) => (i ? ctx.lineTo(i * step, y(v)) : ctx.moveTo(0, y(v))));
  ctx.lineTo(w, hgt); ctx.lineTo(0, hgt); ctx.closePath();
  ctx.fillStyle = grad; ctx.fill();

  ctx.beginPath();
  data.forEach((v, i) => (i ? ctx.lineTo(i * step, y(v)) : ctx.moveTo(0, y(v))));
  ctx.strokeStyle = accent; ctx.lineWidth = 1.6; ctx.lineJoin = 'round'; ctx.stroke();

  const last = data[data.length - 1];
  ctx.fillStyle = accent;
  ctx.beginPath(); ctx.arc(w - 1.5, y(last), 2.2, 0, Math.PI * 2); ctx.fill();
}

setInterval(() => {
  S.rateNow = S.rate[S.rate.length - 1];
  S.rate.shift();
  S.rate.push(0);
  $('#rate-now').textContent = S.rateNow;
  drawSpark();
}, 1000);


// ---------------------------------------------------------------------------
// Left panel tabs
// ---------------------------------------------------------------------------

function showTab(name) {
  for (const tab of $$('.tab')) {
    const on = tab.dataset.tab === name;
    tab.classList.toggle('on', on);
    tab.setAttribute('aria-selected', String(on));
  }
  for (const pane of $$('.tab-pane')) pane.hidden = pane.dataset.pane !== name;
  store('knxide-tab', name);
}
for (const tab of $$('.tab')) tab.addEventListener('click', () => showTab(tab.dataset.tab));
showTab(['devices', 'health'].includes(store('knxide-tab')) ? store('knxide-tab') : 'groups');

// ---------------------------------------------------------------------------
// Devices: every physical address heard on the bus
// ---------------------------------------------------------------------------

function setDevices(list) {
  S.devices = new Map(list.map((d) => [d.address, { ...d, groups: new Map(d.groups) }]));
  renderDevices();
}

function noteDevice(t) {
  let d = S.devices.get(t.source);
  const isNew = !d;
  if (!d) {
    d = { address: t.source, frames: 0, first_seen: t.time, last_seen: t.time, groups: new Map() };
    S.devices.set(t.source, d);
  }
  d.frames += 1;
  d.last_seen = t.time;
  if (t.group) d.groups.set(t.destination, (d.groups.get(t.destination) || 0) + 1);
  if (isNew) renderDevices();
  else updateDevice(d);
}

const iaKey = (a) => a.split('.').map(Number);
function compareIa(a, b) {
  const x = iaKey(a), y = iaKey(b);
  return x[0] - y[0] || x[1] - y[1] || x[2] - y[2];
}

function ago(ms) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 5) return 'just now';
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return new Date(ms).toLocaleDateString();
}

function topGroups(d) {
  return [...d.groups.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0], undefined, { numeric: true }));
}

function deviceMatches(d, q) {
  if (!q) return true;
  if (d.address.includes(q)) return true;
  for (const ga of d.groups.keys()) {
    if (ga.includes(q) || (S.byAddr.get(ga)?.name || '').toLowerCase().includes(q)) return true;
  }
  return false;
}

function renderDevices() {
  const list = $('#dev-list');
  if (!list) return;
  $('#dev-count').textContent = S.devices.size;
  const q = $('#dev-search').value.trim().toLowerCase();
  S.devEls = new Map();
  if (S.devices.size === 0) {
    list.replaceChildren(h('div', { class: 'tree-empty' },
      h('strong', {}, 'No devices heard yet'),
      'Every device that sends something on the bus shows up here, with what it talks to.'));
    return;
  }
  const lines = new Map();
  for (const d of [...S.devices.values()].sort((a, b) => compareIa(a.address, b.address))) {
    if (!deviceMatches(d, q)) continue;
    const [area, line] = iaKey(d.address);
    const key = `${area}.${line}`;
    if (!lines.has(key)) lines.set(key, []);
    lines.get(key).push(d);
  }
  if (lines.size === 0) {
    list.replaceChildren(h('div', { class: 'tree-empty' }, 'Nothing matches.'));
    return;
  }
  const frag = document.createDocumentFragment();
  for (const [key, devices] of lines) {
    const items = devices.map((d) => {
      const el = deviceEl(d);
      S.devEls.set(d.address, el);
      return el;
    });
    frag.append(h('details', { class: 'range', open: true },
      h('summary', {}, svg(ICONS.chevron, 'chev'), h('span', {}, `Line ${key}`), h('span', { class: 'range-count' }, devices.length)),
      h('div', { class: 'range-body' }, items)));
  }
  const top = list.scrollTop;
  list.replaceChildren(frag);
  list.scrollTop = top;
}

function refreshDevice(d, ping) {
  const old = S.devEls.get(d.address);
  if (!old) return;
  const q = $('#dev-search').value.trim().toLowerCase();
  if (!deviceMatches(d, q)) return;
  const el = deviceEl(d);
  old.replaceWith(el);
  S.devEls.set(d.address, el);
  if (ping) {
    const dot = $('.dev-dot', el);
    dot.classList.add('ping');
  }
}

function deviceSummary(d, own) {
  const groups = topGroups(d);
  const [firstGa] = groups[0] || [];
  return own ? 'sent from this console'
    : firstGa ? `${S.byAddr.get(firstGa)?.name || firstGa}${groups.length > 1 ? `  +${groups.length - 1}` : ''}`
    : 'no group traffic';
}

function firstHeard(d) {
  return `First heard ${new Date(d.first_seen).toLocaleTimeString()} · ${d.frames.toLocaleString()} frames`;
}

function gaList(groups) {
  if (!groups.length) return [];
  return [h('div', { class: 'dl' }, 'Sends to'), ...groups.map(([ga, n]) => h('button', {
    class: 'dev-ga', type: 'button', title: 'Select in the send panel',
    onclick: () => selectAddress(ga, true, true),
  }, h('span', { class: 'a' }, ga), h('span', { class: 'n' }, S.byAddr.get(ga)?.name || ''), h('span', { class: 'c' }, `×${n}`)))];
}

/** Live traffic: update numbers in place, never the buttons a user may be about to click. */
function updateDevice(d) {
  const el = S.devEls.get(d.address);
  if (!el || !el.isConnected) return;
  const dot = $('.dev-dot', el);
  dot.dataset.time = d.last_seen;
  dot.classList.add('recent');
  dot.classList.remove('ping');
  void dot.offsetWidth; // restart the animation
  dot.classList.add('ping');
  $('.dev-sum', el).textContent = deviceSummary(d, S.conn.address === d.address);
  $('.dev-meta b', el).textContent = d.frames.toLocaleString();
  const agoEl = $('.dev-ago', el);
  agoEl.dataset.time = d.last_seen;
  agoEl.textContent = ago(d.last_seen);
  const first = $('.dev-first', el);
  if (first) first.textContent = firstHeard(d);
  const gas = $('.dev-gas', el);
  if (gas) {
    const top = gas.closest('.ga-tree')?.scrollTop;
    gas.replaceChildren(...gaList(topGroups(d)));
    if (top !== undefined) gas.closest('.ga-tree').scrollTop = top;
  }
}

function markDevices() {
  for (const el of $$('.dev')) {
    const title = deviceWarning(el.dataset.ia);
    const addr = $('.dev-addr', el);
    let mark = $('.warn-mark', addr);
    if (title && !mark) addr.append(mark = h('span', { class: 'warn-mark' }, '⚠'));
    if (!title && mark) mark.remove();
    if (mark) mark.title = title;
  }
}

function deviceEl(d) {
  const open = S.openDevices.has(d.address);
  const own = S.conn.address === d.address;
  const groups = topGroups(d);
  const summary = deviceSummary(d, own);
  const recent = Date.now() - d.last_seen < 60_000;

  const row = h('button', {
    class: 'dev-row', type: 'button', 'aria-expanded': String(open),
    onclick: () => {
      if (S.openDevices.has(d.address)) S.openDevices.delete(d.address); else S.openDevices.add(d.address);
      if (S.confirming === d.address) S.confirming = null;
      refreshDevice(d, false);
    },
  },
  h('span', { class: 'dev-dot' + (recent ? ' recent' : ''), dataset: { time: d.last_seen } }),
  h('span', { class: 'dev-addr' }, d.address, own ? h('span', { class: 'dev-tag' }, 'this console') : null,
    deviceWarning(d.address) ? h('span', { class: 'warn-mark', title: deviceWarning(d.address) }, '⚠') : null),
  h('span', { class: 'dev-sum' }, summary),
  h('span', { class: 'dev-meta' }, h('b', {}, d.frames.toLocaleString()), h('span', { class: 'dev-ago', dataset: { time: d.last_seen } }, ago(d.last_seen))));

  const el = h('div', { class: 'dev' + (open ? ' open' : ''), dataset: { ia: d.address } }, row);
  if (!open) return el;

  const detail = h('div', { class: 'dev-detail' },
    h('div', { class: 'dl dev-first' }, firstHeard(d)),
    h('div', { class: 'dev-gas' }, gaList(groups)));

  if (!own) {
    const connected = S.conn.status === 'connected';
    if (S.confirming === d.address) {
      detail.append(h('div', { class: 'confirm' },
        h('p', {}, `Restart ${d.address}? The device restarts its application and keeps its configuration. Like ETS's “Restart device”.`),
        h('div', { class: 'btns' },
          h('button', { class: 'btn ghost', type: 'button', onclick: () => { S.confirming = null; refreshDevice(d, false); } }, 'Cancel'),
          h('button', {
            class: 'btn danger', type: 'button', disabled: !connected,
            onclick: () => {
              S.confirming = null;
              refreshDevice(d, false);
              request({ type: 'restart', address: d.address }, `Restart ${d.address}`);
            },
          }, 'Restart'))));
    } else {
      detail.append(h('div', { class: 'dev-actions' },
        h('button', {
          class: 'link-btn', type: 'button', disabled: !connected,
          title: connected ? '' : 'Connect to a gateway first',
          onclick: () => { S.confirming = d.address; refreshDevice(d, false); },
        }, 'Restart device…')));
    }
  }
  el.append(detail);
  return el;
}

$('#dev-search').addEventListener('input', debounce(renderDevices, 80));

setInterval(() => {
  for (const span of $$('.dev-ago, .ago')) span.textContent = ago(Number(span.dataset.time));
  for (const dot of $$('.dev-dot')) dot.classList.toggle('recent', Date.now() - Number(dot.dataset.time) < 60_000);
}, 1000);

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

function setIssues(issues) {
  S.issues = issues;
  const warnings = issues.filter((i) => i.severity === 'warning').length;
  const badge = $('#health-count');
  // Only warnings count; project notes live in the tab without calling for attention.
  badge.textContent = warnings;
  badge.hidden = warnings === 0;
  badge.classList.toggle('warn', warnings > 0);
  renderHealth();
  markGroups();
  markDevices();
}

/** Title of the first warning about a group address, if any. */
function groupWarning(ga) {
  return S.issues.find((i) => i.severity === 'warning' && i.subject === ga)?.title;
}

/** Title of the first warning about a device, if any. */
function deviceWarning(ia) {
  return S.issues.find((i) => i.severity === 'warning' && (i.subject === ia || (i.kind === 'stuck_value' && i.device === ia)))?.title;
}

function markGroups() {
  for (const btn of $$('.ga')) {
    const title = groupWarning(btn.dataset.ga);
    const meta = $('.ga-meta', btn);
    let mark = $('.warn-mark', meta);
    if (title && !mark) meta.append(mark = h('span', { class: 'warn-mark' }, '⚠'));
    if (!title && mark) mark.remove();
    if (mark) mark.title = title;
  }
}

function showInMonitor(subject) {
  $('#filter').value = subject;
  applyFilter();
  if (subject.includes('/')) selectAddress(subject, false, false);
}

function goToDevice(ia) {
  showTab('devices');
  S.openDevices.add(ia);
  $('#dev-search').value = '';
  renderDevices();
  S.devEls.get(ia)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
}

function renderHealth() {
  const list = $('#health-list');
  if (S.issues.length === 0) {
    S.healthShape = '';
    list.replaceChildren(h('div', { class: 'health-ok' }, svg(ICONS.check),
      h('strong', {}, 'No problems found'),
      'knxide keeps watching: stuck sensors, devices that go quiet, repeated frames and failed writes, and where the ETS project does not match what is on the bus.'));
    return;
  }
  const order = { warning: 0, info: 1 };
  const sections = [
    ['bus', 'Bus health', 'something is going wrong'],
    ['project', 'Project', 'ETS does not match the bus'],
  ].map(([category, title, hint]) => [title, hint, S.issues
    .filter((i) => i.category === category)
    // Stable order: cards should not jump around while you read or click them.
    .sort((a, b) => order[a.severity] - order[b.severity] || a.first_seen - b.first_seen || a.key.localeCompare(b.key))]);

  // Counts and times change constantly; rebuild cards only when what they say changes.
  const shape = JSON.stringify(sections.map(([title, , issues]) =>
    [title, issues.map((i) => [i.key, i.title, i.detail, i.severity, i.suggested_dpt, i.subject, i.device])]));
  if (shape === S.healthShape) {
    for (const issue of S.issues) {
      const card = list.querySelector(`.issue[data-key="${CSS.escape(issue.key)}"]`);
      if (!card) continue;
      $('.i-count', card).textContent = countText(issue);
      const agoEl = $('.ago', card);
      agoEl.dataset.time = issue.last_seen;
      agoEl.textContent = ago(issue.last_seen);
    }
    return;
  }
  S.healthShape = shape;
  const frag = document.createDocumentFragment();
  for (const [title, hint, issues] of sections) {
    if (issues.length === 0) continue;
    frag.append(h('div', { class: 'health-section' }, title, h('span', {}, hint)));
    for (const issue of issues) frag.append(issueCard(issue));
  }
  const top = list.scrollTop;
  list.replaceChildren(frag);
  list.scrollTop = top;
}

const countText = (issue) => (issue.count > 1 ? `×${issue.count.toLocaleString()} · ` : '');

function issueCard(issue) {
  const deviceLabel = issue.device === issue.subject ? 'Go to device' : `Device ${issue.device}`;
  return h('div', { class: `issue ${issue.severity}`, dataset: { key: issue.key } },
    svg(issue.severity === 'warning' ? ICONS.warn : ICONS.info, 'i-icon'),
    h('div', { class: 'issue-title' }, issue.title),
    h('div', { class: 'issue-detail' }, issue.detail),
    h('div', { class: 'issue-meta' },
      h('span', { class: 'i-count' }, countText(issue)),
      h('span', { class: 'ago', dataset: { time: issue.last_seen } }, ago(issue.last_seen)),
      h('span', { class: 'spacer' }),
      issue.suggested_dpt ? h('button', {
        class: 'link-btn primary-link', type: 'button',
        title: 'Decode this address with that DPT from now on. Saved next to the project; the ETS export is not changed.',
        onclick: () => request({ type: 'set_dpt', address: issue.subject, dpt: issue.suggested_dpt }, `Use DPT ${issue.suggested_dpt} for ${issue.subject}`),
      }, `Use DPT ${issue.suggested_dpt} here`) : null,
      issue.subject ? h('button', { class: 'link-btn', type: 'button', onclick: () => showInMonitor(issue.subject) }, 'Show in monitor') : null,
      issue.device ? h('button', { class: 'link-btn', type: 'button', onclick: () => goToDevice(issue.device) }, deviceLabel) : null,
      h('button', {
        class: 'link-btn dismiss', type: 'button', title: 'Hide until it happens again',
        onclick: () => sendMsg({ type: 'dismiss_issue', key: issue.key }),
      }, 'Dismiss')));
}

// ---------------------------------------------------------------------------
// Send panel
// ---------------------------------------------------------------------------

const BOOL_LABELS = { 1: ['off', 'on'], 2: ['false', 'true'], 3: ['disable', 'enable'], 8: ['up', 'down'], 9: ['open', 'close'], 10: ['stop', 'start'] };
const HVAC_MODES = ['auto', 'comfort', 'standby', 'economy', 'building-protection'];

/** Input control for a DPT: the project's hint when it matches, else derived here. */
function kindFor(dpt, entry) {
  if (entry && entry.kind && entry.dpt === dpt) return entry.kind;
  if (!dpt) return null;
  const [main, sub] = dpt.split('.').map(Number);
  switch (main) {
    case 1: { const [off, on] = BOOL_LABELS[sub] || ['off', 'on']; return { kind: 'bool', off, on }; }
    case 2: return { kind: 'control' };
    case 3: return { kind: 'step' };
    case 5: return sub === 1 ? { kind: 'float', min: 0, max: 100 } : sub === 3 ? { kind: 'float', min: 0, max: 360 } : { kind: 'integer', min: 0, max: 255 };
    case 6: return { kind: 'integer', min: -128, max: 127 };
    case 7: return { kind: 'integer', min: 0, max: 65535 };
    case 8: return { kind: 'integer', min: -32768, max: 32767 };
    case 9: case 14: return { kind: 'float', min: -1e38, max: 1e38 };
    case 10: return { kind: 'time' };
    case 11: return { kind: 'date' };
    case 12: return { kind: 'integer', min: 0, max: 4294967295 };
    case 13: return { kind: 'integer', min: -2147483648, max: 2147483647 };
    case 16: return { kind: 'text', max_len: 14 };
    case 17: return { kind: 'integer', min: 1, max: 64 };
    case 18: return { kind: 'scene_control' };
    case 20: return sub === 102 ? { kind: 'hvac_mode' } : null;
    case 232: return { kind: 'rgb' };
    default: return null;
  }
}

let getValue = () => '';

function currentAddress() { return $('#send-ga').value.trim(); }
function currentDpt() { return $('#send-dpt').value.trim(); }

function selectAddress(addr, fromUser, scroll) {
  $('#send-ga').value = addr;
  S.dptAuto = true;
  onAddressChange();
  if (scroll) {
    const btn = $(`.ga[data-ga="${CSS.escape(addr)}"]`);
    if (btn) {
      let p = btn.parentElement;
      while (p && p.id !== 'ga-tree') { if (p.tagName === 'DETAILS') p.open = true; p = p.parentElement; }
      btn.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    }
  }
  if (fromUser) $('#value-control input, #value-control button')?.focus({ preventScroll: true });
}

function onAddressChange() {
  const addr = currentAddress();
  S.selected = S.byAddr.has(addr) ? addr : null;
  for (const b of $$('.ga.selected')) b.classList.remove('selected');
  if (S.selected) $(`.ga[data-ga="${CSS.escape(S.selected)}"]`)?.classList.add('selected');
  const entry = S.byAddr.get(addr);
  if (S.dptAuto) {
    $('#send-dpt').value = entry?.dpt ?? '';
  }
  updateDptHint();
  renderSendInfo();
  renderControl();
}

function updateDptHint() {
  const entry = S.byAddr.get(currentAddress());
  const hint = $('#dpt-hint');
  if (!entry?.dpt || currentDpt() !== entry.dpt) { hint.replaceChildren(); return; }
  if (!entry.overridden) { hint.replaceChildren('· from project'); return; }
  hint.replaceChildren(`· override (ETS: ${entry.project_dpt ?? 'none'})`,
    h('button', {
      class: 'reset', type: 'button',
      onclick: () => request({ type: 'set_dpt', address: entry.address, dpt: null }, `Reset DPT of ${entry.address}`),
    }, 'reset'));
}

function renderSendInfo() {
  const addr = currentAddress();
  const entry = S.byAddr.get(addr);
  const last = S.values.get(addr);
  const info = $('#send-info');
  if (!entry && !last) { info.replaceChildren(); return; }
  info.replaceChildren(...[
    entry ? h('div', { class: 'info-name' }, entry.name) : null,
    entry?.path?.length ? h('div', { class: 'info-path' }, entry.path.join(' › ')) : null,
    last ? h('div', {}, 'Last value ', h('span', { class: 'info-value' }, shownValue(last)), ` · ${clock(last.time)}`) : null,
  ].filter(Boolean));
}

function renderControl() {
  const entry = S.byAddr.get(currentAddress());
  const dpt = currentDpt();
  const kind = kindFor(dpt, entry);
  const box = $('#value-control');
  const last = S.values.get(currentAddress());
  const writeNow = (v) => write(v);
  let el;

  switch (kind?.kind) {
    case 'bool': {
      const current = (last?.value || '').toLowerCase();
      el = h('div', { class: 'big-toggle' },
        h('button', { type: 'button', class: 'btn' + (current === kind.off ? ' on-btn' : ''), onclick: () => writeNow(kind.off) }, kind.off),
        h('button', { type: 'button', class: 'btn' + (current === kind.on ? ' on-btn' : ''), onclick: () => writeNow(kind.on) }, kind.on));
      getValue = () => kind.on;
      break;
    }
    case 'step': {
      const blinds = dpt.startsWith('3.008');
      el = h('div', { class: 'trio' },
        h('button', { type: 'button', class: 'btn', onclick: () => writeNow('-1') }, blinds ? '▲ Up' : '− Dim down'),
        h('button', { type: 'button', class: 'btn', onclick: () => writeNow('0') }, '■ Stop'),
        h('button', { type: 'button', class: 'btn', onclick: () => writeNow('+1') }, blinds ? '▼ Down' : '+ Dim up'));
      getValue = () => '0';
      break;
    }
    case 'float':
    case 'integer': {
      const ranged = kind.kind === 'float' && kind.max - kind.min <= 360;
      const lastNum = parseFloat(last?.value ?? '');
      if (ranged) {
        const out = h('span', { class: 'slider-value' });
        const range = h('input', { type: 'range', min: kind.min, max: kind.max, step: kind.max === 100 ? 1 : 0.5 });
        range.value = Number.isFinite(lastNum) ? lastNum : kind.min;
        const unit = entry?.unit ?? (kind.max === 100 ? '%' : '°');
        const sync = () => { out.textContent = `${range.value} ${unit}`; };
        range.addEventListener('input', sync);
        sync();
        el = h('div', { class: 'row-ctl' }, range, out);
        getValue = () => range.value;
      } else {
        const input = h('input', {
          type: 'number', class: 'mono',
          step: kind.kind === 'integer' ? 1 : 'any',
          min: kind.min > -1e30 ? kind.min : null, max: kind.max < 1e30 ? kind.max : null,
          placeholder: kind.kind === 'integer' ? `${kind.min} – ${kind.max}` : 'number',
        });
        if (Number.isFinite(lastNum)) input.value = lastNum;
        el = h('div', { class: 'row-ctl' }, input, entry?.unit ? h('span', { class: 'slider-value' }, entry.unit) : null);
        getValue = () => input.value;
      }
      break;
    }
    case 'control': {
      const c = h('input', { type: 'checkbox' });
      const v = h('input', { type: 'checkbox' });
      el = h('div', { class: 'row-ctl' }, h('label', { class: 'check' }, c, 'priority control'), h('label', { class: 'check' }, v, 'value on'));
      getValue = () => `${c.checked ? 1 : 0},${v.checked ? 1 : 0}`;
      break;
    }
    case 'time': {
      const now = new Date();
      const time = h('input', { type: 'time', step: 1, value: `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}` });
      const day = h('select', {}, ['', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'].map((d) => h('option', { value: d }, d || 'no day')));
      el = h('div', { class: 'row-ctl' }, day, time);
      getValue = () => (day.value ? `${day.value} ` : '') + (time.value.length === 5 ? time.value + ':00' : time.value);
      break;
    }
    case 'date': {
      const date = h('input', { type: 'date', value: new Date().toISOString().slice(0, 10) });
      el = h('div', { class: 'row-ctl' }, date);
      getValue = () => date.value;
      break;
    }
    case 'text': {
      const text = h('input', { type: 'text', maxlength: kind.max_len, placeholder: `up to ${kind.max_len} characters` });
      el = h('div', { class: 'row-ctl' }, text);
      getValue = () => text.value;
      break;
    }
    case 'scene_control': {
      const scene = h('input', { type: 'number', min: 1, max: 64, value: 1, class: 'mono' });
      const learn = h('input', { type: 'checkbox' });
      el = h('div', { class: 'row-ctl' }, scene, h('label', { class: 'check' }, learn, 'learn'));
      getValue = () => (learn.checked ? `learn ${scene.value}` : scene.value);
      break;
    }
    case 'hvac_mode': {
      el = h('div', { class: 'modes' }, HVAC_MODES.map((m) =>
        h('button', { type: 'button', class: 'btn' + ((last?.value || '') === m ? ' primary' : ''), onclick: () => writeNow(m) }, m.replace('-', ' '))));
      getValue = () => 'comfort';
      break;
    }
    case 'rgb': {
      const color = h('input', { type: 'color', value: '#ffaa33' });
      el = h('div', { class: 'row-ctl' }, color, h('span', { class: 'raw-hint' }, 'pick a colour, then Write'));
      getValue = () => color.value;
      break;
    }
    default: {
      const raw = h('input', { type: 'text', class: 'mono', placeholder: '0–63 or 0x0c1a' });
      el = h('div', {}, h('div', { class: 'row-ctl' }, raw),
        h('div', { class: 'raw-hint' }, dpt ? `DPT ${dpt} has no input helper: enter a raw value.` : 'No DPT known: enter a raw value, or set a DPT above.'));
      getValue = () => raw.value;
    }
  }
  box.replaceChildren(el);
  S.controlKind = kind?.kind ?? null;
  const immediate = ['bool', 'step', 'hvac_mode'].includes(kind?.kind);
  $('#write-btn').hidden = immediate;
  $('.send-actions').style.gridTemplateColumns = immediate ? '1fr' : '1fr 1fr';
  updateSendEnabled();
}

function updateSendEnabled() {
  const connected = S.conn.status === 'connected';
  for (const b of $$('#send-form button')) b.disabled = !connected;
  $('#send-form').title = connected ? '' : 'Connect to a gateway to send';
}

$('#send-ga').addEventListener('input', () => { S.dptAuto = true; onAddressChange(); });
$('#send-dpt').addEventListener('input', debounce(() => {
  S.dptAuto = currentDpt() === '';
  updateDptHint();
  renderControl();
}, 150));
$('#send-form').addEventListener('submit', (e) => { e.preventDefault(); write(getValue()); });
$('#read-btn').addEventListener('click', () => {
  const address = currentAddress();
  if (!checkAddress(address)) return;
  request({ type: 'read', address }, `Read ${address}`);
});

function checkAddress(address) {
  if (/^\d{1,2}\/\d{1,4}(\/\d{1,3})?$|^\d{1,5}$/.test(address)) return true;
  toast('Enter a group address like 1/2/3', 'err');
  $('#send-ga').focus();
  return false;
}

function write(value) {
  const address = currentAddress();
  if (!checkAddress(address)) return;
  if (String(value).trim() === '') { toast('Enter a value to write', 'err'); return; }
  const dpt = currentDpt() || null;
  const label = S.byAddr.get(address)?.name;
  request({ type: 'write', address, value: String(value), dpt }, `Write ${value} → ${address}${label ? ` (${label})` : ''}`);
}

// ---------------------------------------------------------------------------
// Activity and toasts
// ---------------------------------------------------------------------------

function request(msg, label) {
  const id = S.nextId++;
  if (!sendMsg({ ...msg, id })) return;
  const item = h('li', { class: 'pending' },
    svg(ICONS.pending, 'a-icon'),
    h('span', { class: 'a-msg' }, label),
    h('span', { class: 'a-time' }, clock(Date.now())));
  const list = $('#activity');
  $('.activity-empty', list)?.remove();
  list.prepend(item);
  while (list.childElementCount > 60) list.lastElementChild.remove();
  S.pending.set(id, item);
}

function settle(id, ok, message) {
  const item = S.pending.get(id);
  if (!item) return; // another tab's request
  S.pending.delete(id);
  item.className = ok ? 'ok' : 'err';
  item.firstChild.replaceWith(svg(ok ? ICONS.ok : ICONS.err, 'a-icon'));
  $('.a-msg', item).textContent = message;
  if (!ok) toast(message, 'err');
}

function toast(message, kind = '') {
  const el = h('div', { class: `toast ${kind}` }, message);
  $('#toasts').append(el);
  setTimeout(() => el.remove(), kind === 'err' ? 6000 : 3500);
}

$('#activity').append(h('li', { class: 'activity-empty' }, 'Writes and reads you send appear here.'));

// ---------------------------------------------------------------------------
// Drop an ETS export to load it
// ---------------------------------------------------------------------------

let dragDepth = 0;
const hasFiles = (e) => Array.from(e.dataTransfer?.types || []).includes('Files');
window.addEventListener('dragenter', (e) => { if (!hasFiles(e)) return; dragDepth++; $('#drop').hidden = false; });
window.addEventListener('dragleave', () => { if (--dragDepth <= 0) { dragDepth = 0; $('#drop').hidden = true; } });
window.addEventListener('dragover', (e) => { if (hasFiles(e)) e.preventDefault(); });
window.addEventListener('drop', async (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth = 0;
  $('#drop').hidden = true;
  const file = e.dataTransfer.files[0];
  if (!file) return;
  const bytes = new Uint8Array(await file.arrayBuffer());
  // ETS writes UTF-8, UTF-16 or Windows-1252; detect rather than guess.
  let text;
  if (bytes[0] === 0xff && bytes[1] === 0xfe) text = new TextDecoder('utf-16le').decode(bytes);
  else if (bytes[0] === 0xfe && bytes[1] === 0xff) text = new TextDecoder('utf-16be').decode(bytes);
  else {
    try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
    catch { text = new TextDecoder('windows-1252').decode(bytes); }
  }
  request({ type: 'load_project', name: file.name, content: text }, `Load ${file.name}`);
});

// ---------------------------------------------------------------------------

renderTree();
renderDevices();
renderHealth();
renderControl();
updateEmpty();
openSocket();
