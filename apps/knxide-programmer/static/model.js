// Rooms, things and controls, worked out from what the server knows: devices with their group
// objects, the building's spaces and the group addresses. No DOM here; the page renders what
// this returns. Nothing in it is specific to one installation or one language: it goes by
// flags, sizes, address order, the names' common beginnings and the building structure.
'use strict';

const KnxModel = (() => {
  const words = name => String(name || '').trim().split(/\s+/).filter(Boolean);
  const commonWords = (a, b) => { let i = 0; while (i < a.length && i < b.length && a[i].toLowerCase() === b[i].toLowerCase()) i++; return i; };
  const sizeOf = (dpt, bits) => { const m = /^DPS?T-(\d+)/.exec(dpt || ''); const byType = {1: 1, 2: 2, 3: 4, 5: 8, 6: 8, 7: 16, 8: 16, 9: 16, 10: 24, 11: 24, 12: 32, 13: 32, 14: 32, 16: 112, 17: 8, 18: 8, 20: 8}; return (m && byType[+m[1]]) || bits || 0; };

  // What a thing is, by the words of its name; any language the pattern knows.
  const NOUNS = [
    ['outlet', /uttag|outlet|socket|steckdose|\bprise/i], ['awning', /markis|jalousie|\bblind|shutter|rollo|rollladen|awning|store\b/i],
    ['fan', /fläkt|\bfan\b|ventilat|lüfter/i], ['heater', /handdukstork|element|radiator|värmare|heater|heiz|towel/i],
    ['pump', /pump/i], ['door', /port\b|\bdoor|\btor\b|dörr|gate/i], ['scene', /^scen|szene/i],
    ['light', /belysning|lampa|ljus|light|lamp|licht|leuchte|spot|\btak\b|skena|fasad/i],
  ];
  // Where a product's objects say which output they belong to.
  const CHANNEL = [[/^(?:Channel|Kanal|Output|Ausgang|Sortie|Canal) ([A-Z]|\d+)$/i, 'name'], [/^(?:Channel|Kanal|Output|Ausgang) (\d+|[A-Z])\b/i, 'function'],
    [/^<Output (\w+)\s*\|/i, 'name'], [/^\[(\d+)\] Group/i, 'name'], [/^(?:Channel|Kanal|Output|Ausgang|Sortie) ([A-Z]|\d+)\b/i, 'name']];
  const channelKey = o => { for (const [rx, field] of CHANNEL) { const m = rx.exec(o[field] || ''); if (m) return m[1].toUpperCase(); } return null; };
  const channelCount = product => { for (const rx of [/(\d+)\s?x\s?\d/i, /(\d+)-fold/i, /(\d+)-f\b/i, /(\d+)[- ]?fach/i, /(\d+) sorties/i, /(\d+)-gang/i]) { const m = rx.exec(product || ''); if (m) return +m[1]; } return 0; };

  function build(raw) {
    // ---------------------------------------------------------------- addresses
    const ADDR = new Map();
    for (const a of raw.addresses) ADDR.set(a.address, {a: a.address, raw: a.raw, name: a.name || a.address, dpt: a.datapoint_type || '', source: a.source, users: []});
    const rangeOf = (rawAddr, level) => { const r = raw.ranges.find(r => r.level === level && r.start <= rawAddr && rawAddr <= r.end); return r ? r.name : ''; };

    // ---------------------------------------------------------------- devices
    const devices = raw.devices.map(d => ({...d, objects: d.objects || [], unused: d.unused || [], pages: d.pages || []}));
    const isPlate = d => d.kind === 'button';
    const isGira = d => /Push button sensor 3|Tastsensor 3/i.test(d.product || '');
    for (const d of devices) {
      d.keyed = new Map();
      for (const o of d.objects) {
        o.size = o.size_bits || sizeOf(o.datapoint_type, 0);
        o.sends = !!o.flags.transmit; o.takes = !!o.flags.write; o.state = o.sends && !o.takes;
        o.addrs = o.links.map(l => l.address);
        if (!isPlate(d)) { o.ch = channelKey(o); if (o.ch) { if (!d.keyed.has(o.ch)) d.keyed.set(o.ch, []); d.keyed.get(o.ch).push(o); } }
        for (const a of o.addrs) { if (!ADDR.has(a)) ADDR.set(a, {a, raw: 0, name: a, dpt: '', source: 'unknown', users: []}); ADDR.get(a).users.push({d, o}); }
      }
      d.module = ['dimmer', 'switch', 'heat', 'blind'].includes(d.kind) || (!isPlate(d) && d.keyed.size > 0);
    }
    for (const A of ADDR.values()) {
      A.size = sizeOf(A.dpt, (A.users[0] || {o: {}}).o.size);
      const out = A.users.filter(u => !isPlate(u.d)), plate = A.users.filter(u => isPlate(u.d));
      A.reportedBy = out.filter(u => u.o.state && u.o.ch && u.d.module);  // an output says so
      A.heardBy = out.filter(u => u.o.takes);                             // an output obeys
      A.state = A.reportedBy.length > 0 || (plate.some(u => u.o.takes && !u.o.sends) && !plate.some(u => u.o.sends));
      A.channels = new Set(A.heardBy.filter(u => u.o.ch && !A.state).map(u => u.d.id + '|' + u.o.ch));
    }

    // ---------------------------------------------------------------- things: runs of addresses that share a name
    const sorted = [...ADDR.values()].filter(A => A.raw).sort((x, y) => x.raw - y.raw);
    const clusters = [];
    const collides = (c, A) => { if (A.state || !A.channels.size) return false; return c.members.some(B => !B.state && B.size === A.size && B.channels.size && ![...B.channels].some(k => A.channels.has(k))); };
    let cur = null, prev = null;
    for (const A of sorted) {
      const w = words(A.name), sameGroup = cur && (cur.last.raw >> 8) === (A.raw >> 8);
      let joined = false;
      if (sameGroup && !collides(cur, A)) {
        const n = commonWords(cur.stem, w), gap = A.raw - cur.last.raw;
        if (cur.locked) joined = n === cur.stem.length && gap <= 9;
        else joined = n >= 1 && ((n === Math.min(cur.stem.length, w.length) && gap <= 9) || (n >= cur.stem.length - 2 && n >= w.length - 2 && gap <= 2));
        if (joined) { if (!cur.locked) { cur.stem = cur.stem.slice(0, n); cur.locked = true; } cur.members.push(A); cur.last = A; }
      }
      // A run broken by one odd name picks up again behind it.
      if (!joined && prev && prev.locked && (prev.last.raw >> 8) === (A.raw >> 8) && commonWords(prev.stem, w) === prev.stem.length && A.raw - prev.last.raw <= 9 && !collides(prev, A)) { prev.members.push(A); prev.last = A; joined = true; }
      if (!joined) { prev = cur; cur = {stem: w, locked: false, members: [A], last: A}; clusters.push(cur); }
    }
    const things = [], thingOf = new Map();
    for (const c of clusters) {
      const t = {id: 't' + c.members[0].raw, name: c.stem.join(' ') || c.members[0].name, addrs: c.members, drivers: [], group: false};
      for (const A of c.members) thingOf.set(A.a, t);
      things.push(t);
    }

    // ---------------------------------------------------------------- outputs: which thing each one is
    const modules = [];
    const channels = [];
    for (const d of devices) {
      if (isPlate(d)) continue;
      const m = {id: d.id, address: d.address || '', name: d.name && d.name !== d.product && !/\d/.test(d.name) ? d.name : '', kind: {Heating: d.module ? 'Heating actuator' : 'Heating', Other: 'Device'}[d.type] || d.type,
        product: d.product || '', order: d.order_number || '', cabinet: (d.location || []).slice(-1)[0] || 'Unplaced', channels: [], device: d};
      let keys = [...d.keyed.keys()];
      const total = d.module ? channelCount(d.product) : 0;
      if (keys.length && keys.every(k => /^[A-Z]$/.test(k))) { const top = Math.max(total, ...keys.map(k => k.charCodeAt(0) - 64)); keys = Array.from({length: top}, (_, i) => String.fromCharCode(65 + i)); }
      else if (keys.length && keys.every(k => /^\d+$/.test(k)) && total) { const top = Math.max(total, ...keys.map(Number)); keys = Array.from({length: top}, (_, i) => String(i + 1)); }
      for (const key of keys) {
        const objs = d.keyed.get(key) || [];
        const ch = {key, objs, linked: objs.filter(o => o.addrs.length), off: objs.length === 0, thing: null, groups: [], stray: [], reports: false, listens: false, m};
        m.channels.push(ch); channels.push(ch);
      }
      modules.push(m);
    }
    const receiving = new Map();   // thing id -> channels that obey one of its command addresses
    const thingsOf = (ch, pick) => { const out = []; for (const o of ch.linked) if (pick(o)) for (const a of o.addrs) { const t = thingOf.get(a), A = ADDR.get(a); if (t && pick(o, A) && !out.includes(t)) out.push(t); } return out; };
    const obeyed = ch => thingsOf(ch, (o, A) => o.takes && (!A || !A.state));
    const told = ch => thingsOf(ch, (o, A) => o.state && (!A || A.state));
    for (const ch of channels) for (const t of obeyed(ch)) { if (!receiving.has(t.id)) receiving.set(t.id, new Set()); receiving.get(t.id).add(ch); }
    for (const ch of channels) {
      const cmd = obeyed(ch), st = told(ch), both = st.filter(t => cmd.includes(t));
      // What it reports and obeys; else the one thing it reports, unless that is a lone address beside things it obeys; else the most particular thing it obeys.
      const pick = both[0] || (st.length === 1 && (st[0].addrs.length > 1 || !cmd.length) ? st[0] : null) || cmd.slice().sort((a, b) => receiving.get(a.id).size - receiving.get(b.id).size)[0] || st[0] || null;
      ch.thing = pick;
    }
    // An address only this one output uses belongs to the output's thing, whatever it is called.
    for (const ch of channels) {
      if (!ch.thing) continue;
      for (const o of ch.linked) for (const a of o.addrs) {
        const t = thingOf.get(a), A = ADDR.get(a);
        if (t && t !== ch.thing && t.addrs.length === 1 && A.users.filter(u => !isPlate(u.d)).every(u => u.d === ch.m.device && u.o.ch === ch.key)) {
          ch.thing.addrs.push(A); thingOf.set(a, ch.thing); t.addrs = [];
        }
      }
    }
    // A thing several outputs obey, each being something else, is a group of them.
    const groups = new Set();
    const mainCommand = t => t.addrs.filter(A => !A.state).sort((a, b) => a.raw - b.raw)[0];
    for (const ch of channels) {
      if (!ch.thing) continue;
      for (const o of ch.linked) if (o.takes) for (const a of o.addrs) { const t = thingOf.get(a); if (t && t !== ch.thing && t.addrs.length && mainCommand(t) === ADDR.get(a)) groups.add(t); }
    }
    for (const ch of channels) if (groups.has(ch.thing)) ch.thing = null;
    for (const ch of channels) {
      if (ch.thing) ch.thing.drivers.push(ch);
      for (const o of ch.linked) for (const a of o.addrs) {
        const t = thingOf.get(a), A = ADDR.get(a); if (!t) continue;
        if (t === ch.thing) { if (o.state && o.size === 1) ch.reports = true; if (o.takes && !A.state) ch.listens = true; }
        else if (groups.has(t)) { if (!ch.groups.includes(t)) ch.groups.push(t); }
        // Obeying another thing's address, or reporting on/off as another thing's state: a crossed link. A value sent on to someone else is not.
        else if (ch.thing && (o.takes || (o.state && o.size === 1 && A.state)) && !ch.stray.some(s => s.a === a)) ch.stray.push({thing: t, a, name: A.name, object: o});
      }
    }
    for (const t of groups) t.group = true;

    // ---------------------------------------------------------------- controls: wall plates and other senders
    const spaces = raw.spaces || [];
    const kindOfSpace = path => { const s = spaces.find(s => s.path.join('\u0001') === path.join('\u0001')); return s ? s.kind : ''; };
    const coarse = new Set(spaces.filter(s => spaces.some(c => c.kind === 'Room' && c.path.length > s.path.length && s.path.every((p, i) => c.path[i] === p))).map(s => s.path[s.path.length - 1]));
    const placeOf = d => {
      const loc = d.location || []; if (!loc.length) return null;
      if (kindOfSpace(loc) !== 'DistributionBoard') return loc[loc.length - 1];
      const parent = loc[loc.length - 2]; return parent && !coarse.has(parent) ? parent : null;
    };
    const plates = [];
    for (const d of devices) {
      const sending = d.objects.filter(o => o.addrs.length);
      if (isPlate(d) && isGira(d)) {
        const pair = /\((\d)\+(\d)\)/.exec(d.product), one = /(\d)-gang|(\d)fach/i.exec(d.product) || [];
        const units = pair ? [+pair[1], +pair[2]] : [+(one[1] || one[2] || 1)];
        const p = {id: d.id, address: d.address || '', name: d.name || d.product, product: d.product, order: d.order_number || '', room: placeOf(d), units, display: /plus/i.test(d.product), rockers: [], temp: null, device: d};
        for (let n = 1; n <= units.reduce((a, b) => a + b, 0); n++) p.rockers.push({n, label: `Rocker ${n}`, thing: null, objs: [], leds: [], pages: []});
        for (const o of d.objects) {
          const mr = /^(?:B\.)?(Rocker|Button) (\d+)$/.exec(o.name), ml = /^(?:B\.)?Status LED (\d+)$/.exec(o.name);
          if (mr) { const r = p.rockers[(mr[1] === 'Rocker' ? +mr[2] : Math.ceil(+mr[2] / 2)) - 1]; if (r) r.objs.push(o); }
          else if (ml) { const r = p.rockers[Math.ceil(+ml[1] / 2) - 1]; if (r) r.leds.push({side: +ml[1] % 2 ? 'L' : 'R', object: o}); }
          else if (o.sends && o.size === 16 && o.addrs.length) { const t = thingOf.get(o.addrs[0]); if (t) p.tempThing = t; }
        }
        for (const r of p.rockers) {
          const main = r.objs.find(o => o.addrs.length && o.sends && o.size === 1) || r.objs.find(o => o.addrs.length);
          r.thing = main ? thingOf.get(main.addrs[0]) || null : null;
          r.pages = d.pages.filter(pg => new RegExp(`^(?:Rocker ${r.n}\\b|R${r.n} - )`, 'i').test(pg.title));
        }
        plates.push(p);
      } else {
        // Any other device: one row per signal it sends to a thing. No claim about its shape.
        const rows = [], seen = new Map();
        for (const o of sending) {
          if (!o.sends || (d.module && (o.state || o.ch))) continue;
          const A = ADDR.get(o.addrs[0]), t = thingOf.get(o.addrs[0]);
          if (!t || !A || A.state || (o.size === 16 && !isPlate(d))) continue;
          const label = o.name + (o.function && o.function !== 'Output' && !isPlate(d) ? ` · ${o.function}` : '');
          const key = isPlate(d) ? o.name : o.reference;
          if (seen.has(key)) { seen.get(key).objs.push(o); continue; }
          const r = {n: rows.length + 1, label, thing: t, objs: [o], leds: [], pages: []}; seen.set(key, r); rows.push(r);
        }
        if (isPlate(d)) for (const o of d.objects) if (o.takes && !o.sends) { const num = /(\d+)/.exec(o.name), r = num && rows.find(r => new RegExp(`\\b${num[1]}$`).test(r.objs[0].name)); if (r) r.leds.push({side: 'L', object: o}); }
        if (rows.length || isPlate(d)) {
          const more = d.objects.filter(o => o.sends && !o.state && !o.addrs.length && !(o.ch && d.module)).length;
          plates.push({id: d.id, address: d.address || '', name: d.name && !/\d/.test(d.name) || isPlate(d) ? (d.name || d.product) : (d.type === 'Other' ? d.product : d.type), product: d.product || '', order: d.order_number || '',
            room: placeOf(d), units: [rows.length], display: false, rockers: rows, temp: null, generic: true, more, device: d});
        }
      }
    }

    // ---------------------------------------------------------------- keep what is a thing
    const controlsOf = new Map();
    for (const p of plates) for (const r of p.rockers) if (r.thing) { if (!controlsOf.has(r.thing)) controlsOf.set(r.thing, []); controlsOf.get(r.thing).push({p, r}); }
    const real = t => t.addrs.length && (t.drivers.length || t.group || (controlsOf.get(t) || []).some(x => !x.p.generic));
    const kept = things.filter(real), keptSet = new Set(kept);
    for (const p of plates) { for (const r of p.rockers) if (r.thing && !keptSet.has(r.thing)) r.thing = null; if (p.generic) { p.rockers = p.rockers.filter(r => r.thing || !p.device || isPlate(p.device)); p.rockers.forEach((r, i) => { r.n = i + 1; }); p.units = [p.rockers.length]; } }
    for (let i = plates.length - 1; i >= 0; i--) if (plates[i].generic && !plates[i].rockers.length) plates.splice(i, 1);
    for (const ch of channels) { ch.stray = ch.stray.filter(s => keptSet.has(s.thing)); ch.groups = ch.groups.filter(t => keptSet.has(t)); }

    // ---------------------------------------------------------------- rooms
    const roomSpaces = spaces.filter(s => s.kind === 'Room' && !devices.some(d => !isPlate(d) && d.module && (d.location || []).join('\u0001') === s.path.join('\u0001') && false));
    const roomNames = [];
    const rooms = [];
    const addRoom = (name, wing) => { if (!name) return; if (!rooms.some(r => r.name.toLowerCase() === name.toLowerCase())) { rooms.push({name, wing: wing || ''}); roomNames.push(name); } };
    for (const s of roomSpaces) addRoom(s.path[s.path.length - 1], s.path.length > 1 ? s.path[s.path.length - 2] : '');
    for (const s of spaces) if (s.kind !== 'Room' && s.kind !== 'DistributionBoard' && !coarse.has(s.path[s.path.length - 1]) && !spaces.some(c => c.kind === 'Room' && c.path.length > s.path.length && s.path.every((p, i) => c.path[i] === p))) roomNames.push(s.path[s.path.length - 1]);
    const byLength = () => roomNames.slice().sort((a, b) => b.length - a.length);
    const esc = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const canonical = name => (rooms.find(r => r.name.toLowerCase() === name.toLowerCase()) || {name}).name;
    for (const t of kept) {
      const names = byLength(), lower = t.name.toLowerCase();
      let room = names.find(n => lower === n.toLowerCase() || lower.startsWith(n.toLowerCase() + ' '));
      if (!room) room = names.find(n => new RegExp(`(^|\\s)${esc(n.toLowerCase())}(\\s|$)`).test(lower));
      const first = t.addrs[0], middle = first ? rangeOf(first.raw, 1) : '', main = first ? rangeOf(first.raw, 0) : '';
      if (!room && middle && !coarse.has(middle)) room = middle;
      if (!room) { const votes = {}; for (const x of controlsOf.get(t) || []) if (x.p.room) votes[x.p.room] = (votes[x.p.room] || 0) + 1; room = Object.keys(votes).sort((a, b) => votes[b] - votes[a])[0]; }
      t.room = canonical(room || middle || main || 'Other');
    }
    for (const p of plates) {
      if (!p.room) { const t = (p.rockers.find(r => r.thing) || {}).thing; p.room = t ? t.room : 'Unplaced'; }
      p.room = canonical(p.room);
    }
    for (const p of plates) addRoom(p.room, '');
    for (const t of kept) addRoom(t.room, '');

    // ---------------------------------------------------------------- what each thing is, and its loose ends
    const short = t => { const lr = t.room.toLowerCase(), ln = t.name.toLowerCase(); const s = ln === lr ? '' : ln.startsWith(lr + ' ') ? t.name.slice(t.room.length).trim() : t.name; return s ? s[0].toUpperCase() + s.slice(1) : t.name; };
    const named = new Map();
    for (const t of kept) { const k = t.room + '\u0001' + t.name.toLowerCase(); if (named.has(k)) t.name = t.addrs[0].name; else named.set(k, t); }
    for (const t of kept) {
      const driver = t.drivers[0] && t.drivers[0].m.device.kind, noun = (NOUNS.find(([, rx]) => rx.test(t.name)) || [])[0];
      t.kind = driver === 'heat' || (!driver && t.addrs.every(A => A.size === 16)) ? 'heating' : driver === 'blind' ? 'awning' : noun && noun !== 'light' ? noun
        : driver === 'dimmer' || t.addrs.some(A => A.size === 4) ? 'dimlight' : noun || 'load';
      t.short = t.kind === 'heating' && t.name.toLowerCase() === t.room.toLowerCase() ? 'Heating' : short(t);
      t.controls = controlsOf.get(t) || [];
      const cmd = t.addrs.filter(A => !A.state);
      t.heard = cmd.some(A => A.heardBy.length > 0);
      t.users = [...new Set(t.addrs.flatMap(A => A.users.filter(u => !isPlate(u.d) && !t.drivers.some(ch => ch.m.device === u.d)).map(u => u.d)))];
      t.members = t.group ? channels.filter(ch => ch.groups.includes(t)) : [];
      t.leds = []; for (const p of plates) for (const r of p.rockers) for (const l of r.leds) for (const a of l.object.addrs) if (thingOf.get(a) === t) t.leds.push({p, r, l, a, waits: !(ADDR.get(a).reportedBy.length)});
      t.unused = !t.controls.length && !t.drivers.length && !t.group;
    }
    for (const p of plates) if (p.tempThing && keptSet.has(p.tempThing) && p.tempThing.kind === 'heating') p.temp = p.tempThing;

    const ends = [];
    for (const t of kept) {
      const fromPlates = t.controls.filter(x => x.r.objs.some(o => o.sends));
      if (fromPlates.length && !t.heard && t.kind !== 'scene') ends.push({type: 'unheard', t, title: 'Nothing listens', text: `Pressing a button for ${t.name} reaches no output.`});
      const waiting = t.leds.filter(l => l.waits);
      if (waiting.length) ends.push({type: 'unreported', t, leds: waiting, title: waiting.length > 1 ? `${waiting.length} LEDs never update` : 'LED never updates', text: `Nothing reports whether ${t.name} is on.`});
    }
    for (const ch of channels) for (const s of ch.stray) ends.push({type: 'stray', t: ch.thing, ch, s, title: 'Crossed link', text: `${chLabel(ch)} drives ${ch.thing.name}, and is also linked to ${s.name}.`});

    return {raw, rooms, things: kept, plates, modules, channels, ends, addr: ADDR, thingOf, devices: new Map(devices.map(d => [d.id, d]))};
  }

  const modLabel = m => m.name || m.kind;
  const chLabel = ch => `${modLabel(ch.m)} ${ch.m.address} · ${ch.key}`;
  // The address of a thing a group object of this size and direction goes with.
  const pick = (t, size, state) => t.addrs.find(A => A.size === size && !!A.state === !!state) || null;

  return {build, modLabel, chLabel, pick, sizeOf};
})();

if (typeof module !== 'undefined') module.exports = KnxModel;
