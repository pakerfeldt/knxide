// knxide programmer: the page. It opens on the list of projects; inside a project it shows the
// house as things (lights, outlets, heating) wired to what controls them. Everything it knows
// comes from the server's JSON; what a thing is, is worked out in model.js.
'use strict';

const $ = (s, r = document) => r.querySelector(s), $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = s => String(s == null ? '' : s).replace(/[&<>"]/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;'}[c]));
const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;
const {modLabel, chLabel, pick} = KnxModel;

async function api(method, path, body, raw) {
  const init = {method, headers: {}};
  if (raw) init.body = body; else if (body !== undefined) { init.body = JSON.stringify(body); init.headers['Content-Type'] = 'application/json'; }
  const response = await fetch(path, init), text = await response.text();
  let data = null; try { data = text ? JSON.parse(text) : null; } catch (e) { /* not JSON */ }
  if (!response.ok) throw new Error((data && data.error) || `The server answered ${response.status}`);
  return data;
}

/* ---------- state ---------- */
const app = {view: 'start', projects: [], open: null, library: true, menu: null, dialog: null, opening: null, working: false};
let M = null, RAW = null, ENDS = [];
const st = {lens: 'rooms', room: '', sel: null, hover: null, review: false, ends: false, q: '', wide: false, page: {}, queue: null, job: null, busy: false};
const editors = new Map();      // device id -> what the server says of it, with parameter pages
try { st.wide = localStorage.getItem('knxide-wide') === '1'; } catch (e) { /* no storage */ }

const KIND = {dimlight: 'Dimmable light', light: 'Light', outlet: 'Outlet', heater: 'Heater', heating: 'Heating', fan: 'Fan', pump: 'Pump', door: 'Door', awning: 'Awning', scene: 'Scenes', load: 'Switched'};
const ICON = {
  light: '<circle cx="10" cy="8" r="4.2"/><path d="M8.2 14.3h3.6M8.8 16.6h2.4"/>',
  dimlight: '<circle cx="10" cy="9" r="3.6"/><path d="M8.4 14.8h3.2M8.9 17h2.2M10 2.2v1.6M4.4 4.6l1.1 1.1M15.6 4.6l-1.1 1.1"/>',
  outlet: '<circle cx="10" cy="10" r="6.6"/><circle cx="7.6" cy="10" r=".7"/><circle cx="12.4" cy="10" r=".7"/>',
  heater: '<path d="M5.5 3.5c1.6 2.2-1.6 3.4 0 5.6s-1.6 3.4 0 5.6M10 3.5c1.6 2.2-1.6 3.4 0 5.6s-1.6 3.4 0 5.6M14.5 3.5c1.6 2.2-1.6 3.4 0 5.6s-1.6 3.4 0 5.6"/>',
  heating: '<path d="M3 5h11a2.5 2.5 0 0 1 0 5H6a2.5 2.5 0 0 0 0 5h11"/>',
  fan: '<circle cx="10" cy="10" r="6.8"/><circle cx="10" cy="10" r="1.2"/><path d="M10 8.8V4.6M11 10.7l3.6 2.1M9 10.7l-3.6 2.1"/>',
  pump: '<path d="M10 3c3 3.8 4.8 6.1 4.8 8.6a4.8 4.8 0 0 1-9.6 0C5.2 9.1 7 6.8 10 3z"/>',
  door: '<rect x="3.5" y="4" width="13" height="12" rx="1"/><path d="M3.5 8h13M3.5 12h13"/>',
  awning: '<path d="M3 5h14l-2.2 6H5.2zM10 11v5"/>',
  scene: '<rect x="3.5" y="3.5" width="5" height="5" rx="1"/><rect x="11.5" y="3.5" width="5" height="5" rx="1"/><rect x="3.5" y="11.5" width="5" height="5" rx="1"/><rect x="11.5" y="11.5" width="5" height="5" rx="1"/>',
  group: '<circle cx="7.5" cy="10" r="4.5"/><circle cx="12.5" cy="10" r="4.5"/>',
  load: '<rect x="4" y="4" width="12" height="12" rx="2"/>'
};
const icon = t => `<span class="ico"><svg viewBox="0 0 20 20">${ICON[t.group ? 'group' : t.kind] || ICON.load}</svg></span>`;
const WARN = '<span class="wicon" title="Loose end"><svg viewBox="0 0 16 16"><path d="M8 2.2 14.3 13H1.7zM8 6.6v3.2M8 11.6v.2"/></svg></span>';

/* ---------- lookups ---------- */
const thingById = id => M.things.find(t => t.id === id);
const plateById = id => M.plates.find(p => p.id === id);
const moduleById = id => M.modules.find(m => m.id === id);
const deviceById = id => M.devices.get(id);
const tname = (t, room) => t.room === room ? t.short : t.name;
const kindLabel = t => t.group ? `Group of ${t.members.length}` : (KIND[t.kind] || 'Switched');
const edited = () => RAW.devices.filter(d => d.state === 'edited');
const changeCount = () => edited().reduce((n, d) => n + Math.max(1, d.edits || 0), 0);
function freeChannels() { return M.channels.filter(ch => !ch.off && !ch.linked.length && ['dimmer', 'switch'].includes(ch.m.device.kind) && ch.m.device.editable); }
function deviceMarks(d) {
  let h = '';
  if (d.attention) h += `<span class="mark warn"><i></i>${esc(d.attention)}</span>`;
  else if (d.state === 'edited') h += '<span class="mark"><i></i>Changed</span>';
  return h;
}

/* ---------- projects ---------- */
function ago(ms) {
  const d = Math.floor((Date.now() - ms) / 864e5);
  return d <= 0 ? 'today' : d === 1 ? 'yesterday' : d < 30 ? `${d} days ago` : new Date(ms).toLocaleDateString(undefined, {year: 'numeric', month: 'short', day: 'numeric'});
}
async function loadProjects() {
  const list = await api('GET', '/api/projects');
  app.projects = list.projects; app.open = list.open; app.library = list.library;
}
async function openProject(id) {
  if (app.opening) return;
  app.opening = id; app.menu = null; renderStart();
  try { await api('POST', `/api/projects/${id}/open`); app.open = id; await enter(); }
  catch (e) { toast(e.message); }
  finally { app.opening = null; render(); }
}
async function enter() {
  st.sel = null; st.review = false; st.ends = false; st.q = ''; st.queue = null; st.job = null;
  await refresh(true);
  app.view = 'project'; setHash(st.lens); connect(); render();
}
function goStart() { app.view = 'start'; app.menu = null; st.review = false; disconnect(); setHash('projects'); loadProjects().then(render, render); render(); }
function projectName() { const p = app.projects.find(p => p.id === app.open); return p ? p.name : (RAW && RAW.project.name) || 'Project'; }

async function refresh(first) {
  RAW = await api('GET', '/api/model');
  M = KnxModel.build(RAW); ENDS = M.ends; editors.clear();
  st.job = RAW.job || st.job; st.queue = RAW.queue || st.queue;
  const withContent = M.rooms.filter(r => M.things.some(t => t.room === r.name) || M.plates.some(p => p.room === r.name));
  M.rail = withContent;
  if (!withContent.some(r => r.name === st.room)) st.room = withContent[0] ? withContent[0].name : '';
  const s = st.sel;
  if (s && ((s.type === 'thing' && !thingById(s.id)) || (s.type === 'rocker' && !(plateById(s.plate) && plateById(s.plate).rockers[s.n - 1])) || (s.type === 'device' && !deviceById(s.id)) || (s.type === 'chan' && !chanOf(s)))) st.sel = null;
  $('#all-addresses').innerHTML = RAW.addresses.map(a => `<option value="${esc(a.address)}">${esc(a.name || '')}</option>`).join('');
  if (!first) render();
  const summary = {rooms: withContent.length, things: M.things.length, devices: RAW.devices.length, ends: ENDS.length, changes: changeCount()};
  const known = app.projects.find(p => p.id === app.open);
  if (known && app.library && JSON.stringify(known.summary) !== JSON.stringify(summary)) { known.summary = summary; api('PUT', `/api/projects/${app.open}`, {summary}).catch(() => {}); }
}
const chanOf = s => { const m = moduleById(s.dev); return m && m.channels.find(c => c.key === s.key); };

function projCard(p) {
  const s = p.summary, fact = (n, w) => `<span><b>${n}</b> ${w}${n === 1 ? '' : 's'}</span>`, busy = app.opening === p.id;
  return `<article class="proj${busy ? ' busy' : ''}" data-act="open" data-id="${esc(p.id)}">
    <div><h2>${esc(p.name)}</h2>${s ? `<div class="facts">${fact(s.rooms, 'room')}${fact(s.things, 'thing')}${fact(s.devices, 'device')}</div>` : ''}
      <div class="proj-state">${busy ? '<span>Opening</span>' : ''}${s && s.changes ? `<span class="tag go">${plural(s.changes, 'change')} to program</span>` : ''}${s && s.ends ? `<span class="tag warn"><span class="dot"></span>${plural(s.ends, 'loose end')}</span>` : ''}${p.opened_ms ? `<span>Opened ${ago(p.opened_ms)}</span>` : ''}</div></div>
    <div class="acts"><button class="btn go" data-act="open" data-id="${esc(p.id)}">Open</button><button class="btn" data-act="export" data-id="${esc(p.id)}">Export</button>
      <span class="more"><button class="btn icon" data-act="menu" data-id="${esc(p.id)}" aria-label="More for ${esc(p.name)}" aria-expanded="${app.menu === p.id}">···</button>
        ${app.menu === p.id ? `<span class="menu"><button data-act="rename" data-id="${esc(p.id)}">Rename</button><button data-act="copy" data-id="${esc(p.id)}">Make a copy</button><button class="danger" data-act="delete" data-id="${esc(p.id)}">${p.owned ? 'Delete' : 'Remove from list'}</button></span>` : ''}</span></div>
  </article>`;
}
function renderStart() {
  const ps = app.projects.slice().sort((a, b) => (b.opened_ms || b.created_ms || 0) - (a.opened_ms || a.created_ms || 0));
  $('#start').innerHTML = `<div class="start-in"><div class="wordmark">knxide</div>
    <div class="start-head"><h1>Projects</h1>${ps.length ? '<div class="acts"><button class="btn go" data-act="new">New project</button></div>' : ''}</div>
    ${ps.length ? `<div class="plist">${ps.map(projCard).join('')}</div>` : '<div class="none-yet"><p>No projects yet.</p><div class="acts"><button class="btn go" data-act="new">New project</button></div></div>'}</div>`;
}

/* ---------- top bar, rail ---------- */
function renderTop() {
  const n = ENDS.length, c = changeCount(), running = st.queue && st.queue.active;
  const prev = $('#q'), hadFocus = prev && document.activeElement === prev;
  $('#top').innerHTML = `
    ${app.library ? '<button class="back" data-act="start"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M10 3.5 5.5 8l4.5 4.5"/></svg>Projects</button>' : ''}
    <div class="brand"><b>${esc(projectName())}</b></div>
    <div class="seg" role="group" aria-label="View">
      <button data-act="lens" data-lens="rooms" aria-pressed="${st.lens === 'rooms'}">Rooms</button>
      <button data-act="lens" data-lens="cabinet" aria-pressed="${st.lens === 'cabinet'}">Cabinet</button>
    </div>
    <div class="find"><input id="q" type="search" placeholder="Find a light, a button or an address" autocomplete="off" value="${esc(st.q)}" aria-label="Find"><div id="results"></div></div>
    <span class="spacer"></span>
    ${n ? `<button class="pill warn" data-act="ends" aria-expanded="${st.ends}"><span class="dot"></span>${plural(n, 'loose end')}</button>` : ''}
    ${running ? '<button class="pill go" data-act="review">Programming</button>' : c ? `<button class="pill go" data-act="review">Review ${plural(c, 'change')}</button>` : ''}
    ${st.ends ? `<div class="pop">${ENDS.map((e, i) => `<button data-act="end" data-i="${i}"><b>${esc(e.title)}</b><span>${esc(e.text)}</span></button>`).join('')}</div>` : ''}`;
  if (hadFocus) { const q = $('#q'); q.focus(); q.setSelectionRange(q.value.length, q.value.length); }
  renderResults();
}
function search(q) {
  q = q.trim().toLowerCase(); if (q.length < 2) return [];
  const out = [];
  for (const t of M.things) if (t.name.toLowerCase().includes(q) || t.addrs.some(a => a.a.startsWith(q))) out.push({label: t.name, sub: `${kindLabel(t)} · ${t.room}`, act: `data-act="thing" data-thing="${esc(t.id)}" data-go="1"`});
  for (const d of RAW.devices) if ((d.name || '').toLowerCase().includes(q) || (d.address || '').startsWith(q) || (d.product || '').toLowerCase().includes(q)) out.push({label: d.name || d.product || d.address, sub: `${d.type === 'Other' ? 'Device' : d.type} · ${d.address || ''}`, act: `data-act="device" data-id="${esc(d.id)}"`});
  return out.slice(0, 10);
}
function renderResults() {
  const r = M ? search(st.q) : [], el = $('#results'); if (!el) return;
  el.innerHTML = r.length ? `<div class="results">${r.map(x => `<button ${x.act}><span>${esc(x.label)}</span><small>${esc(x.sub)}</small></button>`).join('')}</div>` : '';
}
function renderRail() {
  let h = '', wing = null; const wings = M.rail.some(r => r.wing);
  for (const r of M.rail) {
    const things = M.things.filter(t => t.room === r.name);
    if (r.wing !== wing) { wing = r.wing; if (wings) h += `<div class="lbl">${esc(wing || 'Elsewhere')}</div>`; }
    const warn = ENDS.some(e => e.t.room === r.name);
    h += `<button class="room" data-act="room" data-room="${esc(r.name)}" aria-current="${st.lens === 'rooms' && st.room === r.name}"><span>${esc(r.name)}</span><small>${warn ? '<span class="dot"></span>' : ''}${things.length}</small></button>`;
  }
  $('#rail').innerHTML = h;
}
function isSel(type, a, b) { const s = st.sel; if (!s || s.type !== type) return false; return type === 'thing' || type === 'device' ? s.id === a : type === 'rocker' ? s.plate === a && s.n === b : s.dev === a && s.key === b; }

/* ---------- rooms ---------- */
function ledHtml(r, side) {
  const l = r.leds.find(x => x.side === side); if (!l || !l.object.addrs.length) return '<span class="led"></span>';
  const waits = l.object.addrs.some(a => !M.addr.get(a).reportedBy.length);
  return `<span class="led ${waits ? 'wait' : 'on'}"></span>`;
}
function plateHead(p) { return `<div class="plate-h" data-act="device" data-id="${esc(p.id)}"><b>${esc(p.name)}</b><span>${deviceMarks(p.device)} <span class="mono">${esc(p.address)}</span></span></div>`; }
function signalsHtml(p) {
  const rows = p.rockers.map(r => { const t = r.thing, rk = `${p.id}:${r.n}`;
    return `<div class="rocker sig${isSel('rocker', p.id, r.n) ? ' sel' : ''}" role="button" tabindex="0" data-act="rocker" data-plate="${esc(p.id)}" data-n="${r.n}" data-rk="${esc(rk)}"${t ? ` data-thing="${esc(t.id)}"` : ''}>
      <span class="sig-n"><small>${esc(r.label)}</small><b${t ? '' : ' class="none"'}>${t ? esc(tname(t, p.room)) : 'Unused'}</b></span>
      <span class="nub${t ? ' wired' : ''}" data-rk="${esc(rk)}"${t ? ` data-thing="${esc(t.id)}"` : ''} aria-hidden="true"></span></div>`; }).join('');
  return `<section class="plate">${plateHead(p)}<div class="face flat"><div class="unit">${rows || '<div class="rocker sig"><span class="sig-n"><b class="none">Nothing linked</b></span></div>'}</div></div>${p.more ? `<p class="more">${p.more} more signals, unused</p>` : ''}</section>`;
}
function plateHtml(p) {
  if (p.generic) return signalsHtml(p);
  let n = 0, units = '';
  p.units.forEach((cnt, ui) => {
    let rows = '';
    for (let i = 0; i < cnt; i++) {
      const r = p.rockers[n++]; if (!r) break; const t = r.thing, rk = `${p.id}:${r.n}`;
      rows += `<div class="rocker${isSel('rocker', p.id, r.n) ? ' sel' : ''}" role="button" tabindex="0" data-act="rocker" data-plate="${esc(p.id)}" data-n="${r.n}" data-rk="${esc(rk)}"${t ? ` data-thing="${esc(t.id)}"` : ''} aria-label="Rocker ${r.n}">
        ${ledHtml(r, 'L')}<span class="rk-n${t ? '' : ' none'}">${t ? esc(t.short) + (t.room !== p.room ? `<small>${esc(t.room)}</small>` : '') : 'Unused'}</span>${ledHtml(r, 'R')}
        <span class="nub${t ? ' wired' : ''}" data-rk="${esc(rk)}"${t ? ` data-thing="${esc(t.id)}"` : ''} aria-hidden="true"></span></div>`;
    }
    units += (ui === 0 && p.display ? '<div class="display"><i></i><i></i><i></i></div>' : '') + `<div class="unit">${rows}</div>`;
  });
  const ht = p.temp;
  return `<section class="plate">${plateHead(p)}<div class="face">${units}</div>
    ${ht ? `<div class="sensor" data-thing="${esc(ht.id)}"><span>Room sensor</span><span class="nub fixed wired" data-thing="${esc(ht.id)}" data-rk="s:${esc(p.id)}" data-dash="1"></span></div>` : ''}</section>`;
}
function driverChips(t) {
  if (t.drivers.length) { const ch = t.drivers[0]; return `<span class="chip">${esc(modLabel(ch.m))} <span class="mono">${esc(ch.m.address)}</span> · ${esc(ch.key)}</span>` + (t.drivers.length > 1 ? `<span class="chip">+${t.drivers.length - 1}</span>` : ''); }
  if (t.group) return `<span class="chip">${plural(t.members.length, 'output')}</span>`;
  return '<span class="chip none">No output</span>';
}
function thingRow(t, room, far) {
  const others = [...new Set(t.controls.filter(x => x.p.room !== room).map(x => x.p.room))];
  const sub = far ? `${kindLabel(t)} · ${t.room}` : kindLabel(t) + (others.length ? ` · also from ${others.join(', ')}` : '');
  return `<div class="thing${t.unused ? ' faint' : ''}${isSel('thing', t.id) ? ' sel' : ''}" role="button" tabindex="0" data-act="thing" data-thing="${esc(t.id)}"><span class="pin"></span>${icon(t)}
    <span class="t-main"><span class="t-name">${esc(far ? t.name : t.short)}</span><span class="t-sub">${esc(sub)}</span></span>
    <span class="t-right">${ENDS.some(e => e.t === t) ? WARN : ''}${driverChips(t)}</span></div>`;
}
function stageRooms() {
  if (!M.rail.length) return `<div class="blank"><h1>${esc(projectName())}</h1><p>This project has no devices that control or drive anything yet.</p></div>`;
  const room = st.room, plates = M.plates.filter(p => p.room === room), own = M.things.filter(t => t.room === room);
  const ids = new Set(own), far = [];
  for (const p of plates) { for (const r of p.rockers) if (r.thing && !ids.has(r.thing) && !far.includes(r.thing)) far.push(r.thing); if (p.temp && !ids.has(p.temp) && !far.includes(p.temp)) far.push(p.temp); }
  own.sort((a, b) => (!!a.unused - !!b.unused) || ((a.kind === 'heating') - (b.kind === 'heating')));
  return `<h1>${esc(room)}</h1><p class="sub">${plural(own.length, 'thing')} · ${plural(plates.length, 'control')}</p>
    <div class="bench"><svg class="wires" aria-hidden="true"></svg>
      <div class="wall"><div class="lbl">${plates.length && plates.every(p => p.generic) ? 'Controls' : 'On the wall'}</div>${plates.map(plateHtml).join('') || '<p class="empty">No controls here.</p>'}</div>
      <div class="things"><div class="lbl">In the room</div>${own.map(t => thingRow(t, room)).join('') || '<p class="empty">Nothing here.</p>'}
        ${far.length ? `<div class="lbl">Elsewhere</div>${far.map(t => thingRow(t, room, true)).join('')}` : ''}</div>
    </div>`;
}

/* ---------- cabinet ---------- */
function cell(m, c) {
  const t = c.thing, g = !t && c.groups.length ? c.groups[0] : null;
  if (t) return `<button class="ch${isSel('thing', t.id) ? ' sel' : ''}" data-act="thing" data-thing="${esc(t.id)}"><span class="ch-k">${esc(c.key)}${ENDS.some(e => e.t === t) ? WARN : ''}</span><span class="ch-n">${esc(t.short)}</span><span class="ch-r">${esc(t.room)}</span></button>`;
  if (g) return `<button class="ch${isSel('thing', g.id) ? ' sel' : ''}" data-act="thing" data-thing="${esc(g.id)}"><span class="ch-k">${esc(c.key)}</span><span class="ch-n">${esc(g.name)}</span><span class="ch-r">Group</span></button>`;
  if (c.off) return `<div class="ch used"><span class="ch-k">${esc(c.key)}</span><span class="ch-n">Off</span></div>`;
  if (c.linked.length) return `<div class="ch used"><span class="ch-k">${esc(c.key)}</span><span class="ch-n">${esc((M.addr.get(c.linked[0].addrs[0]) || {}).name || 'In use')}</span></div>`;
  return `<button class="ch free${isSel('chan', m.id, c.key) ? ' sel' : ''}" data-act="chan" data-dev="${esc(m.id)}" data-key="${esc(c.key)}"><span class="ch-k">${esc(c.key)}</span><span class="ch-n">Free</span></button>`;
}
function stageCabinet() {
  if (!M.modules.length) return '<h1>Cabinet</h1><p class="sub">No devices yet.</p>';
  let h = '<h1>Cabinet</h1><p class="sub">Every output, and what it feeds</p>';
  for (const cab of [...new Set(M.modules.map(m => m.cabinet))]) {
    const ms = M.modules.filter(m => m.cabinet === cab);
    const free = ms.reduce((n, m) => n + m.channels.filter(c => !c.off && !c.linked.length).length, 0);
    h += `<section class="cab"><h2>${esc(cab)}<small>${plural(ms.length, 'device')}${free ? ` · ${plural(free, 'free output')}` : ''}</small></h2><div class="din">${ms.map(m => {
      const n = m.channels.length, cols = n > 8 ? Math.ceil(n / 2) : n;
      return `<div class="mod${n ? '' : ' plain'}${isSel('device', m.id) ? ' sel' : ''}" style="--cols:${cols || 1}"><div class="mod-h" data-act="device" data-id="${esc(m.id)}"><b>${esc(modLabel(m))}</b><span>${deviceMarks(m.device)} <span class="mono">${esc(m.address)}</span></span></div><div class="mod-p">${esc(m.product)}</div>${n ? `<div class="chs">${m.channels.map(c => cell(m, c)).join('')}</div>` : ''}</div>`;
    }).join('')}</div></section>`;
  }
  return h;
}
function renderStage() { $('#stage').innerHTML = st.lens === 'rooms' ? stageRooms() : stageCabinet(); drawWires(); }

function drawWires() {
  const b = $('.bench'); if (!b) return;
  const svg = $('.wires', b), R = b.getBoundingClientRect(); let h = '';
  $$('.nub[data-thing]', b).forEach(n => {
    const pin = $(`.thing[data-thing="${n.dataset.thing}"] .pin`, b); if (!pin || !n.offsetParent) return;
    const a = n.getBoundingClientRect(), z = pin.getBoundingClientRect();
    const x1 = a.left + a.width / 2 - R.left, y1 = a.top + a.height / 2 - R.top, x2 = z.left + z.width / 2 - R.left, y2 = z.top + z.height / 2 - R.top, dx = Math.max(30, (x2 - x1) * .5);
    h += `<path class="wire${n.dataset.dash ? ' dash' : ''}" data-thing="${n.dataset.thing}" data-rk="${n.dataset.rk}" d="M${x1.toFixed(1)} ${y1.toFixed(1)}C${(x1 + dx).toFixed(1)} ${y1.toFixed(1)} ${(x2 - dx).toFixed(1)} ${y2.toFixed(1)} ${x2.toFixed(1)} ${y2.toFixed(1)}"/>`;
  });
  svg.innerHTML = h; applyFocus();
}
function applyFocus() {
  const b = $('.bench'); if (!b) return;
  $$('.hl', b).forEach(e => e.classList.remove('hl'));
  let f = st.hover;
  if (!f && st.sel) { const s = st.sel; if (s.type === 'thing') f = {thing: s.id}; else if (s.type === 'rocker') { const r = plateById(s.plate).rockers[s.n - 1]; f = {rk: `${s.plate}:${s.n}`, thing: r.thing && r.thing.id}; } }
  let hit = [];
  if (f) {
    if (f.rk) { hit = $$(`[data-rk="${f.rk}"]`, b); if (f.thing) hit.push(...$$(`.thing[data-thing="${f.thing}"]`, b)); }
    else if (f.thing) hit = $$(`[data-thing="${f.thing}"]`, b);
  }
  hit.forEach(e => e.classList.add('hl'));
  b.classList.toggle('focus', !!st.hover && hit.length > 0);
}

/* ---------- inspector ---------- */
const headBtns = () => { const l = st.wide ? 'Narrow panel' : 'Widen panel'; return `<span class="hbtns"><button class="x widen" data-act="wide" aria-pressed="${st.wide}" aria-label="${l}" title="${l}"><svg viewBox="0 0 16 16"><path d="${st.wide ? 'M2 4l4 4-4 4M14 4l-4 4 4 4' : 'M6 4 2 8l4 4M10 4l4 4-4 4'}"/></svg></button><button class="x" data-act="close" aria-label="Close">×</button></span>`; };
function nodeBtn(title, sub, act) { return `<button class="node" ${act}><span><b>${esc(title)}</b>${sub ? `<small>${esc(sub)}</small>` : ''}</span><span class="go" aria-hidden="true">›</span></button>`; }
function needEditor(id) {
  if (editors.has(id)) return editors.get(id);
  editors.set(id, null);
  api('GET', `/api/devices/${encodeURIComponent(id)}`).then(e => { editors.set(id, e); renderInsp(); if (st.review) renderLayer(); }, () => editors.delete(id));
  return null;
}
function paramRow(d, it) {
  if (it.type === 'text') return it.text ? `<p class="set-text">${esc(it.text)}</p>` : '';
  const id = `p-${d.id}-${it.reference}`.replace(/[^\w-]/g, '-'), c = it.control || {type: 'fixed'}, off = d.editable ? '' : ' disabled';
  const data = `data-param="${esc(it.reference)}" data-dev="${esc(d.id)}"`;
  let ctl;
  if (c.type === 'choice') { const known = c.choices.some(x => x.value === it.value); ctl = `<select id="${id}" ${data}${off}>${known ? '' : `<option value="${esc(it.value)}" selected>${esc(it.value_text)}</option>`}${c.choices.map(x => `<option value="${esc(x.value)}"${x.value === it.value ? ' selected' : ''}>${esc(x.text)}</option>`).join('')}</select>`; }
  else if (c.type === 'number') ctl = `<input id="${id}" type="number" ${data}${off} value="${esc(it.value)}"${c.minimum != null ? ` min="${esc(c.minimum)}"` : ''}${c.maximum != null ? ` max="${esc(c.maximum)}"` : ''}>`;
  else if (c.type === 'text') ctl = `<input id="${id}" type="text" ${data}${off} value="${esc(it.value)}"${c.max_length ? ` maxlength="${esc(c.max_length)}"` : ''}>`;
  else ctl = `<span class="fixed">${esc(it.value_text)}</span>`;
  const changed = it.standing === 'edited';
  return `<div class="set${changed ? ' is-changed' : ''}"><label for="${id}">${esc(it.label)}${it.suffix ? ` (${esc(it.suffix)})` : ''}</label>${ctl}${changed ? `<button class="undo" data-act="undo-param" data-dev="${esc(d.id)}" data-ref="${esc(it.reference)}" title="Back to ${esc(it.baseline_text)}" aria-label="Back to ${esc(it.baseline_text)}">↺</button>` : '<span></span>'}</div>`;
}
function settingsHtml(d, pages) {
  const e = needEditor(d.id); if (!e || !e.pages) return '';
  const shown = e.pages.filter(pg => pages(pg) && pg.items.some(it => it.type !== 'text'));
  if (!shown.length) return '';
  return `<div class="sec"><div class="lbl">Settings</div>${shown.map(pg => (shown.length > 1 ? `<p class="set-text"><b>${esc(pg.title)}</b></p>` : '') + pg.items.map(it => paramRow(d, it)).join('')).join('')}</div>`;
}
const chanPages = key => pg => { const k = key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'); return new RegExp(`\\b(?:Channel|Kanal|Output|Ausgang|Sortie)\\s+${k}\\b|^O?${k}\\s*:`, 'i').test(pg.title); };
function addrRows(rows) { return rows.length ? `<details><summary class="lbl">Addresses</summary><div class="addr">${rows.map(r => `<span class="mono">${esc(r.a)}</span><span>${esc(r.n || '')}<small>${esc(r.s || '')}</small></span>`).join('')}</div></details>` : ''; }
function thingSelect(id, cur, label, filter, off) {
  let h = `<select id="${id}"${off ? ' disabled' : ''}><option value="">${esc(label)}</option>`;
  for (const r of M.rooms) { const ts = M.things.filter(t => t.room === r.name && filter(t)); if (ts.length) h += `<optgroup label="${esc(r.name)}">${ts.map(t => `<option value="${esc(t.id)}"${t === cur ? ' selected' : ''}>${esc(t.name)}</option>`).join('')}</optgroup>`; }
  return h + '</select>';
}
function assignSelect(t) { const fc = freeChannels(); return fc.length ? `<select id="assign-${esc(t.id)}" data-assign="${esc(t.id)}" aria-label="Give it an output"><option value="">Give it an output…</option>${fc.map(ch => `<option value="${esc(ch.m.id)}|${esc(ch.key)}">${esc(chLabel(ch))}</option>`).join('')}</select>` : ''; }
function reportObject(ch) { return ch.objs.find(o => o.state && o.size === 1 && !o.addrs.length); }
function listenObject(ch) { return ch.objs.find(o => o.takes && !o.sends && o.size === 1) || ch.objs.find(o => o.takes && o.size === 1); }
function endHtml(e, i) {
  const t = e.t; let fix = '';
  if (e.type === 'unreported' && t.drivers.length && t.drivers[0].m.device.editable && reportObject(t.drivers[0])) fix = `<button class="btn go" data-act="fix" data-i="${i}">Let ${esc(chLabel(t.drivers[0]))} report it</button>`;
  else if (e.type === 'unheard' && t.drivers.length && t.drivers[0].m.device.editable && listenObject(t.drivers[0])) fix = `<button class="btn go" data-act="fix" data-i="${i}">Let ${esc(chLabel(t.drivers[0]))} listen</button>`;
  else if (e.type === 'unheard' && !t.drivers.length) fix = assignSelect(t);
  else if (e.type === 'stray' && e.ch.m.device.editable) fix = '<button class="btn go" data-act="fix" data-i="' + i + '">Remove the link</button>';
  return `<div class="end"><b>${WARN}${esc(e.title)}</b><p>${esc(e.text)}</p>${fix}</div>`;
}
function rockerWhat(x) { const f = x.r.objs.filter(o => o.addrs.length).map(o => o.function).join(' '); return x.p.generic ? x.r.label : `Rocker ${x.r.n} · ${/dimm/i.test(f) ? 'switch and dim' : /scene/i.test(f) ? 'scene' : /operation/i.test(f) ? 'up, down, stop' : 'switch'}`; }
function inspThing(t) {
  const sens = M.plates.filter(p => p.temp === t), ends = ENDS.map((e, i) => [e, i]).filter(x => x[0].t === t), drv = t.drivers[0];
  const partOf = []; for (const ch of t.drivers) for (const g of ch.groups) if (!partOf.includes(g)) partOf.push(g);
  const leds = [...new Map(t.leds.map(l => [`${l.p.id}:${l.r.n}`, l])).values()];
  return `<header><div><h2>${esc(t.name)}</h2><p>${esc(kindLabel(t))} · ${esc(t.room)}</p></div>${headBtns()}</header>
    ${ends.map(x => endHtml(x[0], x[1])).join('')}
    <div class="sec"><div class="lbl">Controlled by</div>
      ${t.controls.map(x => nodeBtn(x.p.name, rockerWhat(x), `data-act="rocker" data-plate="${esc(x.p.id)}" data-n="${x.r.n}"`)).join('')}
      ${sens.map(p => nodeBtn(p.name, 'Room sensor', `data-act="device" data-id="${esc(p.id)}"`)).join('')}
      ${t.controls.length + sens.length ? '' : '<p class="empty">No button.</p>'}</div>
    <div class="sec"><div class="lbl">${t.group ? 'Switches together' : 'Driven by'}</div>
      ${t.drivers.map(ch => nodeBtn(`${modLabel(ch.m)} ${ch.m.address}`, `Output ${ch.key} · ${ch.m.cabinet}`, `data-act="device" data-id="${esc(ch.m.id)}"`)).join('')}
      ${t.members.map(ch => ch.thing ? nodeBtn(ch.thing.name, chLabel(ch), `data-act="thing" data-thing="${esc(ch.thing.id)}" data-go="1"`) : nodeBtn(chLabel(ch), ch.m.cabinet, `data-act="device" data-id="${esc(ch.m.id)}"`)).join('')}
      ${t.drivers.length + t.members.length ? '' : '<p class="empty">No output.</p>'}
      ${!t.drivers.length && !t.group && !ends.some(x => x[0].type === 'unheard') && t.kind !== 'scene' ? assignSelect(t) : ''}</div>
    ${partOf.length ? `<div class="sec"><div class="lbl">Part of</div>${partOf.map(g => nodeBtn(g.name, kindLabel(g), `data-act="thing" data-thing="${esc(g.id)}" data-go="1"`)).join('')}</div>` : ''}
    ${leds.length ? `<div class="sec"><div class="lbl">Shown on</div>${leds.map(x => nodeBtn(x.p.name, `LED of rocker ${x.r.n}`, `data-act="rocker" data-plate="${esc(x.p.id)}" data-n="${x.r.n}"`)).join('')}</div>` : ''}
    ${drv ? settingsHtml(drv.m.device, chanPages(drv.key)) : ''}
    ${addrRows(t.addrs.map(A => ({a: A.a, n: A.name, s: A.dpt.replace(/^DPS?T-/, 'DPT ').replace('-', '.')})))}`;
}
function rockerSay(p, r) {
  const t = r.thing; if (!t) return 'Does nothing yet.';
  if (p.generic) return `Sends to <b>${esc(t.name)}</b>.`;
  const f = r.objs.filter(o => o.addrs.length).map(o => o.function).join(' '), n = `<b>${esc(t.name)}</b>`;
  let s = /scene/i.test(f) ? 'Recalls a scene.' : /dimm/i.test(f) ? `Switches and dims ${n}.` : /operation/i.test(f) ? `Runs ${n} out and in. A short press stops it.` : `Switches ${n}.`;
  const led = r.leds.find(l => l.object.addrs.length);
  if (led) { const lt = M.thingOf.get(led.object.addrs[0]), waits = !M.addr.get(led.object.addrs[0]).reportedBy.length; s += !lt || lt === t ? (waits ? ' The LED is meant to show whether it is on.' : ' The LED shows whether it is on.') : ` The LED shows <b>${esc(lt.name)}</b>.`; }
  return s;
}
function inspRocker(p, r) {
  const d = p.device, rows = [];
  for (const o of [...r.objs, ...r.leds.map(l => l.object)]) for (const a of o.addrs) rows.push({a, n: (M.addr.get(a) || {}).name, s: `${o.name} · ${o.function}`});
  const ids = new Set(r.pages.map(pg => pg.id));
  return `<header><div><h2>${esc(r.label)}</h2><p>${esc(p.name)} · ${esc(p.room)}</p></div>${headBtns()}</header>
    <p class="say">${rockerSay(p, r)}</p>
    ${d.editable ? '' : `<p class="note">${esc(d.reason || 'This device cannot be changed here.')}</p>`}
    <div class="field"><label class="lbl" for="bind">Controls</label>${thingSelect('bind', r.thing, 'Nothing', t => t.kind !== 'heating' || t === r.thing, !d.editable || !r.objs.length)}</div>
    ${r.thing ? `<div class="sec">${nodeBtn(r.thing.name, `${kindLabel(r.thing)} · ${r.thing.room}`, `data-act="thing" data-thing="${esc(r.thing.id)}" data-go="1"`)}</div>` : ''}
    ${ids.size ? settingsHtml(d, pg => ids.has(pg.id)) : ''}
    ${addrRows(rows)}
    <div class="sec">${nodeBtn(p.name, `${d.product || ''} · ${p.address}`, `data-act="device" data-id="${esc(p.id)}"`)}</div>`;
}
function inspChan(m, c) {
  return `<header><div><h2>Output ${esc(c.key)}</h2><p>${esc(modLabel(m))} <span class="mono">${esc(m.address)}</span> · ${esc(m.cabinet)}</p></div>${headBtns()}</header>
    <p class="say">Free.</p>
    ${m.device.editable ? `<div class="field"><label class="lbl" for="feed">Use it for</label>${thingSelect('feed', null, 'Choose a thing', t => !t.drivers.length && !t.group && t.kind !== 'scene')}</div>` : `<p class="note">${esc(m.device.reason || 'This device cannot be changed here.')}</p>`}
    ${settingsHtml(m.device, chanPages(c.key))}`;
}
function findingsHtml(d, e) {
  const fs = e.findings; if (!fs || !fs.length) return '';
  const row = f => f.type === 'parameter' ? `<div class="find-row"><b>${esc(f.page ? `${f.page} · ` : '')}${esc(f.label)}</b><span>Here</span><span>${esc(f.here)}</span><span>Device</span><span>${esc(f.device)}</span></div>`
    : f.type === 'links' ? `<div class="find-row"><b>${esc(f.name)}</b><span>Here</span><span>${esc((f.here || []).map(a => a.address || a).join(', ') || 'none')}</span><span>Device</span><span>${esc((f.device || []).map(a => a.address || a).join(', ') || 'none')}</span></div>`
    : `<div class="find-row"><b>${esc(f.what || 'Memory')}</b><span>Here</span><span class="mono">${esc(f.here)}</span><span>Device</span><span class="mono">${esc(f.device)}</span></div>`;
  return `<div class="sec"><div class="lbl">The device differs</div>${fs.map(row).join('')}${fs.some(f => f.adoptable) && d.editable ? `<button class="btn" data-act="adopt" data-id="${esc(d.id)}">Take over what the device has</button>` : ''}</div>`;
}
// What the last operation on the device came to, in a sentence.
function lastHtml(d) {
  const j = d.last; if (!j || j.status === 'running') return '';
  const r = j.result || {}, cap = s => s ? s[0].toUpperCase() + s.slice(1) : '';
  if (j.kind === 'compare') {
    if (r.not_loaded) return `<p class="note warn">${esc(cap(r.not_loaded))}</p>`;
    if (j.status === 'succeeded') return r.differing ? '' : '<p class="note">The device holds what is here.</p>';
    if (j.status === 'incomplete') return `<p class="note warn">${esc(cap(r.stopped_because || 'The comparison could not be finished'))}.${r.planned ? ` ${r.compared || 0} of ${r.planned} bytes were compared${r.differing ? '' : ' and matched'}.` : ''}</p>`;
    if (j.status === 'cancelled') return '';
    return `<p class="note warn">${esc(cap(j.error || 'The comparison failed'))}</p>`;
  }
  if (j.status === 'failed' || j.status === 'cancelled') return `<p class="note warn">${esc(cap(j.error || (j.status === 'cancelled' ? 'Programming was stopped' : 'Programming failed')))}${j.written ? ` ${esc(cap(j.written))}` : ''}</p>`;
  return '';
}
function inspDevice(d) {
  const e = needEditor(d.id), job = st.job && st.job.device === d.id && st.job.status === 'running' ? st.job : null;
  const bus = RAW.bus.mode !== 'none';
  let body = '';
  if (e && e.pages && e.pages.length) {
    const cur = e.pages.find(pg => pg.id === st.page[d.id]) || e.pages[0];
    body += `<div class="sec"><div class="lbl">Settings</div><select class="pagesel" id="pagesel" data-pagesel="${esc(d.id)}" aria-label="Page">${e.pages.map(pg => `<option value="${esc(pg.id)}"${pg === cur ? ' selected' : ''}>${esc(pg.title.replace(/\s+/g, ' '))}</option>`).join('')}</select>${cur.items.map(it => paramRow(d, it)).join('') || '<p class="empty">Nothing to set on this page.</p>'}</div>`;
  }
  if (e && e.objects && e.objects.length) {
    body += `<details${st.objs ? ' open' : ''} data-keep="objs"><summary class="lbl">Group objects</summary>${e.objects.map(o => `<div class="obj"><div class="obj-h"><b>${esc(o.name)}${o.function ? ` · ${esc(o.function)}` : ''}</b><span>${o.size_bits ? `${o.size_bits} bit` : ''}</span></div>
      <div class="links">${o.links.map(l => `<span class="lk"><span><span class="mono">${esc(l.address)}</span> ${esc(l.name || '')}</span>${d.editable ? `<button data-act="unlink" data-dev="${esc(d.id)}" data-ref="${esc(o.reference)}" data-a="${esc(l.address)}" aria-label="Remove ${esc(l.address)}">×</button>` : ''}</span>`).join('')}
      ${d.editable ? `<input class="addlink" list="all-addresses" data-addlink="${esc(o.reference)}" data-dev="${esc(d.id)}" placeholder="Add address" aria-label="Add address to ${esc(o.name)}">` : ''}</div></div>`).join('')}</details>`;
  }
  const app_ = d.application;
  return `<header><div><h2>${esc(d.name || d.product || d.address)}</h2><p>${esc(d.type === 'Other' ? 'Device' : d.type)} · <span class="mono">${esc(d.address || '')}</span>${(d.location || []).length ? ` · ${esc(d.location[d.location.length - 1])}` : ''}</p></div>${headBtns()}</header>
    ${d.attention ? `<p class="note warn">${esc(d.attention)}</p>` : ''}
    ${d.pending_note ? `<p class="note">${esc(d.pending_note)}</p>` : ''}
    ${d.editable ? '' : `<p class="note">${esc(d.reason || 'This device cannot be changed here.')}</p>`}
    ${d.editable && d.download_blocked ? `<p class="note">${esc(d.download_blocked)}</p>` : ''}
    ${job ? `<div class="sec"><div class="lbl">${job.kind === 'compare' ? 'Comparing' : 'Programming'}</div><div class="bar"><i style="width:${job.expected ? Math.min(100, Math.round(100 * job.sent / job.expected)) : 5}%"></i></div><button class="btn" data-act="cancel-job">Stop</button></div>`
      : bus && d.comparable ? `<div class="sec"><button class="btn" data-act="compare" data-id="${esc(d.id)}">Compare with device</button></div>` : ''}
    ${job ? '' : lastHtml(d)}
    ${e ? findingsHtml(d, e) : ''}
    ${body}
    <details${st.info ? ' open' : ''} data-keep="info"><summary class="lbl">About</summary><div class="addr">
      <span>Product</span><span>${esc(d.product || '')}<small>${esc(d.order_number || '')}</small></span>
      ${app_ ? `<span>Program</span><span>${esc(app_.name)}<small>Version ${esc(app_.version)} · mask ${esc(app_.mask)}</small></span>` : ''}
      ${d.downloaded_ms ? `<span>Programmed</span><span>${esc(new Date(d.downloaded_ms).toLocaleString())}</span>` : ''}</div></details>`;
}
function renderInsp() {
  if (app.view !== 'project' || !M) return;
  const main = $('#main'); let el = $('#insp'); const s = st.sel;
  if (!s) { if (el) el.remove(); main.classList.remove('has-insp', 'wide'); return; }
  if (!el) { el = document.createElement('aside'); el.id = 'insp'; el.className = 'insp'; main.appendChild(el); }
  main.classList.add('has-insp'); main.classList.toggle('wide', st.wide); el.classList.toggle('wide', st.wide);
  const top = el.scrollTop, active = document.activeElement && el.contains(document.activeElement) ? document.activeElement.id : null;
  el.innerHTML = s.type === 'thing' ? inspThing(thingById(s.id)) : s.type === 'rocker' ? inspRocker(plateById(s.plate), plateById(s.plate).rockers[s.n - 1]) : s.type === 'device' ? inspDevice(deviceById(s.id)) : inspChan(moduleById(s.dev), chanOf(s));
  el.scrollTop = top; if (active) { const again = document.getElementById(active); if (again) again.focus(); }
}

/* ---------- review and programming ---------- */
function changeLines(e) {
  const c = e.changes || {parameters: [], links: []}, out = [];
  for (const p of c.parameters || []) out.push(`<li>${esc(p.page ? `${String(p.page).replace(/\s+/g, ' ')} · ` : '')}${esc(p.label)}: <b>${esc(p.to)}</b><small>Was ${esc(p.from)}</small></li>`);
  for (const l of c.links || []) { const now = (l.now || []).map(a => a.name || a.address).join(', '), gone = (l.removed || []).map(a => a.name || a.address).join(', ');
    const o = (e.objects || []).concat(e.unused || []).find(o => o.reference === l.reference), what = o && o.function && o.function !== l.name ? `${l.name} · ${o.function}` : l.name;
    out.push(`<li>${esc(what)}: <b>${esc(now || 'no address')}</b>${gone ? `<small>Was ${esc(gone)}</small>` : ''}</li>`); }
  for (const o of c.dropped || []) out.push(`<li>${esc(o.name)}${o.function ? ` · ${esc(o.function)}` : ''} is no longer in use</li>`);
  for (const o of c.gained || []) out.push(`<li>${esc(o.name)}${o.function ? ` · ${esc(o.function)}` : ''} comes into use</li>`);
  return out.join('');
}
function reviewHtml() {
  const q = st.queue, running = q && q.active, devs = edited();
  let list = '', can = [];
  if (q && (running || st.queueSeen !== q.id)) {
    list = q.entries.map(en => {
      const job = st.job && st.job.device === en.device && en.status === 'running' ? st.job : null;
      const pct = job && job.expected ? Math.min(100, Math.round(100 * job.sent / job.expected)) : 0;
      const status = {waiting: 'Waiting', running: job && job.phase ? job.phase : 'Writing', done: 'Done', failed: 'Failed', pending: 'Not programmed'}[en.status] || en.status;
      return `<div class="rev-dev ${esc(en.status)}"><header><b>${esc(en.name)}</b><span class="status">${esc(status)}</span></header>${en.status === 'running' ? `<div class="bar"><i style="width:${pct}%"></i></div>` : ''}${en.error ? `<p class="note warn">${esc(en.error)}</p>` : ''}</div>`;
    }).join('');
    return `<header style="display:flex;justify-content:space-between;align-items:start;gap:10px"><h2>${running ? 'Programming' : 'Programmed'}</h2>${running ? '' : '<button class="x" data-act="review-close" aria-label="Close">×</button>'}</header>
      <div>${list}</div>${running ? '<button class="btn" data-act="cancel-job">Stop</button>' : '<button class="btn go" data-act="queue-done">Done</button>'}`;
  }
  for (const d of devs) {
    const e = needEditor(d.id), blocked = !d.supported;
    if (!blocked) can.push(d.id);
    list += `<div class="rev-dev"><header><b>${esc(d.name || d.product)} <span class="mono">${esc(d.address || '')}</span></b><button class="btn" data-act="discard" data-id="${esc(d.id)}">Discard</button></header>
      <ul>${e ? changeLines(e) : ''}${d.pending_note ? `<li>${esc(d.pending_note)}</li>` : ''}</ul>${blocked ? `<p class="note">${esc(d.download_blocked || d.reason || 'This device cannot be programmed here.')}</p>` : ''}</div>`;
  }
  const bus = RAW.bus.mode !== 'none', n = changeCount();
  return `<header style="display:flex;justify-content:space-between;align-items:start;gap:10px"><h2>${plural(n, 'change')}</h2><button class="x" data-act="review-close" aria-label="Close">×</button></header>
    <div>${list || '<p class="empty">Nothing to program.</p>'}</div>
    ${RAW.bus.warning ? `<p class="note warn">${esc(RAW.bus.warning)}</p>` : ''}
    ${!bus ? '<p class="note">Not connected to a bus.</p>' : can.length ? `<button class="btn go" data-act="program" data-ids="${esc(can.join(','))}">Program ${plural(can.length, 'device')}</button>` : ''}`;
}
function dialogHtml(d) {
  const p = d.id ? app.projects.find(x => x.id === d.id) : null;
  if (d.type === 'new') return `<div class="scrim center" data-act="scrim"><form class="modal${app.working ? ' busy' : ''}" data-form="new" role="dialog" aria-modal="true" aria-label="New project">
    <h2>New project</h2>
    <fieldset class="from"><legend class="lbl">Start from</legend>
      <label class="opt"><input type="radio" name="from" id="from-file" value="file" checked><span><b>An ETS project file</b><input class="file" type="file" id="pfile" accept=".knxproj" aria-label="Project file"></span></label>
      ${app.projects.length ? `<label class="opt"><input type="radio" name="from" id="from-copy" value="copy"><span><b>A copy of</b><select id="from-src" aria-label="Project to copy">${app.projects.map(x => `<option value="${esc(x.id)}">${esc(x.name)}</option>`).join('')}</select></span></label>` : ''}
    </fieldset>
    <div class="field"><label class="lbl" for="pname">Name</label><input id="pname" maxlength="80" autocomplete="off" placeholder="The project's own name"></div>
    <p class="note warn" id="derr" hidden></p>
    <div class="modal-acts"><button type="button" class="btn" data-act="dialog-close">Cancel</button><button class="btn go">Create</button></div></form></div>`;
  if (d.type === 'rename' || d.type === 'copy') return `<div class="scrim center" data-act="scrim"><form class="modal" data-form="${d.type}" role="dialog" aria-modal="true">
    <h2>${d.type === 'rename' ? 'Rename project' : 'Make a copy'}</h2>
    <div class="field"><label class="lbl" for="pname">Name</label><input id="pname" required maxlength="80" autocomplete="off" value="${esc(d.type === 'rename' ? p.name : `${p.name} copy`)}"></div>
    <p class="note warn" id="derr" hidden></p>
    <div class="modal-acts"><button type="button" class="btn" data-act="dialog-close">Cancel</button><button class="btn go">${d.type === 'rename' ? 'Rename' : 'Copy'}</button></div></form></div>`;
  return `<div class="scrim center" data-act="scrim"><div class="modal" role="dialog" aria-modal="true" aria-label="Delete project">
    <h2>${p.owned ? `Delete ${esc(p.name)}?` : `Remove ${esc(p.name)} from the list?`}</h2><p>${p.owned ? 'The project and its changes are deleted. This cannot be undone. Export it first if you want to keep a copy.' : 'The project file stays where it is.'}</p>
    <div class="modal-acts"><button class="btn" data-act="dialog-close">Cancel</button><button class="btn danger" data-act="delete-yes" data-id="${esc(p.id)}">${p.owned ? 'Delete' : 'Remove'}</button></div></div></div>`;
}
function renderLayer() {
  let h = '';
  if (app.dialog) h = dialogHtml(app.dialog);
  else if (st.review && app.view === 'project') h = `<div class="scrim" data-act="scrim"><div class="drawer" role="dialog" aria-label="Changes">${reviewHtml()}</div></div>`;
  // A dialog that is up stays as it is: what was typed and the chosen file are in it.
  const layer = $('#layer');
  if (app.dialog && app.dialog.shown) return;
  layer.innerHTML = h;
  if (app.dialog) { app.dialog.shown = true; const i = $('#pname'); if (i && app.dialog.type !== 'new') { i.focus(); i.select(); } }
}
function render() {
  const start = app.view === 'start' || !M;
  $('#start').hidden = !start; $('#app').hidden = start;
  if (start) { renderStart(); renderLayer(); return; }
  renderTop(); renderRail(); renderInsp(); renderStage(); renderLayer();
}

/* ---------- edits ---------- */
async function act(work) {
  if (st.busy) return; st.busy = true; document.body.classList.add('working');
  try { await work(); await refresh(); } catch (e) { toast(e.message); try { await refresh(); } catch (x) { /* shown already */ } }
  finally { st.busy = false; document.body.classList.remove('working'); }
}
const putLinks = (dev, ref, addrs) => api('PUT', `/api/devices/${encodeURIComponent(dev)}/objects/${encodeURIComponent(ref)}`, {links: addrs});
const same = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
// Point every group object of a rocker at the thing: each at the address of its own size and
// direction, the LEDs at the thing's state. Without a thing, they are all let go.
async function rebind(p, r, t) {
  for (const o of r.objs) { const A = t && pick(t, o.size, false), want = A ? [A.a] : []; if (!same(o.addrs, want)) await putLinks(p.id, o.reference, want); }
  for (const l of r.leds) { const o = l.object, A = t && pick(t, o.size, true), want = A ? [A.a] : []; if (!same(o.addrs, want)) await putLinks(p.id, o.reference, want); }
}
async function assignDriver(t, ch) {
  for (const o of ch.objs) { const A = o.takes ? pick(t, o.size, false) : o.state ? pick(t, o.size, true) : null; if (A && !o.addrs.includes(A.a)) await putLinks(ch.m.id, o.reference, [...o.addrs, A.a]); }
}
async function fix(e) {
  const t = e.t;
  if (e.type === 'stray') { const o = e.s.object; await putLinks(e.ch.m.id, o.reference, o.addrs.filter(a => a !== e.s.a)); }
  else if (e.type === 'unreported') { const ch = t.drivers[0], o = reportObject(ch); await putLinks(ch.m.id, o.reference, [e.leds[0].a]); }
  else if (e.type === 'unheard') { const ch = t.drivers[0], o = listenObject(ch), A = t.addrs.find(A => !A.state && A.size === 1); if (o && A) await putLinks(ch.m.id, o.reference, [...o.addrs, A.a]); }
}

/* ---------- behaviour ---------- */
function go(sel) {
  st.sel = sel;
  if (sel.type === 'thing') { const t = thingById(sel.id); if (st.lens === 'rooms') { const here = M.plates.some(p => p.room === st.room && (p.temp === t || p.rockers.some(r => r.thing === t))); if (t.room !== st.room && !here) st.room = t.room; } }
  if (sel.type === 'rocker') { st.lens = 'rooms'; st.room = plateById(sel.plate).room; }
}
function toast(msg) { const el = $('#toast'); el.innerHTML = `<div class="toast" role="status">${esc(msg)}</div>`; clearTimeout(toast.t); toast.t = setTimeout(() => { el.innerHTML = ''; }, 4200); }
function setHash(h) { try { history.replaceState(null, '', '#' + h); } catch (e) { /* not allowed */ } }
function setLens(l) { st.lens = l; setHash(l); }
function dialogState(error) {
  const form = $('.modal'), note = $('#derr'); if (!form) return;
  form.classList.toggle('busy', app.working);
  if (note) { note.textContent = error || ''; note.hidden = !error; }
  const button = $('.modal .btn.go'); if (button && app.dialog && app.dialog.type === 'new') button.textContent = app.working ? 'Reading the project' : 'Create';
}

document.addEventListener('click', e => {
  const a = e.target.closest('[data-act]');
  if (!a) {
    if (app.menu && !e.target.closest('.more')) { app.menu = null; render(); }
    else if (M && st.ends && !e.target.closest('.pop')) { st.ends = false; renderTop(); }
    else if (M && st.q && !e.target.closest('.find')) { st.q = ''; renderTop(); }
    return;
  }
  const d = a.dataset;
  if (d.act !== 'menu') app.menu = null;
  switch (d.act) {
    /* projects */
    case 'open': openProject(d.id); return;
    case 'start': goStart(); return;
    case 'new': app.dialog = {type: 'new'}; break;
    case 'rename': case 'copy': case 'delete': app.dialog = {type: d.act, id: d.id}; break;
    case 'delete-yes': api('DELETE', `/api/projects/${d.id}`).then(list => { app.projects = list.projects; app.open = list.open; app.dialog = null; render(); }, err => { app.dialog = null; render(); toast(err.message); }); return;
    case 'dialog-close': if (app.working) return; app.dialog = null; break;
    case 'menu': app.menu = app.menu === d.id ? null : d.id; break;
    case 'export': window.location.assign(`/api/projects/${encodeURIComponent(d.id)}/export`); break;
    /* inside a project */
    case 'lens': setLens(d.lens); st.q = ''; break;
    case 'room': st.room = d.room; setLens('rooms'); if (st.sel && st.sel.type !== 'thing') st.sel = null; break;
    case 'thing': if (d.go) { setLens('rooms'); st.room = thingById(d.thing).room; st.sel = {type: 'thing', id: d.thing}; st.q = ''; } else go({type: 'thing', id: d.thing}); break;
    case 'rocker': go({type: 'rocker', plate: d.plate, n: +d.n}); setLens('rooms'); st.q = ''; break;
    case 'device': st.sel = {type: 'device', id: d.id}; st.q = ''; break;
    case 'chan': st.sel = {type: 'chan', dev: d.dev, key: d.key}; break;
    case 'close': st.sel = null; break;
    case 'wide': st.wide = !st.wide; try { localStorage.setItem('knxide-wide', st.wide ? '1' : '0'); } catch (x) { /* no storage */ } break;
    case 'ends': st.ends = !st.ends; break;
    case 'end': { const en = ENDS[+d.i]; st.ends = false; setLens('rooms'); st.room = en.t.room; st.sel = {type: 'thing', id: en.t.id}; break; }
    case 'fix': { const en = ENDS[+d.i]; act(() => fix(en)); return; }
    case 'undo-param': act(() => api('DELETE', `/api/devices/${encodeURIComponent(d.dev)}/parameters/${encodeURIComponent(d.ref)}`)); return;
    case 'unlink': { const e2 = editors.get(d.dev), o = e2 && e2.objects.find(o => o.reference === d.ref); if (o) act(() => putLinks(d.dev, d.ref, o.links.map(l => l.address).filter(x => x !== d.a))); return; }
    case 'compare': api('POST', `/api/devices/${encodeURIComponent(d.id)}/compare`).catch(err => toast(err.message)); return;
    case 'adopt': act(() => api('POST', `/api/devices/${encodeURIComponent(d.id)}/adopt`, {})); return;
    case 'cancel-job': api('POST', '/api/job/cancel').catch(err => toast(err.message)); return;
    case 'review': st.review = true; break;
    case 'review-close': st.review = false; if (st.queue && !st.queue.active) st.queueSeen = st.queue.id; break;
    case 'queue-done': st.queueSeen = st.queue.id; st.review = changeCount() > 0; break;
    case 'scrim': if (e.target !== a || app.working || (st.queue && st.queue.active)) return; app.dialog = null; st.review = false; if (st.queue) st.queueSeen = st.queue.id; break;
    case 'discard': act(() => api('DELETE', `/api/devices/${encodeURIComponent(d.id)}/edits`)); return;
    case 'program': api('POST', '/api/program', {devices: d.ids.split(','), confirm: true}).then(r => { st.queue = r.queue; st.queueSeen = null; renderTop(); renderLayer(); }, err => toast(err.message)); return;
  }
  render();
});
document.addEventListener('toggle', e => { const k = e.target.dataset && e.target.dataset.keep; if (k) st[k] = e.target.open; }, true);
document.addEventListener('submit', e => {
  const f = e.target.closest('[data-form]'); if (!f) return; e.preventDefault();
  const name = ($('#pname') ? $('#pname').value : '').trim(), id = app.dialog && app.dialog.id;
  const done = project => { app.working = false; app.dialog = null; return loadProjects().then(() => { render(); return project; }); };
  const failed = err => { app.working = false; dialogState(err.message); };
  switch (f.dataset.form) {
    case 'new': {
      if (app.working) return;
      const copy = $('#from-copy') && $('#from-copy').checked, file = $('#pfile').files[0];
      if (!copy && !file) { dialogState('Choose a project file.'); return; }
      app.working = true; dialogState('');
      const made = copy ? api('POST', `/api/projects/${$('#from-src').value}/copy`, {name}) : api('POST', `/api/projects${name ? `?name=${encodeURIComponent(name)}` : ''}`, file, true);
      made.then(done).then(project => openProject(project.id), failed); return;
    }
    case 'rename': api('PUT', `/api/projects/${id}`, {name}).then(done, failed); return;
    case 'copy': api('POST', `/api/projects/${id}/copy`, {name}).then(done, failed); return;
  }
});
document.addEventListener('keydown', e => {
  if ((e.key === 'Enter' || e.key === ' ') && e.target.matches('[role="button"]')) { e.preventDefault(); e.target.click(); }
  if (e.key === 'Escape') {
    if (app.dialog) { if (app.working) return; app.dialog = null; } else if (app.menu) app.menu = null; else if (app.view !== 'project') return;
    else if (st.review) { if (st.queue && st.queue.active) return; st.review = false; } else if (st.ends) st.ends = false; else if (st.q) st.q = ''; else st.sel = null;
    render();
  }
  if (e.key === 'Enter' && e.target.id === 'q') { const b = $('#results button'); if (b) b.click(); }
});
document.addEventListener('input', e => { if (e.target.id === 'q') { st.q = e.target.value; renderResults(); } });
document.addEventListener('change', e => {
  const el = e.target, s = st.sel;
  if (el.id === 'from-src') { $('#from-copy').checked = true; return; }
  if (el.id === 'pfile') { $('#from-file').checked = true; return; }
  if (app.view !== 'project' || !M) return;
  if (el.id === 'bind' && s && s.type === 'rocker') { const p = plateById(s.plate), r = p.rockers[s.n - 1], t = el.value ? thingById(el.value) : null; act(() => rebind(p, r, t)); }
  else if (el.id === 'feed' && s && s.type === 'chan' && el.value) { const ch = chanOf(s), t = thingById(el.value); st.sel = {type: 'thing', id: t.id}; act(() => assignDriver(t, ch)); }
  else if (el.dataset.assign && el.value) { const [dev, key] = el.value.split('|'), t = thingById(el.dataset.assign); act(() => assignDriver(t, chanOf({dev, key}))); }
  else if (el.dataset.param) act(() => api('PUT', `/api/devices/${encodeURIComponent(el.dataset.dev)}/parameters/${encodeURIComponent(el.dataset.param)}`, {value: el.value}));
  else if (el.dataset.pagesel) { st.page[el.dataset.pagesel] = el.value; renderInsp(); }
  else if (el.dataset.addlink && el.value.trim()) { const e2 = editors.get(el.dataset.dev), o = e2 && e2.objects.find(o => o.reference === el.dataset.addlink), v = el.value.trim(); if (o) act(() => putLinks(el.dataset.dev, o.reference, [...o.links.map(l => l.address), v])); }
});

/* hover traces a wire end to end */
const stage = $('#stage');
stage.addEventListener('mouseover', e => {
  if (drag) return; const el = e.target.closest('.rocker,.thing,.sensor'); if (!el || !el.dataset) return;
  st.hover = el.classList.contains('rocker') ? {rk: el.dataset.rk, thing: el.dataset.thing} : el.classList.contains('sensor') ? {rk: $('.nub', el).dataset.rk, thing: el.dataset.thing} : {thing: el.dataset.thing}; applyFocus();
});
stage.addEventListener('mouseout', e => { if (drag) return; const el = e.target.closest('.rocker,.thing,.sensor'); if (el && !el.contains(e.relatedTarget)) { st.hover = null; applyFocus(); } });

/* drag a wire from a rocker to a thing */
let drag = null;
stage.addEventListener('pointerdown', e => {
  const n = e.target.closest('.nub'); if (!n || n.classList.contains('fixed') || !n.dataset.rk) return;
  const [pid] = n.dataset.rk.split(':'), p = plateById(pid); if (!p || !p.device.editable) return;
  e.preventDefault(); const b = $('.bench'), R = b.getBoundingClientRect(), a = n.getBoundingClientRect();
  const path = document.createElementNS('http://www.w3.org/2000/svg', 'path'); path.setAttribute('class', 'wire live'); $('.wires', b).appendChild(path);
  drag = {n, b, path, x1: a.left + a.width / 2 - R.left, y1: a.top + a.height / 2 - R.top, over: null};
  n.setPointerCapture(e.pointerId);
});
stage.addEventListener('pointermove', e => {
  if (!drag) return; const R = drag.b.getBoundingClientRect(), x2 = e.clientX - R.left, y2 = e.clientY - R.top, dx = Math.max(30, Math.abs(x2 - drag.x1) * .5);
  drag.path.setAttribute('d', `M${drag.x1} ${drag.y1}C${drag.x1 + dx} ${drag.y1} ${x2 - dx} ${y2} ${x2} ${y2}`);
  const hit = document.elementFromPoint(e.clientX, e.clientY), th = hit && hit.closest('.thing');
  if (drag.over && drag.over !== th) drag.over.classList.remove('drop');
  drag.over = th && thingById(th.dataset.thing).kind !== 'heating' ? th : null; if (drag.over) drag.over.classList.add('drop');
});
const endDrag = () => {
  if (!drag) return; const d = drag; drag = null; d.path.remove();
  if (d.over) { const [pid, n] = d.n.dataset.rk.split(':'), p = plateById(pid), t = thingById(d.over.dataset.thing); st.sel = {type: 'rocker', plate: pid, n: +n}; st.hover = null; act(() => rebind(p, p.rockers[+n - 1], t)); }
};
stage.addEventListener('pointerup', endDrag); stage.addEventListener('pointercancel', endDrag);

/* ---------- what the server tells while it works ---------- */
let socket = null, again = null;
function connect() {
  disconnect();
  const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`); socket = ws;
  ws.onmessage = ev => {
    let m; try { m = JSON.parse(ev.data); } catch (x) { return; }
    if (m.type === 'hello') { st.job = m.job; st.queue = m.queue; if (st.queue && !st.queue.active) st.queueSeen = st.queue.id; renderTop(); }
    else if (m.type === 'job') { const ended = m.job.status !== 'running'; st.job = m.job; if (ended) { clearTimeout(connect.t); connect.t = setTimeout(() => refresh().catch(() => {}), 150); } else { renderInsp(); renderLayer(); } }
    else if (m.type === 'progress') { if (st.job && st.job.id === m.job) { Object.assign(st.job, {sent: m.sent}, m.expected != null ? {expected: m.expected} : {}, m.phase ? {phase: m.phase} : {}); if (st.review) refreshDrawer(); else if (st.sel && st.sel.type === 'device') renderInsp(); } }
    else if (m.type === 'queue') { st.queue = m.queue; if (m.queue && m.queue.active) st.review = true; renderTop(); refreshDrawer(); if (m.queue && !m.queue.active) refresh().catch(() => {}); }
    else if (m.type === 'edited' && !st.busy) { clearTimeout(connect.e); connect.e = setTimeout(() => { if (!st.busy) refresh().catch(() => {}); }, 400); }
  };
  ws.onclose = () => { if (socket === ws && app.view === 'project') again = setTimeout(connect, 2000); };
}
function disconnect() { clearTimeout(again); if (socket) { const ws = socket; socket = null; ws.close(); } }
function refreshDrawer() { const el = $('.drawer'); if (el && st.review) el.innerHTML = reviewHtml(); else renderLayer(); }

let rz; window.addEventListener('resize', () => { clearTimeout(rz); rz = setTimeout(drawWires, 60); });
if (document.fonts && document.fonts.ready) document.fonts.ready.then(drawWires);
window.addEventListener('hashchange', () => { const h = location.hash.slice(1); if (h === 'projects' && app.view === 'project' && app.library) goStart(); else if (app.view === 'project' && (h === 'cabinet' || h === 'rooms') && h !== st.lens) { st.lens = h; render(); } });

(async () => {
  try {
    await loadProjects();
    const h = location.hash.slice(1);
    if (h === 'cabinet') st.lens = 'cabinet';
    if (app.open !== null && (h !== 'projects' || !app.library)) await enter(); else { setHash('projects'); render(); }
  } catch (e) { render(); toast(e.message); }
})();
