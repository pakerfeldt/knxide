'use strict';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

/** Build an element. Text always goes in as text, never as HTML. */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === null || v === undefined || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k === 'style') el.style.cssText = v;
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

const ICONS = {
  bulb: '<path d="M9 18h6M10 21h4"/><path d="M12 3a6 6 0 0 0-3.6 10.8c.7.5 1.1 1.3 1.1 2.2h5c0-.9.4-1.7 1.1-2.2A6 6 0 0 0 12 3z"/>',
  plug: '<path d="M9 3v5M15 3v5"/><path d="M6 8h12v3a6 6 0 0 1-12 0z"/><path d="M12 17v4"/>',
  power: '<path d="M12 3v8"/><path d="M6.3 6.3a8 8 0 1 0 11.4 0"/>',
  thermo: '<path d="M10 14.5V5a2 2 0 1 1 4 0v9.5a4 4 0 1 1-4 0z"/><path d="M12 11v5"/>',
  bolt: '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
  drop: '<path d="M12 3s6 6.5 6 11a6 6 0 0 1-12 0c0-4.5 6-11 6-11z"/>',
  wind: '<path d="M3 8h11a3 3 0 1 0-3-3M3 16h15a3 3 0 1 1-3 3M3 12h17"/>',
  rain: '<path d="M7 15a4.5 4.5 0 0 1-.4-9A6 6 0 0 1 18 7.5a3.8 3.8 0 0 1-.5 7.5z"/><path d="M8 18l-1 3M12 18l-1 3M16 18l-1 3"/>',
  fan: '<circle cx="12" cy="12" r="1.6"/><path d="M12 10.4C11 6 12.5 3 15 3.5c2.3.5 1.5 4.6-1.6 7.3M13.6 12.6c4.2 1.5 6.2 4.2 4.6 6.1-1.5 1.8-4.8-.9-5.4-4.9M10.6 12.9c-3.4 2.9-6.7 3.3-7.4 1-.7-2.3 3.2-3.7 7.2-2.6"/>',
  radiator: '<rect x="4" y="6" width="16" height="12" rx="2"/><path d="M8 6v12M12 6v12M16 6v12M6 21v-3M18 21v-3"/>',
  door: '<path d="M6 21V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v17"/><path d="M3 21h18"/><circle cx="14.5" cy="12" r="1"/>',
  garage: '<path d="M3 21V9l9-5 9 5v12"/><path d="M7 21v-9h10v9M7 15h10M7 18h10"/>',
  shield: '<path d="M12 3 5 6v5c0 4.5 3 8.4 7 10 4-1.6 7-5.5 7-10V6z"/><path d="m9 12 2 2 4-4"/>',
  car: '<path d="M5 17h14v-4.5l-1.8-4.6A2 2 0 0 0 15.3 6H8.7a2 2 0 0 0-1.9 1.9L5 12.5z"/><path d="M5 12.5h14"/><circle cx="8" cy="17" r="1.8"/><circle cx="16" cy="17" r="1.8"/>',
  wave: '<path d="M2 8c2 0 2-1.5 4-1.5S8 8 10 8s2-1.5 4-1.5S16 8 18 8s2-1.5 4-1.5"/><path d="M2 13c2 0 2-1.5 4-1.5S8 13 10 13s2-1.5 4-1.5S16 13 18 13s2-1.5 4-1.5"/><path d="M2 18c2 0 2-1.5 4-1.5S8 18 10 18s2-1.5 4-1.5S16 18 18 18s2-1.5 4-1.5"/>',
  gauge: '<path d="M4 15a8 8 0 1 1 16 0"/><path d="m12 15 4-5"/>',
  scene: '<path d="m12 3 1.8 4.7L18.5 9l-4.7 1.8L12 15.5l-1.8-4.7L5.5 9l4.7-1.3z"/><path d="M18 15l.8 2.2L21 18l-2.2.8L18 21l-.8-2.2L15 18l2.2-.8z"/>',
  blinds: '<path d="M4 4h16M5 4v16M19 4v16"/><path d="M5 8h14M5 12h14M5 16h14"/>',
  flame: '<path d="M12 3c1 3.5 5 5.5 5 10a5 5 0 0 1-10 0c0-2.5 1.5-4 2.5-5 .3 1.6 1 2.5 2 3-.3-3 0-5.5.5-8z"/>',
  tap: '<path d="M9 11V5a1.5 1.5 0 0 1 3 0v5"/><path d="M12 10V8.5a1.5 1.5 0 0 1 3 0V11"/><path d="M15 10.5a1.5 1.5 0 0 1 3 0V15a6 6 0 0 1-6 6h-.5a6 6 0 0 1-4.7-2.3L4.5 15.5a1.6 1.6 0 0 1 2.4-2.1L9 15.5"/>',
  pencil: '<path d="M4 20h4L18.5 9.5a2.1 2.1 0 0 0-3-3L5 17v3z"/>',
  x: '<path d="M6 6l12 12M18 6 6 18"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  minus: '<path d="M5 12h14"/>',
  up: '<path d="m6 15 6-6 6 6"/>',
  down: '<path d="m6 9 6 6 6-6"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="1.5"/>',
};

function icon(name) {
  const el = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  el.setAttribute('viewBox', '0 0 24 24');
  el.setAttribute('aria-hidden', 'true');
  el.innerHTML = ICONS[name] || ICONS.gauge; // static markup from this file only
  return el;
}

function store(key, value) {
  try {
    if (value === undefined) return localStorage.getItem(key);
    localStorage.setItem(key, value);
  } catch { /* private mode */ }
  return null;
}

const newId = () => (crypto.randomUUID ? crypto.randomUUID() : `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`);
const isGa = (s) => /^\s*(\d{1,2}\/\d{1,4}(\/\d{1,3})?|\d{1,5})\s*$/.test(s || '');
const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, n));
const capitalise = (s) => (s ? s.charAt(0).toLocaleUpperCase() + s.slice(1) : s);

function ago(ms) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 10) return t('just now');
  if (s < 60) return `${s} ${t('s ago')}`;
  if (s < 3600) return `${Math.floor(s / 60)} ${t('min ago')}`;
  if (s < 86400) return `${Math.floor(s / 3600)} ${t('h ago')}`;
  return new Date(ms).toLocaleDateString(locale());
}

function toast(message, kind = '') {
  const el = h('div', { class: `toast ${kind}` }, message);
  $('#toasts').append(el);
  setTimeout(() => el.remove(), kind === 'err' ? 5000 : 2500);
}

// ---------------------------------------------------------------------------
// Language
// ---------------------------------------------------------------------------

/** Swedish for every fixed text, keyed by the English. Missing entries stay English. */
const SV = {
  'just now': 'nyss', 's ago': 's sedan', 'min ago': 'min sedan', 'h ago': 'h sedan',
  'Lost contact with the dashboard server. Reconnecting…': 'Tappade kontakten med servern. Försöker igen…',
  'No connection to the KNX bus': 'Ingen anslutning till KNX-bussen', 'Retrying…': 'Försöker igen…',
  'Connected to': 'Ansluten till', 'Connecting to': 'Ansluter till',
  'Not connected to the dashboard server': 'Inte ansluten till servern',
  'Add tile': 'Lägg till ruta', 'Nothing here yet': 'Här finns inget än',
  'Tap the pencil to add tiles, or let knxide suggest some from your ETS project.': 'Tryck på pennan för att lägga till rutor, eller låt knxide föreslå några från ditt ETS-projekt.',
  'Now': 'Nu', 'Target': 'Börvärde', 'No target value yet': 'Inget börvärde än', 'Climate': 'Klimat',
  'Heating': 'Värme', 'Lower': 'Sänk', 'Raise': 'Höj', 'Other': 'Övrigt', 'on': 'tänd', 'lights on': 'tända',
  'All off': 'Släck alla', 'Up': 'Upp', 'Stop': 'Stopp', 'Down': 'Ner', 'Open': 'Öppen', 'closed': 'stängd',
  'Scene': 'Scen', 'Sends': 'Skickar',
  'Auto': 'Auto', 'Comfort': 'Komfort', 'Standby': 'Standby', 'Eco': 'Eko', 'Protect': 'Skydd',
  'Saved': 'Sparat', 'A dashboard needs at least one page': 'En panel behöver minst en sida',
  'Removed': 'Tog bort', 'Cancel to undo.': 'Avbryt för att ångra.', 'New page': 'Ny sida', 'Home': 'Hem',
  'Edit': 'Redigera', 'Remove': 'Ta bort', 'Edit tile': 'Redigera ruta', 'Not a group address': 'Inte en gruppadress',
  'is not a group address': 'är inte en gruppadress', 'e.g. 1/2/3': 't.ex. 1/2/3',
  'No group ranges in the project (load knx.xml with --project)': 'Projektet har inga grupper (starta med --project knx.xml)',
  'Looking…': 'Letar…', 'Nothing to suggest here.': 'Inget att föreslå här.',
  'Switch': 'Brytare', 'Dimmer': 'Dimmer', 'Value': 'Värde', 'Temperature': 'Temperatur', 'Blinds': 'Markis/persienn',
  'Heating mode': 'Värmeläge', 'Button': 'Knapp',
  'Switch address': 'Bryt-adress', 'Status address (optional)': 'Statusadress (valfri)',
  'On/off address (optional)': 'Till/från-adress (valfri)', 'On/off status (optional)': 'Till/från-status (valfri)',
  'Brightness address (%)': 'Ljusstyrka-adress (%)', 'Brightness status (optional)': 'Ljusstyrka-status (valfri)',
  'Address': 'Adress', 'Target temperature address': 'Börvärde-adress', 'Target status (optional)': 'Börvärde-status (valfri)',
  'Measured temperature (optional)': 'Uppmätt temperatur (valfri)', 'Heating output, % (optional)': 'Värmeeffekt, % (valfri)',
  'Step (°)': 'Steg (°)',
  'Scene address': 'Scen-adress', 'Scene number (1–64)': 'Scennummer (1–64)', 'Up/down address': 'Upp/ner-adress',
  'Stop address (optional)': 'Stopp-adress (valfri)', 'Position address (optional)': 'Position-adress (valfri)',
  'Position status (optional)': 'Position-status (valfri)', 'Mode address': 'Läge-adress', 'Mode status (optional)': 'Läge-status (valfri)',
  'Value to send': 'Värde att skicka', 'DPT (optional, else from project)': 'DPT (valfri, annars från projektet)',
  'Drag sideways to dim': 'Dra i sidled för att dimra',
  // static page texts
  '+ Page': '+ Sida', 'Suggest from project…': 'Föreslå från projektet…', 'Delete page': 'Ta bort sida',
  'Cancel': 'Avbryt', 'Save': 'Spara', 'Tile': 'Ruta', 'Kind': 'Typ', 'Label': 'Namn', 'Delete': 'Ta bort', 'OK': 'OK',
  'Room': 'Rum', 'Room or section (optional)': 'Rum eller avsnitt (valfritt)',
  'Suggest tiles from the project': 'Föreslå rutor från projektet', 'Group range': 'Grupp',
  'Pick a part of your ETS project. Commands and their status addresses are paired by name; untick what you do not want.':
    'Välj en del av ETS-projektet. Kommandon och deras statusadresser paras ihop via namnen; bocka ur det du inte vill ha.',
  'Add as new page': 'Lägg till som ny sida', 'Add to this page': 'Lägg till på denna sida', 'Page': 'Sida',
  'Dashboard title': 'Titel', 'Language': 'Språk', 'Page name': 'Sidans namn', 'Edit dashboard': 'Redigera panelen',
  '◀ Move': '◀ Flytta', 'Move ▶': 'Flytta ▶', 'Switch: on/off': 'Brytare: till/från',
  'Dimmer: on/off + brightness': 'Dimmer: till/från + ljusstyrka', 'Value: show a reading': 'Värde: visa en mätning',
  'Temperature: target − / +': 'Temperatur: börvärde − / +', 'Blinds: up / stop / down': 'Markis/persienn: upp / stopp / ner',
  'Button: send a value': 'Knapp: skicka ett värde',
};

/** Words in values from the bus: DPT labels and HVAC modes. */
const SV_VALUES = {
  on: 'På', off: 'Av', open: 'Öppen', close: 'Stängd', up: 'Upp', down: 'Ner', true: 'Ja', false: 'Nej',
  enable: 'Aktiv', disable: 'Inaktiv', start: 'Start', stop: 'Stopp', auto: 'Auto', comfort: 'Komfort',
  standby: 'Standby', economy: 'Eko', 'building-protection': 'Skydd', pulses: 'pulser',
};
const EN_VALUES = { on: 'On', off: 'Off', open: 'Open', close: 'Closed', true: 'Yes', false: 'No', enable: 'Active', disable: 'Inactive' };

const lang = () => (S.draft || S.dashboard).language || 'en';
const locale = () => (lang() === 'sv' ? 'sv-SE' : 'en-GB');
const t = (text) => (lang() === 'sv' ? SV[text] ?? text : text);

/** A word from the bus in the page's language. */
function vt(text) {
  if (!text) return text;
  const table = lang() === 'sv' ? SV_VALUES : EN_VALUES;
  return text.split(' ').map((w) => table[w.toLowerCase()] ?? w).join(' ');
}

/** A number in the page's language, with exactly `digits` decimals. */
const num = (n, digits = 0) =>
  n.toLocaleString(locale(), { minimumFractionDigits: digits, maximumFractionDigits: digits });

function applyLanguage() {
  document.documentElement.lang = lang();
  for (const el of $$('[data-t]')) {
    el.dataset.src ??= el.textContent;
    el.textContent = t(el.dataset.src);
  }
  for (const el of $$('[data-t-ph]')) {
    el.dataset.srcPh ??= el.placeholder;
    el.placeholder = t(el.dataset.srcPh);
  }
  for (const el of $$('[data-t-title]')) {
    el.dataset.srcTitle ??= el.title;
    el.title = t(el.dataset.srcTitle);
    el.setAttribute('aria-label', el.title);
  }
}

// ---------------------------------------------------------------------------
// State and server link
// ---------------------------------------------------------------------------

const S = {
  dashboard: { title: 'Home', pages: [] },
  draft: null, // copy being edited
  project: [],
  byAddr: new Map(),
  ranges: [],
  values: new Map(),
  state: { status: 'connecting' },
  page: store('knxide-page'),
  tiles: [], // rendered tiles and room headers: { update, gas }
  nextId: 1,
  waiting: new Map(),
};

const current = () => S.draft || S.dashboard;
const editing = () => S.draft !== null;
const value = (ga) => (ga ? S.values.get(ga.trim()) : undefined);

let socket = null;
let retryMs = 500;
let everOpened = false;

function openSocket() {
  socket = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws');
  socket.addEventListener('open', () => { everOpened = true; retryMs = 500; $('#offline').hidden = true; });
  socket.addEventListener('message', (e) => {
    let msg;
    try { msg = JSON.parse(e.data); } catch { return; }
    onMessage(msg);
  });
  socket.addEventListener('close', () => {
    if (everOpened) showOffline(t('Lost contact with the dashboard server. Reconnecting…'));
    setTimeout(openSocket, retryMs);
    retryMs = Math.min(retryMs * 2, 5000);
  });
}

function showOffline(text) {
  const el = $('#offline');
  el.textContent = text;
  el.hidden = !text;
  document.body.classList.toggle('offline-mode', Boolean(text));
}

/** Send a request and resolve with the server's result. */
function request(msg) {
  return new Promise((resolve) => {
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      resolve({ ok: false, message: t('Not connected to the dashboard server') });
      return;
    }
    const id = S.nextId++;
    S.waiting.set(id, resolve);
    socket.send(JSON.stringify({ ...msg, id }));
  });
}

async function write(ga, text, dpt) {
  const result = await request({ type: 'write', ga, value: String(text), dpt: dpt || null });
  if (!result.ok) toast(result.message, 'err');
  return result.ok;
}

function onMessage(msg) {
  switch (msg.type) {
    case 'hello':
      S.project = msg.project;
      S.byAddr = new Map(msg.project.map((e) => [e.address, e]));
      S.ranges = msg.ranges;
      S.values = new Map(Object.entries(msg.values));
      if (!editing()) S.dashboard = msg.dashboard;
      $('#ga-list').replaceChildren(...msg.project.map((e) => h('option', { value: e.address }, `${e.name}${e.dpt ? ` · ${e.dpt}` : ''}`)));
      setState(msg.state);
      render();
      break;
    case 'state':
      setState(msg.state);
      break;
    case 'value':
      S.values.set(msg.ga, msg.value);
      for (const item of S.tiles) if (item.gas.includes(msg.ga)) item.update();
      break;
    case 'dashboard':
      if (!editing()) {
        S.dashboard = msg.dashboard;
        render();
      }
      break;
    case 'result': {
      const resolve = S.waiting.get(msg.id);
      if (resolve) { S.waiting.delete(msg.id); resolve(msg); }
      break;
    }
  }
}

function setState(state) {
  S.state = state;
  const dot = $('#conn');
  dot.dataset.status = state.status;
  dot.title = state.status === 'connected' ? `${t('Connected to')} ${state.gateway}`
    : state.error ? `${state.status}: ${state.error}` : `${t('Connecting to')} ${state.gateway}`;
  if (socket && socket.readyState === WebSocket.OPEN) {
    showOffline(state.status === 'connected' ? '' : `${t('No connection to the KNX bus')}${state.error ? `: ${state.error}` : ''}. ${t('Retrying…')}`);
  }
}

// ---------------------------------------------------------------------------
// Pages and rooms
// ---------------------------------------------------------------------------

function currentPage() {
  const pages = current().pages;
  return pages.find((p) => p.id === S.page) || pages[0];
}

function tickClock() {
  const now = new Date();
  const day = capitalise(now.toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long' }));
  const time = now.toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' });
  $('#today').textContent = `${day} · ${time}`;
}
setInterval(tickClock, 15000);

function render() {
  const dash = current();
  applyLanguage();
  tickClock();
  $('#title').textContent = dash.title || t('Home');
  document.title = dash.title || 'knxide';
  const page = currentPage();
  if (page) S.page = page.id;
  $('#pages').replaceChildren(...dash.pages.map((p) => h('button', {
    class: 'page-tab' + (p.id === S.page ? ' on' : ''), type: 'button',
    'aria-current': p.id === S.page ? 'page' : null,
    onclick: () => { S.page = p.id; store('knxide-page', p.id); render(); window.scrollTo({ top: 0 }); },
  }, p.name || 'Untitled')));
  $('#pages').hidden = dash.pages.length < 2 && !editing();
  if (editing()) {
    $('#page-name').value = page?.name || '';
    $('#title-input').value = dash.title || '';
    $('#lang-select').value = lang();
  }
  renderPage();
}

/** Tiles of the page in rooms, in the order rooms first appear. Tiles without a room come last. */
function rooms(page) {
  const byName = new Map();
  const loose = [];
  (page?.tiles || []).forEach((tile, index) => {
    const name = (tile.group || '').trim();
    if (!name) { loose.push({ tile, index }); return; }
    const key = name.toLowerCase();
    if (!byName.has(key)) byName.set(key, { name, items: [] });
    byName.get(key).items.push({ tile, index });
  });
  const list = [...byName.values()];
  if (loose.length) list.push({ name: list.length ? t('Other') : '', items: loose, loose: true });
  return list;
}

function renderPage() {
  const main = $('#grid');
  const page = currentPage();
  S.tiles = [];
  const sections = rooms(page).map((room) => renderRoom(room));
  if (editing()) {
    sections.push(h('div', { class: 'grid' }, addButton(null)));
  } else if (sections.length === 0) {
    sections.push(h('div', { class: 'empty-page' }, h('strong', {}, t('Nothing here yet')),
      t('Tap the pencil to add tiles, or let knxide suggest some from your ETS project.')));
  }
  main.classList.toggle('editing', editing());
  main.replaceChildren(...sections);
  $('#group-list').replaceChildren(...rooms(page).filter((r) => !r.loose).map((r) => h('option', { value: r.name })));
}

function addButton(group) {
  return h('button', { class: 'add-tile', type: 'button', onclick: () => openTileDialog(null, group) }, icon('plus'), t('Add tile'));
}

function renderRoom(room) {
  const grid = h('div', { class: 'grid' }, room.items.map(({ tile, index }) => {
    const built = build(tile, room.loose ? null : room.name);
    S.tiles.push(built);
    built.update();
    if (editing()) decorate(built.el, tile, index);
    return built.el;
  }));
  if (editing() && !room.loose) grid.append(addButton(room.name));
  if (!room.name) return h('section', { class: 'room' }, grid);

  // The header counts lit lights and offers to switch them all off.
  const lights = room.items.map((i) => i.tile).filter((tile) => isLight(tile));
  const meta = h('span', { class: 'room-meta' });
  const allOff = h('button', { class: 'pill', type: 'button', hidden: true, onclick: () => lightsOff(lights) }, t('All off'));
  const header = {
    gas: lights.flatMap(shownOf),
    update() {
      const lit = lights.filter((tile) => lightOn(tile)).length;
      meta.textContent = lit ? `${lit} ${lit === 1 ? t('on') : t('lights on')}` : '';
      allOff.hidden = lit < 2 || editing();
    },
  };
  header.update();
  S.tiles.push(header);
  return h('section', { class: 'room' },
    h('header', { class: 'room-head' }, h('h2', {}, room.name), meta, allOff),
    grid);
}

/** "Hall tak" under "Hall" reads "Tak"; a label equal to the room gets a kind name. */
function displayLabel(tile, group) {
  const label = (tile.label || '').trim();
  if (!group) return label;
  const g = group.toLowerCase();
  const l = label.toLowerCase();
  if (l === g) return tile.type === 'setpoint' ? t('Climate') : label;
  if (l.startsWith(`${g} `)) return capitalise(label.slice(group.length).trim());
  return label;
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/** A value for display: { main, unit, attention }. Numbers get sensible decimals. */
function format(v) {
  if (!v) return null;
  if (typeof v.value === 'number') {
    const unit = (/^[-+\d.,eE\s]+(.*)$/.exec(v.text || '')?.[1] || '').trim();
    const n = v.value;
    let digits = Number.isInteger(n) || Math.abs(n) >= 100 ? 0 : 1;
    if (unit === '%') digits = 0;
    if (unit === '°C' && Math.abs(n) < 100) digits = 1;
    return { main: num(n, digits), unit: unit === 'pulses' ? '' : unit };
  }
  return { main: vt(v.text || ''), unit: '' };
}

/** "Port öppen: off" reads better as "Port öppen: Stängd". */
function formatReading(tile, v) {
  if (typeof v?.value === 'boolean' && /öppen|\bopen\b/i.test(tile.label || '')) {
    return { main: vt(v.value ? 'open' : 'close'), unit: '' };
  }
  return format(v);
}

/** Readings that deserve a look: open doors, active alarms. */
function attention(tile, v) {
  if (!v) return false;
  const l = (tile.label || '').toLowerCase();
  if (v.text === 'open') return true;
  // An armed alarm ("enable") is the normal state, not something to look at.
  return v.value === true && v.text !== 'enable' && /öppen|open|larm|alarm|läck|leak|fel|fault/.test(l);
}

function isOn(v) {
  if (!v) return null;
  if (typeof v.value === 'boolean') return v.value;
  if (typeof v.value === 'number') return v.value > 0;
  return null;
}

const STALE_MS = 30 * 60 * 1000;

// ---------------------------------------------------------------------------
// Tiles
// ---------------------------------------------------------------------------

/** Icon from the label first (it says what the thing is), then from the kind and unit. */
function iconFor(tile, v) {
  const l = (tile.label || '').toLowerCase();
  // A temperature is a temperature, whatever measures it.
  if (tile.type === 'value' && /°C/.test(v?.text || '') && !/pool/.test(l)) return 'thermo';
  const rules = [
    [/belysning|lampa\b|lamp\b|light|spot/, 'bulb'],
    [/uttag|socket|outlet|steckdose/, 'plug'],
    [/fläkt|fan|ventilation|lüfter/, 'fan'],
    [/markis|persienn|blind|jalousie|rullgardin|awning/, 'blinds'],
    [/garageport|garage door/, 'garage'],
    [/dörr|door|tür|entre$/, 'door'],
    [/larm|alarm/, 'shield'],
    [/laddbox|charger|wallbox/, 'bolt'],
    [/\bbil\b|car\b|motorvärmare/, 'car'],
    [/värmepump|heat pump|\blp\b|effekt|energi|kwh|förbrukning/, 'bolt'],
    [/pool/, 'wave'],
    [/pump|vatten|water/, 'drop'],
    [/handdukstork|element|radiator|heater|panel|värmare/, 'radiator'],
    [/vind\b|wind/, 'wind'],
    [/nederbörd|regn|rain/, 'rain'],
    [/sol|sun|lux/, 'sun'],
    [/aktiv|läge|tyst|kyla|power|mode/, 'power'],
  ];
  for (const [re, name] of rules) if (re.test(l)) return name;
  switch (tile.type) {
    case 'switch': case 'dimmer': return 'bulb';
    case 'setpoint': return 'thermo';
    case 'scene': return 'scene';
    case 'blinds': return 'blinds';
    case 'mode': return 'flame';
    case 'button': return 'tap';
  }
  const text = v?.text || '';
  if (/°C|°F|K$/.test(text)) return 'thermo';
  if (/W|Wh|V|A$/.test(text)) return 'bolt';
  if (/lx/.test(text)) return 'sun';
  if (/m\/s|km\/h/.test(text)) return 'wind';
  if (/%/.test(text)) return 'drop';
  return 'gauge';
}

const isLight = (tile) => (tile.type === 'switch' || tile.type === 'dimmer') && iconFor(tile) === 'bulb';

/** Addresses whose values a tile shows. */
function shownOf(tile) {
  switch (tile.type) {
    case 'switch': case 'mode': return [tile.status || tile.ga];
    case 'dimmer': return [tile.switch_status || tile.switch, tile.brightness_status || tile.brightness].filter(Boolean);
    default: return [];
  }
}

function level(tile) {
  const b = value(tile.brightness_status || tile.brightness);
  return typeof b?.value === 'number' ? b.value : null;
}

function lightOn(tile) {
  if (tile.type === 'switch') return isOn(value(tile.status || tile.ga)) === true;
  // Lit if the switch says so or there is any brightness: either may lag the other.
  const sw = tile.switch_status || tile.switch;
  const lvl = level(tile);
  return isOn(value(sw)) === true || (lvl !== null && lvl > 0);
}

function lightsOff(tiles) {
  for (const tile of tiles.filter(lightOn)) {
    if (tile.type === 'switch') write(tile.ga, '0', '1.001');
    else if (tile.switch) write(tile.switch, '0', '1.001');
    else write(tile.brightness, '0', '5.001');
  }
}

function busyUntilUpdate(el) {
  el.classList.add('busy');
  clearTimeout(el._busy);
  el._busy = setTimeout(() => el.classList.remove('busy'), 2500);
}

function flash(el) {
  el.classList.remove('flash');
  void el.offsetWidth;
  el.classList.add('flash');
}

/** The common compact tile: icon, label and a state line. */
function compact(tile, group, extra = '') {
  const ico = h('span', { class: 'ico' }, icon(iconFor(tile)));
  const state = h('span', { class: 'state' });
  const label = displayLabel(tile, group);
  const el = h('div', { class: `tile ${extra}`, title: tile.label },
    ico, h('div', { class: 'text' }, h('span', { class: 'label' }, label), state));
  return { el, ico, state };
}

/** Tap or Enter runs `action`, unless the tile is being edited. */
function pressable(el, action) {
  el.tabIndex = 0;
  el.setAttribute('role', 'button');
  el.addEventListener('click', (e) => {
    if (editing() || el._dragged) { el._dragged = false; return; }
    if (e.target.closest('button')) return;
    action();
  });
  el.addEventListener('keydown', (e) => {
    if (!editing() && (e.key === 'Enter' || e.key === ' ') && e.target === el) { e.preventDefault(); action(); }
  });
}

/**
 * Sideways drag across a tile sets a level: `preview` while dragging, `commit` on release.
 * Vertical movement is left to the browser, so the page still scrolls on touch screens.
 */
function draggable(el, preview, commit) {
  let start = null;
  let dragging = false;
  const levelAt = (x) => {
    const r = el.getBoundingClientRect();
    return clamp(Math.round(((x - r.left) / r.width) * 100), 0, 100);
  };
  el.addEventListener('pointerdown', (e) => {
    if (editing() || e.button > 0 || e.target.closest('button')) return;
    start = { x: e.clientX, y: e.clientY, id: e.pointerId };
    dragging = false;
  });
  el.addEventListener('pointermove', (e) => {
    if (!start || e.pointerId !== start.id) return;
    const dx = Math.abs(e.clientX - start.x);
    if (!dragging && dx > 8 && dx > Math.abs(e.clientY - start.y)) {
      dragging = true;
      el.setPointerCapture(e.pointerId);
      el.classList.add('dragging-level');
    }
    if (dragging) preview(levelAt(e.clientX));
  });
  const end = (e, ok) => {
    if (!start || e.pointerId !== start.id) return;
    start = null;
    if (!dragging) return;
    dragging = false;
    el._dragged = true; // the click that follows is not a tap
    setTimeout(() => { el._dragged = false; }, 0);
    el.classList.remove('dragging-level');
    if (ok) commit(levelAt(e.clientX));
  };
  el.addEventListener('pointerup', (e) => end(e, true));
  el.addEventListener('pointercancel', (e) => end(e, false));
  return () => dragging;
}

const BUILDERS = {
  switch(tile, group) {
    const { el, state } = compact(tile, group, 'tap');
    const shown = tile.status || tile.ga;
    pressable(el, async () => {
      const on = isOn(value(shown));
      busyUntilUpdate(el);
      await write(tile.ga, on ? '0' : '1', '1.001');
    });
    return {
      el, gas: [tile.ga, tile.status].filter(Boolean),
      update() {
        el.classList.remove('busy');
        const v = value(shown);
        const on = isOn(v);
        el.classList.toggle('on', on === true);
        el.classList.toggle('unknown', on === null);
        state.textContent = on === null ? '–' : vt(on ? 'on' : 'off');
      },
    };
  },

  dimmer(tile, group) {
    const { el, state } = compact(tile, group, 'tap dimmer');
    el.title = `${tile.label} · ${t('Drag sideways to dim')}`;
    const show = (on, lvl) => {
      // Switched on with no brightness reported yet: show it as fully on.
      const partial = on && lvl !== null && lvl > 0 && lvl < 100;
      el.style.setProperty('--level', `${on ? (partial ? lvl : 100) : 0}%`);
      state.textContent = partial ? `${num(lvl)} %` : vt(on ? 'on' : 'off');
    };
    const setLevel = async (lvl) => {
      busyUntilUpdate(el);
      if (lvl === 0 && tile.switch) await write(tile.switch, '0', '1.001');
      else await write(tile.brightness, String(lvl), '5.001');
    };
    const isDragging = draggable(el, (lvl) => { el.classList.add('on'); show(lvl > 0, lvl); }, setLevel);
    const toggle = async () => {
      const on = lightOn(tile);
      busyUntilUpdate(el);
      if (tile.switch) await write(tile.switch, on ? '0' : '1', '1.001');
      else await write(tile.brightness, on ? '0' : '100', '5.001');
    };
    pressable(el, toggle);
    el.addEventListener('keydown', (e) => {
      if (editing() || (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight')) return;
      e.preventDefault();
      setLevel(clamp(Math.round(((level(tile) ?? 0) + (e.key === 'ArrowRight' ? 10 : -10)) / 10) * 10, 0, 100));
    });
    return {
      el, gas: [tile.switch, tile.switch_status, tile.brightness, tile.brightness_status].filter(Boolean),
      update() {
        if (isDragging()) return;
        el.classList.remove('busy');
        const on = lightOn(tile);
        el.classList.toggle('on', on);
        show(on, level(tile));
      },
    };
  },

  value(tile, group) {
    const { el, ico, state } = compact(tile, group, 'reading');
    return {
      el, gas: [tile.ga],
      update() {
        const v = value(tile.ga);
        const f = formatReading(tile, v);
        ico.replaceChildren(icon(iconFor(tile, v)));
        const stale = v && Date.now() - v.time > STALE_MS;
        state.replaceChildren(...(f
          ? [f.main, f.unit ? h('small', {}, f.unit) : null, stale ? h('em', {}, ago(v.time)) : null]
          : ['–']).filter((c) => c !== null));
        el.classList.toggle('unknown', !f);
        el.classList.toggle('alert', attention(tile, v));
        el.title = `${tile.label}${v ? ` · ${ago(v.time)}` : ''}`;
      },
    };
  },

  setpoint(tile, group) {
    const step = Number(tile.step) || 0.5;
    const shown = tile.status || tile.ga;
    const big = h('div', { class: 'big' });
    const bigCaption = h('div', { class: 'caption' });
    const target = h('div', { class: 'target' });
    const heat = h('span', { class: 'heat' }, icon('flame'), h('span'));
    const bar = h('div', { class: 'heat-bar' }, h('span'));
    let pending = null;
    let timer = null;
    const deg = (n) => (n === null ? '–' : `${num(n, 1)}°`);
    const bump = (delta) => {
      if (editing()) return;
      const base = pending ?? value(shown)?.value;
      if (typeof base !== 'number') { toast(t('No target value yet')); return; }
      pending = Math.round((base + delta) / step) * step;
      el.classList.add('pending');
      update();
      clearTimeout(timer);
      timer = setTimeout(async () => {
        const ok = await write(tile.ga, pending.toFixed(2), '9.001');
        if (!ok) pending = null;
        setTimeout(() => { pending = null; el.classList.remove('pending'); update(); }, 1500);
      }, 700);
    };
    const stepper = h('div', { class: 'stepper' },
      h('button', { class: 'round', type: 'button', title: t('Lower'), 'aria-label': t('Lower'), onclick: () => bump(-step) }, icon('minus')),
      h('div', { class: 'target-box' }, target, h('div', { class: 'caption' }, t('Target'))),
      h('button', { class: 'round', type: 'button', title: t('Raise'), 'aria-label': t('Raise'), onclick: () => bump(step) }, icon('plus')));
    const el = h('div', { class: 'tile climate', title: tile.label },
      h('div', { class: 'climate-head' },
        h('span', { class: 'ico' }, icon('thermo')),
        h('span', { class: 'label' }, displayLabel(tile, group)),
        tile.heating ? heat : null),
      h('div', { class: 'climate-body' }, h('div', {}, big, bigCaption), stepper),
      tile.heating ? bar : null);
    function update() {
      const set = pending ?? (typeof value(shown)?.value === 'number' ? value(shown).value : null);
      const actual = value(tile.actual);
      if (tile.actual) {
        big.textContent = typeof actual?.value === 'number' ? deg(actual.value) : '–';
        bigCaption.textContent = t('Now');
        target.textContent = deg(set);
      } else {
        // Without a measured temperature the target is the headline.
        big.textContent = deg(set);
        bigCaption.textContent = t('Target');
        target.textContent = '';
      }
      stepper.classList.toggle('bare', !tile.actual);
      const hv = value(tile.heating);
      const pct = typeof hv?.value === 'number' ? clamp(Math.round(hv.value), 0, 100) : null;
      heat.lastChild.textContent = pct === null ? '–' : `${num(pct)} %`;
      heat.classList.toggle('active', (pct ?? 0) > 0);
      el.classList.toggle('heating', (pct ?? 0) > 0);
      bar.firstChild.style.width = `${pct ?? 0}%`;
      heat.title = `${t('Heating')} ${pct === null ? '–' : `${pct} %`}`;
    }
    return { el, gas: [tile.ga, tile.status, tile.actual, tile.heating].filter(Boolean), update };
  },

  scene(tile, group) {
    const { el, state } = compact(tile, group, 'tap');
    state.textContent = `${t('Scene')} ${tile.scene}`;
    pressable(el, async () => { if (await write(tile.ga, String(tile.scene), '17.001')) flash(el); });
    return { el, gas: [], update() {} };
  },

  blinds(tile, group) {
    const { el, state } = compact(tile, group, 'wide blinds');
    const move = (dir) => () => !editing() && write(tile.move, dir, '1.008');
    el.append(h('div', { class: 'controls' },
      tile.move ? h('button', { class: 'round', type: 'button', title: t('Up'), 'aria-label': t('Up'), onclick: move('0') }, icon('up')) : null,
      tile.stop ? h('button', { class: 'round', type: 'button', title: t('Stop'), 'aria-label': t('Stop'), onclick: () => !editing() && write(tile.stop, '1', '1.007') }, icon('stop')) : null,
      tile.move ? h('button', { class: 'round', type: 'button', title: t('Down'), 'aria-label': t('Down'), onclick: move('1') }, icon('down')) : null));
    const show = (p) => {
      el.style.setProperty('--level', `${p ?? 0}%`);
      state.textContent = p === null ? '' : p === 0 ? t('Open') : `${num(p)} % ${t('closed')}`;
    };
    const isDragging = tile.position
      ? draggable(el, show, (p) => write(tile.position, String(p), '5.001'))
      : () => false;
    return {
      el, gas: [tile.position, tile.position_status].filter(Boolean),
      update() {
        if (isDragging()) return;
        const v = value(tile.position_status || tile.position);
        show(typeof v?.value === 'number' ? Math.round(v.value) : null);
      },
    };
  },

  mode(tile, group) {
    const modes = [['auto', 'Auto'], ['comfort', 'Comfort'], ['standby', 'Standby'], ['economy', 'Eco'], ['building-protection', 'Protect']];
    const shown = tile.status || tile.ga;
    const btns = modes.map(([m, label]) => h('button', {
      class: 'seg', type: 'button', dataset: { mode: m },
      onclick: () => !editing() && write(tile.ga, m, '20.102'),
    }, t(label)));
    const el = h('div', { class: 'tile mode', title: tile.label },
      h('div', { class: 'climate-head' }, h('span', { class: 'ico' }, icon('flame')), h('span', { class: 'label' }, displayLabel(tile, group))),
      h('div', { class: 'segments' }, btns));
    return {
      el, gas: [tile.ga, tile.status].filter(Boolean),
      update() {
        const v = value(shown);
        for (const b of btns) b.classList.toggle('on', v?.value === b.dataset.mode);
      },
    };
  },

  button(tile, group) {
    const { el, state } = compact(tile, group, 'tap');
    state.textContent = `${t('Sends')} ${tile.value}`;
    pressable(el, async () => { if (await write(tile.ga, tile.value, tile.dpt)) flash(el); });
    return { el, gas: [], update() {} };
  },
};

function build(tile, group) {
  const builder = BUILDERS[tile.type] || BUILDERS.value;
  const built = builder(tile, group);
  built.tile = tile;
  built.gas = built.gas.map((g) => g.trim());
  return built;
}

// Ages of readings move on without new telegrams.
setInterval(() => {
  for (const item of S.tiles) if (item.tile?.type === 'value') item.update();
}, 30000);

// ---------------------------------------------------------------------------
// Edit mode
// ---------------------------------------------------------------------------

function startEditing() {
  S.draft = structuredClone(S.dashboard);
  if (S.draft.pages.length === 0) S.draft.pages.push({ id: newId(), name: 'Home', tiles: [] });
  $('#edit-bar').hidden = false;
  $('#edit-btn').classList.add('on');
  render();
}

function stopEditing() {
  S.draft = null;
  $('#edit-bar').hidden = true;
  $('#edit-btn').classList.remove('on');
  render();
}

$('#edit-btn').addEventListener('click', () => (editing() ? stopEditing() : startEditing()));
$('#cancel-edit').addEventListener('click', stopEditing);
$('#done-edit').addEventListener('click', async () => {
  const result = await request({ type: 'save', dashboard: S.draft });
  if (!result.ok) { toast(result.message, 'err'); return; }
  S.dashboard = S.draft;
  toast(t('Saved'));
  stopEditing();
});
$('#page-name').addEventListener('input', (e) => {
  const page = currentPage();
  if (page) { page.name = e.target.value; render(); e.target.focus(); }
});
$('#lang-select').addEventListener('change', (e) => {
  S.draft.language = e.target.value;
  render();
});
$('#title-input').addEventListener('input', (e) => {
  S.draft.title = e.target.value;
  $('#title').textContent = e.target.value || 'Home';
});
$('#add-page').addEventListener('click', () => {
  const page = { id: newId(), name: t('New page'), tiles: [] };
  S.draft.pages.push(page);
  S.page = page.id;
  render();
  $('#page-name').select();
});
$('#delete-page').addEventListener('click', () => {
  if (S.draft.pages.length < 2) { toast(t('A dashboard needs at least one page')); return; }
  const page = currentPage();
  S.draft.pages = S.draft.pages.filter((p) => p !== page);
  S.page = S.draft.pages[0].id;
  toast(`${t('Removed')} “${page.name}”. ${t('Cancel to undo.')}`);
  render();
});

/** Edit handles and drag-and-drop on a tile in edit mode. Dropping on a tile in another room moves it there. */
function decorate(el, tile, index) {
  el.append(h('div', { class: 'tile-tools' },
    h('button', { type: 'button', title: t('Edit'), onclick: (e) => { e.stopPropagation(); openTileDialog(tile); } }, icon('pencil')),
    h('button', { type: 'button', title: t('Remove'), onclick: (e) => { e.stopPropagation(); removeTile(tile); } }, icon('x'))));
  el.addEventListener('click', () => openTileDialog(tile));
  el.draggable = true;
  el.addEventListener('dragstart', (e) => { el.classList.add('dragging'); e.dataTransfer.setData('text/plain', String(index)); });
  el.addEventListener('dragend', () => el.classList.remove('dragging'));
  el.addEventListener('dragover', (e) => { e.preventDefault(); el.classList.add('drop-before'); });
  el.addEventListener('dragleave', () => el.classList.remove('drop-before'));
  el.addEventListener('drop', (e) => {
    e.preventDefault();
    el.classList.remove('drop-before');
    const from = Number(e.dataTransfer.getData('text/plain'));
    const tiles = currentPage().tiles;
    const [moved] = tiles.splice(from, 1);
    if (!moved) return;
    if (tile.group) moved.group = tile.group;
    else delete moved.group;
    tiles.splice(from < index ? index - 1 : index, 0, moved);
    renderPage();
  });
}

function removeTile(tile) {
  const page = currentPage();
  page.tiles = page.tiles.filter((x) => x !== tile);
  renderPage();
}

// ---------------------------------------------------------------------------
// Tile dialog
// ---------------------------------------------------------------------------

/** Fields per tile kind: [key, label, required] for addresses, or { key, label, kind } for others. */
const FIELDS = {
  switch: [['ga', 'Switch address', true], ['status', 'Status address (optional)']],
  dimmer: [['switch', 'On/off address (optional)'], ['switch_status', 'On/off status (optional)'], ['brightness', 'Brightness address (%)', true], ['brightness_status', 'Brightness status (optional)']],
  value: [['ga', 'Address', true]],
  setpoint: [['ga', 'Target temperature address', true], ['status', 'Target status (optional)'], ['actual', 'Measured temperature (optional)'], ['heating', 'Heating output, % (optional)'], { key: 'step', label: 'Step (°)', kind: 'number', value: 0.5 }],
  scene: [['ga', 'Scene address', true], { key: 'scene', label: 'Scene number (1–64)', kind: 'number', value: 1 }],
  blinds: [['move', 'Up/down address'], ['stop', 'Stop address (optional)'], ['position', 'Position address (optional)'], ['position_status', 'Position status (optional)']],
  mode: [['ga', 'Mode address', true], ['status', 'Mode status (optional)']],
  button: [['ga', 'Address', true], { key: 'value', label: 'Value to send', kind: 'text', value: '1' }, { key: 'dpt', label: 'DPT (optional, else from project)', kind: 'text', value: '' }],
};

let editingTile = null;

/** "Kök tak center bryt" → "Kök tak center". */
function baseName(name) {
  return (name || '').replace(/\s+(bryt|status värde|status|dim absolut|bör|switch|state|schalten|rückmeldung)$/i, '').trim();
}

function gaHint(input, hint) {
  const e = S.byAddr.get(input.value.trim());
  hint.classList.toggle('ok', Boolean(e));
  hint.textContent = e ? `${e.name}${e.dpt ? ` · DPT ${e.dpt}` : ''}` : (input.value && !isGa(input.value) ? t('Not a group address') : '');
}

function renderFields(type, from) {
  const box = $('#tile-fields');
  box.replaceChildren(...FIELDS[type].map((f) => {
    if (Array.isArray(f)) {
      const [key, label, required] = f;
      const input = h('input', { type: 'text', inputmode: 'decimal', list: 'ga-list', dataset: { key }, placeholder: t('e.g. 1/2/3'), required, autocomplete: 'off' });
      input.value = from?.[key] ?? '';
      const hint = h('span', { class: 'hint' });
      input.addEventListener('input', () => {
        gaHint(input, hint);
        const label = $('#tile-label');
        const entry = S.byAddr.get(input.value.trim());
        if (entry && !label.value) label.value = baseName(entry.name);
      });
      gaHint(input, hint);
      return h('label', { class: 'field' }, h('span', {}, t(label)), input, hint);
    }
    const input = h('input', { type: f.kind === 'number' ? 'number' : 'text', step: 'any', dataset: { key: f.key } });
    input.value = from?.[f.key] ?? f.value;
    return h('label', { class: 'field' }, h('span', {}, t(f.label)), input);
  }));
}

function openTileDialog(tile, group = null) {
  editingTile = tile;
  $('#tile-dialog-title').textContent = tile ? t('Edit tile') : t('Add tile');
  $('#tile-type').value = tile?.type || 'switch';
  $('#tile-label').value = tile?.label || '';
  $('#tile-group').value = tile ? tile.group || '' : group || '';
  $('#tile-delete').hidden = !tile;
  $('.dialog-move').hidden = !tile;
  renderFields($('#tile-type').value, tile);
  $('#tile-dialog').showModal();
}

$('#tile-type').addEventListener('change', (e) => {
  // Keep what was typed in fields both kinds share.
  const old = {};
  for (const input of $$('#tile-fields input')) old[input.dataset.key] = input.value;
  renderFields(e.target.value, old);
});

$('#tile-dialog').addEventListener('close', () => {
  if ($('#tile-dialog').returnValue !== 'ok') return;
  const type = $('#tile-type').value;
  const tile = { id: editingTile?.id || newId(), type, label: $('#tile-label').value.trim() };
  const group = $('#tile-group').value.trim();
  if (group) tile.group = group;
  for (const input of $$('#tile-fields input')) {
    const v = input.value.trim();
    const key = input.dataset.key;
    if (input.type === 'number') tile[key] = Number(v);
    else if (v) tile[key] = v;
  }
  if (type === 'scene') tile.scene = Math.min(64, Math.max(1, Math.round(tile.scene || 1)));
  const page = currentPage();
  if (editingTile) page.tiles[page.tiles.indexOf(editingTile)] = tile;
  else {
    // A new tile goes after the last one of its room.
    let at = -1;
    page.tiles.forEach((x, i) => { if ((x.group || '') === (tile.group || '')) at = i; });
    page.tiles.splice(at < 0 ? page.tiles.length : at + 1, 0, tile);
  }
  renderPage();
});

$('#tile-form').addEventListener('submit', (e) => {
  if (e.submitter?.value !== 'ok') return;
  for (const input of $$('#tile-fields input[list]')) {
    if (input.value.trim() && !isGa(input.value)) {
      e.preventDefault();
      input.focus();
      toast(`${input.value} ${t('is not a group address')}`, 'err');
      return;
    }
  }
  if (!$('#tile-label').value.trim()) {
    e.preventDefault();
    $('#tile-label').focus();
  }
});

$('#tile-delete').addEventListener('click', () => {
  removeTile(editingTile);
  $('#tile-dialog').close('cancel');
});

function moveTile(delta) {
  const tiles = currentPage().tiles;
  const i = tiles.indexOf(editingTile);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= tiles.length) return;
  [tiles[i], tiles[j]] = [tiles[j], tiles[i]];
  renderPage();
}
$('#move-left').addEventListener('click', () => moveTile(-1));
$('#move-right').addEventListener('click', () => moveTile(1));

// ---------------------------------------------------------------------------
// Suggestions from the ETS project
// ---------------------------------------------------------------------------

let suggested = [];
const KIND_NAMES = { switch: 'Switch', dimmer: 'Dimmer', value: 'Value', setpoint: 'Temperature', scene: 'Scene', blinds: 'Blinds', mode: 'Heating mode', button: 'Button' };
const kindName = (kind) => t(KIND_NAMES[kind] || kind);

$('#suggest-btn').addEventListener('click', () => {
  const select = $('#suggest-range');
  select.replaceChildren(...S.ranges.map((path, i) => h('option', { value: i }, path.join(' › '))));
  if (S.ranges.length === 0) { toast(t('No group ranges in the project (load knx.xml with --project)')); return; }
  $('#suggest-dialog').showModal();
  loadSuggestions();
});
$('#suggest-range').addEventListener('change', loadSuggestions);

let suggestSeq = 0;

async function loadSuggestions() {
  const path = S.ranges[Number($('#suggest-range').value)] || [];
  const list = $('#suggest-list');
  list.replaceChildren(h('li', { class: 'empty' }, t('Looking…')));
  // Only the latest request counts: an earlier, slower answer must not overwrite it.
  const seq = ++suggestSeq;
  const result = await request({ type: 'suggest', path });
  if (seq !== suggestSeq) return;
  suggested = (result.tiles || []).map((tile) => ({ ...tile, id: newId(), _keep: true }));
  if (suggested.length === 0) {
    list.replaceChildren(h('li', { class: 'empty' }, t('Nothing to suggest here.')));
    return;
  }
  list.replaceChildren(...suggested.map((tile) => {
    const box = h('input', { type: 'checkbox', checked: true, onchange: (e) => { tile._keep = e.target.checked; } });
    return h('li', {},
      h('label', { class: 'suggest-item' }, box, tile.group ? h('span', { class: 'room-tag' }, tile.group) : null, displayLabel(tile, tile.group)),
      h('span', { class: 'kind' }, kindName(tile.type)));
  }));
}

function takeSuggested() {
  return suggested.filter((tile) => tile._keep).map(({ _keep, ...tile }) => tile);
}

$('#suggest-add').addEventListener('click', () => {
  currentPage().tiles.push(...takeSuggested());
  $('#suggest-dialog').close();
  renderPage();
});
$('#suggest-new-page').addEventListener('click', () => {
  const path = S.ranges[Number($('#suggest-range').value)] || [];
  const page = { id: newId(), name: path[path.length - 1] || t('New page'), tiles: takeSuggested() };
  S.draft.pages.push(page);
  S.page = page.id;
  $('#suggest-dialog').close();
  render();
});

// ---------------------------------------------------------------------------

openSocket();
