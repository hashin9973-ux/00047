import { BLOCKS, CATS, validate, fieldError } from './blocks.js';

const W = 260; // must match .node width in styles.css
const PORT = { in: 21, if: 20, else: 40, after: 60, tgt: 21 };
const KINDS = {
  if: { label: 'IF', color: '#4ade80' },
  else: { label: 'ELSE', color: '#ff6b81' },
  after: { label: 'AFTER', color: '#5aa9ff' },
  target: { label: 'TARGET', color: '#3ddbd9' },
};
const NS = 'http://www.w3.org/2000/svg';
const svg = (t, a = {}) => { const e = document.createElementNS(NS, t); for (const k in a) e.setAttribute(k, a[k]); return e; };
const uid = () => (crypto.randomUUID ? crypto.randomUUID() : 'n' + Math.random().toString(36).slice(2) + Date.now().toString(36));
const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const isTyping = t => !!(t && t.closest && t.closest('input,textarea,select,[contenteditable]'));

function bez(a, b) {
  const dx = Math.max(40, Math.abs(b.x - a.x) * 0.4);
  return `M${a.x},${a.y} C${a.x + dx},${a.y} ${b.x - dx},${b.y} ${b.x},${b.y}`;
}

function rounded(pts, r = 14) {
  let d = `M${pts[0][0]},${pts[0][1]}`;
  for (let i = 1; i < pts.length - 1; i++) {
    const [x0, y0] = pts[i - 1], [x1, y1] = pts[i], [x2, y2] = pts[i + 1];
    const l1 = Math.hypot(x1 - x0, y1 - y0) || 1, l2 = Math.hypot(x2 - x1, y2 - y1) || 1;
    const rr = Math.min(r, l1 / 2, l2 / 2);
    d += ` L${x1 + (x0 - x1) / l1 * rr},${y1 + (y0 - y1) / l1 * rr} Q${x1},${y1} ${x1 + (x2 - x1) / l2 * rr},${y1 + (y2 - y1) / l2 * rr}`;
  }
  const l = pts[pts.length - 1];
  return d + ` L${l[0]},${l[1]}`;
}

const cub = (p0, p1, p2, p3, t) => {
  const u = 1 - t;
  return {
    x: u * u * u * p0.x + 3 * u * u * t * p1.x + 3 * u * t * t * p2.x + t * t * t * p3.x,
    y: u * u * u * p0.y + 3 * u * u * t * p1.y + 3 * u * t * t * p2.y + t * t * t * p3.y,
  };
};
const hitsRect = (pts, rs) => pts.some(p => rs.some(r => p.x > r[0] && p.x < r[2] && p.y > r[1] && p.y < r[3]));

export class Editor {
  constructor(host, hooks) {
    this.host = host;
    this.hooks = hooks;
    this.auto = null;
    this.vp = { x: 80, y: 80, k: 1 };
    this.els = new Map();
    this.wires = new Map();
    this.states = {};
    this.sel = new Set();
    this.selEdge = null;
    this.clip = null;
    this.space = false;
    this.active = false;
    this.hover = false;
    this.cursor = { x: 0, y: 0 };
    this.drag = null;
    this.temp = null;
    this.raf = 0;
    this.uStack = []; this.rStack = []; this.last = ''; this.lastTag = null; this.lastT = 0; this.applying = false;
    this.build();
  }

  build() {
    this.host.innerHTML = `
      <div class="canvas">
        <div class="world">
          <svg class="wires"><defs>${Object.entries(KINDS).map(([k, v]) =>
            `<marker id="ar-${k}" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="5" markerHeight="5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="${v.color}"/></marker>`).join('')}</defs><g class="wl"></g></svg>
          <div class="nodes"></div>
        </div>
        <div class="marquee" hidden></div>
        <div class="edge-ui" hidden>
          <div class="eu-bar glass"><button data-k="if">IF</button><button data-k="else">ELSE</button><button data-k="after">AFTER</button><button data-del="1" title="Delete wire">✕</button></div>
          <div class="eu-handle" title="Drag to reroute"></div>
        </div>
        <div class="ctx glass" hidden></div>
      </div>`;
    const q = s => this.host.querySelector(s);
    this.cv = q('.canvas'); this.world = q('.world'); this.nodesEl = q('.nodes'); this.wl = q('.wl');
    this.marq = q('.marquee'); this.eui = q('.edge-ui'); this.menu = q('.ctx');

    const cv = this.cv;
    cv.addEventListener('pointerdown', e => this.down(e));
    cv.addEventListener('pointermove', e => this.move(e));
    cv.addEventListener('pointerup', e => this.up(e));
    cv.addEventListener('pointercancel', e => this.up(e));
    cv.addEventListener('wheel', e => this.wheel(e), { passive: false });
    cv.addEventListener('contextmenu', e => this.ctxmenu(e));
    cv.addEventListener('pointerenter', () => { this.hover = true; });
    cv.addEventListener('pointerleave', () => { this.hover = false; });
    window.addEventListener('keydown', e => this.key(e, true));
    window.addEventListener('keyup', e => this.key(e, false));
    document.addEventListener('pointerdown', e => { if (!this.menu.hidden && !this.menu.contains(e.target)) this.hideMenu(); }, true);

    this.eui.addEventListener('click', e => {
      const b = e.target.closest('button'); const ed = this.edge();
      if (!b || !ed) return;
      if (b.dataset.del) { this.auto.edges = this.auto.edges.filter(x => x !== ed); this.selEdge = null; this.refreshAll(); }
      else ed.kind = b.dataset.k;
      this.scheduleWires(); this.changed();
    });
    this.eui.querySelector('.eu-handle').addEventListener('pointerdown', e => {
      const ed = this.edge(); if (!ed) return;
      e.stopPropagation(); e.preventDefault();
      this.drag = { t: 'bend', id: ed.id, sy: e.clientY, orig: ed.bend || 0 };
      this.cv.setPointerCapture(e.pointerId);
    });
  }

  // ---------- public API ----------
  setActive(v) { this.active = v; }

  load(auto) {
    this.auto = auto;
    if (!auto.viewport || !auto.viewport.k) auto.viewport = { x: 80, y: 80, k: 1 };
    this.vp = auto.viewport;
    this.nodesEl.innerHTML = ''; this.wl.innerHTML = '';
    this.els.clear(); this.wires.clear();
    this.sel = new Set(); this.selEdge = null; this.states = {};
    this.hideMenu();
    for (const n of auto.nodes) this.addEl(n);
    this.refreshAll(); this.applyVp(); this.scheduleWires();
    this.uStack = []; this.rStack = []; this.last = this.snap(); this.lastTag = null;
  }

  setNodeState(id, st) { this.states[id] = st; this.paintState(id, st); }
  clearStates() { this.states = {}; for (const id of this.els.keys()) this.paintState(id, { state: 'idle', progress: null, detail: '' }); }
  settle() {
    for (const [id, st] of Object.entries(this.states)) {
      if (st.state === 'running' || st.state === 'waiting') this.setNodeState(id, { ...st, state: 'idle', progress: null });
    }
  }

  fit() {
    const ns = this.auto.nodes;
    if (!ns.length) { Object.assign(this.vp, { x: 80, y: 80, k: 1 }); }
    else {
      const r = this.cv.getBoundingClientRect();
      let x0 = 1e9, y0 = 1e9, x1 = -1e9, y1 = -1e9;
      for (const n of ns) {
        const h = this.els.get(n.id)?.offsetHeight || 90;
        x0 = Math.min(x0, n.x); y0 = Math.min(y0, n.y); x1 = Math.max(x1, n.x + W); y1 = Math.max(y1, n.y + h);
      }
      const availH = Math.max(120, r.height - 230);
      const k = clamp(Math.min((r.width - 120) / (x1 - x0), availH / (y1 - y0)), 0.2, 1.2);
      Object.assign(this.vp, { k, x: (r.width - (x1 - x0) * k) / 2 - x0 * k, y: (availH - (y1 - y0) * k) / 2 - y0 * k + 20 });
    }
    this.applyVp(); this.changed();
  }

  openMenuCenter() {
    const r = this.cv.getBoundingClientRect();
    this.openMenu(r.left + r.width / 2 - 150, r.top + 70, null);
  }

  // ---------- helpers ----------
  node(id) { return this.auto.nodes.find(n => n.id === id); }
  edge() { return this.auto.edges.find(e => e.id === this.selEdge); }
  snap() { return JSON.stringify({ n: this.auto.nodes, e: this.auto.edges }); }

  changed(tag) {
    if (!this.applying) {
      const now = this.snap();
      if (now !== this.last) {
        const t = Date.now();
        if (!(tag && tag === this.lastTag && t - this.lastT < 1000)) {
          this.uStack.push(this.last);
          if (this.uStack.length > 100) this.uStack.shift();
        }
        this.rStack = []; this.last = now; this.lastTag = tag || null; this.lastT = t;
      }
    }
    this.hooks.onChange(this.auto);
  }

  restore(s) {
    const o = JSON.parse(s);
    this.applying = true;
    this.auto.nodes = o.n; this.auto.edges = o.e; this.last = s; this.lastTag = null;
    const keep = new Set([...this.sel].filter(id => o.n.some(n => n.id === id)));
    this.nodesEl.innerHTML = ''; this.wl.innerHTML = '';
    this.els.clear(); this.wires.clear();
    for (const n of this.auto.nodes) this.addEl(n);
    this.sel = new Set(); this.selEdge = null;
    this.setSel(keep); this.refreshAll(); this.scheduleWires();
    this.applying = false;
    this.hooks.onChange(this.auto);
  }
  doUndo() { if (!this.uStack.length) return; this.rStack.push(this.last); this.restore(this.uStack.pop()); }
  doRedo() { if (!this.rStack.length) return; this.uStack.push(this.last); this.restore(this.rStack.pop()); }
  toWorld(cx, cy) {
    const r = this.cv.getBoundingClientRect();
    return { x: (cx - r.left - this.vp.x) / this.vp.k, y: (cy - r.top - this.vp.y) / this.vp.k };
  }
  place(id) { const n = this.node(id), el = this.els.get(id); if (n && el) el.style.transform = `translate(${n.x}px,${n.y}px)`; }

  applyVp() {
    const { x, y, k } = this.vp;
    this.world.style.transform = `translate(${x}px,${y}px) scale(${k})`;
    const s = this.cv.style;
    s.setProperty('--g', 24 * k + 'px'); s.setProperty('--g5', 120 * k + 'px');
    s.setProperty('--vx', x + 'px'); s.setProperty('--vy', y + 'px');
    this.placeEdgeUI();
  }

  // ---------- selection ----------
  setSel(s) {
    this.sel = s;
    if (s.size) this.selEdge = null;
    for (const [id, el] of this.els) el.classList.toggle('sel', s.has(id));
    this.scheduleWires();
  }
  select(ids) { this.setSel(new Set(ids)); }
  toggle(id) { const s = new Set(this.sel); s.has(id) ? s.delete(id) : s.add(id); this.setSel(s); }
  selectEdge(id) { this.setSel(new Set()); this.selEdge = id; this.scheduleWires(); }

  // ---------- pointer ----------
  down(e) {
    if (e.target.closest('.ctx,.edge-ui')) return;
    if (e.button === 2) return;
    const port = e.target.closest('.port.out');
    if (port && e.button === 0) return this.startConnect(e, port);
    const nodeEl = e.target.closest('.node');
    const pan = e.button === 1 || (e.button === 0 && this.space);
    if (nodeEl && !pan) {
      const id = nodeEl.dataset.id;
      if (e.target.closest('.n-body,.n-chev')) { if (!this.sel.has(id)) this.select([id]); return; }
      const mod = e.ctrlKey || e.metaKey || e.shiftKey;
      if (mod) this.toggle(id); else if (!this.sel.has(id)) this.select([id]);
      if (!this.sel.has(id)) return;
      this.drag = {
        t: 'nodes', sx: e.clientX, sy: e.clientY, moved: false, id, mod,
        orig: new Map([...this.sel].map(i => { const n = this.node(i); return [i, { x: n.x, y: n.y }]; })),
      };
      this.cv.setPointerCapture(e.pointerId); e.preventDefault();
      return;
    }
    if (document.activeElement && document.activeElement !== document.body) document.activeElement.blur();
    e.preventDefault();
    this.cv.setPointerCapture(e.pointerId);
    if (e.button === 0 && !pan && (e.ctrlKey || e.metaKey || e.shiftKey)) {
      const r = this.cv.getBoundingClientRect();
      this.drag = { t: 'box', x0: e.clientX - r.left, y0: e.clientY - r.top, base: new Set(this.sel) };
      this.marq.hidden = false;
    } else {
      this.drag = { t: 'pan', sx: e.clientX, sy: e.clientY, ox: this.vp.x, oy: this.vp.y, moved: false, btn: e.button };
    }
  }

  move(e) {
    this.cursor = this.toWorld(e.clientX, e.clientY);
    const d = this.drag; if (!d) return;
    if (d.t === 'nodes') {
      if (!d.moved && Math.hypot(e.clientX - d.sx, e.clientY - d.sy) < 3) return;
      d.moved = true;
      const dx = (e.clientX - d.sx) / this.vp.k, dy = (e.clientY - d.sy) / this.vp.k;
      for (const [id, o] of d.orig) { const n = this.node(id); n.x = Math.round(o.x + dx); n.y = Math.round(o.y + dy); this.place(id); }
      this.scheduleWires();
    } else if (d.t === 'pan') {
      if (Math.hypot(e.clientX - d.sx, e.clientY - d.sy) > 2) d.moved = true;
      this.vp.x = d.ox + e.clientX - d.sx; this.vp.y = d.oy + e.clientY - d.sy;
      this.applyVp();
    } else if (d.t === 'box') {
      const r = this.cv.getBoundingClientRect();
      const x1 = e.clientX - r.left, y1 = e.clientY - r.top;
      const x = Math.min(d.x0, x1), y = Math.min(d.y0, y1), w = Math.abs(x1 - d.x0), h = Math.abs(y1 - d.y0);
      Object.assign(this.marq.style, { left: x + 'px', top: y + 'px', width: w + 'px', height: h + 'px' });
      const s = new Set(d.base); const { x: vx, y: vy, k } = this.vp;
      for (const n of this.auto.nodes) {
        const nx = n.x * k + vx, ny = n.y * k + vy, nw = W * k, nh = (this.els.get(n.id)?.offsetHeight || 90) * k;
        if (nx < x + w && nx + nw > x && ny < y + h && ny + nh > y) s.add(n.id);
      }
      this.setSel(s);
    } else if (d.t === 'connect') {
      d.p = this.cursor; this.drawTemp();
    } else if (d.t === 'bend') {
      const ed = this.auto.edges.find(x => x.id === d.id);
      if (ed) { ed.bend = Math.round(d.orig + (e.clientY - d.sy) / this.vp.k); this.scheduleWires(); }
    }
  }

  up(e) {
    const d = this.drag; this.drag = null;
    try { this.cv.releasePointerCapture(e.pointerId); } catch { /* not captured */ }
    if (!d) return;
    if (d.t === 'nodes') { if (d.moved) this.changed(); else if (!d.mod) this.select([d.id]); }
    else if (d.t === 'pan') { if (!d.moved && d.btn === 0) { this.select([]); this.selEdge = null; this.scheduleWires(); } else if (d.moved) this.changed(); }
    else if (d.t === 'box') this.marq.hidden = true;
    else if (d.t === 'connect') this.finishConnect(e, d);
    else if (d.t === 'bend') this.changed();
  }

  wheel(e) {
    if (e.target.closest('.ctx')) return;
    e.preventDefault();
    this.zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.0016));
  }
  zoomAt(cx, cy, f) {
    const r = this.cv.getBoundingClientRect();
    const k = clamp(this.vp.k * f, 0.2, 2.5);
    const px = cx - r.left, py = cy - r.top;
    this.vp.x = px - (px - this.vp.x) * (k / this.vp.k);
    this.vp.y = py - (py - this.vp.y) * (k / this.vp.k);
    this.vp.k = k;
    this.applyVp();
    clearTimeout(this.zt); this.zt = setTimeout(() => this.changed(), 500);
  }

  key(e, down) {
    if (!this.active) return;
    if (e.code === 'Space') {
      if (isTyping(e.target)) return;
      this.space = down; this.cv.classList.toggle('pan', down);
      e.preventDefault();
      return;
    }
    if (!down || isTyping(e.target)) return;
    const c = e.ctrlKey || e.metaKey, k = e.key.toLowerCase();
    if (this.drag?.t === 'connect' && this.drag.kind !== 'target' && ['1', '2', '3'].includes(e.key)) {
      this.drag.kind = ['if', 'else', 'after'][+e.key - 1]; this.drawTemp(); return;
    }
    if (e.key === 'Delete' || e.key === 'Backspace') { this.deleteSel(); e.preventDefault(); }
    else if (c && k === 'c') this.copy();
    else if (c && k === 'v') { this.paste(this.hover ? this.cursor : null); e.preventDefault(); }
    else if (c && k === 'd') { this.duplicate(); e.preventDefault(); }
    else if (c && k === 'a') { this.setSel(new Set(this.auto.nodes.map(n => n.id))); e.preventDefault(); }
    else if (!c && k === 'b') { this.toggleDisabled(); e.preventDefault(); }
    else if (c && k === 'z' && !e.shiftKey) { this.doUndo(); e.preventDefault(); }
    else if (c && (k === 'y' || (k === 'z' && e.shiftKey))) { this.doRedo(); e.preventDefault(); }
    else if (e.key === 'Escape') { this.select([]); this.selEdge = null; this.hideMenu(); this.scheduleWires(); }
  }

  // ---------- connecting ----------
  startConnect(e, port) {
    const id = port.closest('.node').dataset.id;
    this.drag = { t: 'connect', from: id, kind: port.dataset.t, p: this.toWorld(e.clientX, e.clientY) };
    this.temp = svg('path', { class: 'wire temp' });
    this.wl.appendChild(this.temp);
    this.cv.setPointerCapture(e.pointerId);
    e.preventDefault(); e.stopPropagation();
    this.drawTemp();
  }
  drawTemp() {
    const d = this.drag, f = this.node(d.from);
    if (!d || !f || !this.temp) return;
    const a = { x: f.x + W, y: f.y + PORT[d.kind === 'target' ? 'tgt' : d.kind] };
    this.temp.setAttribute('d', bez(a, d.p));
    this.temp.setAttribute('class', `wire temp k-${d.kind}`);
    this.temp.setAttribute('marker-end', `url(#ar-${d.kind})`);
  }
  finishConnect(e, d) {
    if (this.temp) { this.temp.remove(); this.temp = null; }
    const hit = document.elementFromPoint(e.clientX, e.clientY)?.closest('.node');
    if (hit && hit.dataset.id !== d.from) this.connect(d.from, hit.dataset.id, d.kind);
    else if (!hit) this.openMenu(e.clientX, e.clientY, { from: d.from, kind: d.kind });
  }
  connect(from, to, kind) {
    const A = this.node(from), B = this.node(to);
    if (!A || !B || from === to) return false;
    const ca = BLOCKS[A.kind]?.cat, cb = BLOCKS[B.kind]?.cat;
    if (kind === 'target') {
      if (ca !== 'tgt' || cb === 'tgt') { this.hooks.toast('A Target block can only feed a Condition or an Action.'); return false; }
    } else if (ca === 'tgt' || cb === 'tgt') {
      this.hooks.toast('IF / ELSE / AFTER wires connect Conditions and Actions. Use the teal port on a Target block to bind it.');
      return false;
    }
    if (this.auto.edges.some(e => e.from === from && e.to === to && e.kind === kind)) return false;
    this.auto.edges.push({ id: uid(), from, to, kind, bend: 0 });
    this.refreshAll(); this.scheduleWires(); this.changed();
    return true;
  }

  // ---------- wires ----------
  scheduleWires() { if (!this.raf) this.raf = requestAnimationFrame(() => { this.raf = 0; this.drawWires(); }); }

  calcRects() {
    return this.auto.nodes.map(n => ({ id: n.id, r: [n.x - 8, n.y - 8, n.x + W + 8, n.y + (this.els.get(n.id)?.offsetHeight || 90) + 8] }));
  }

  route(e, f, t) {
    const a = { x: f.x + W, y: f.y + PORT[e.kind === 'target' ? 'tgt' : e.kind] };
    const b = { x: t.x, y: t.y + PORT.in };
    const rs = (this.rcache || (this.rcache = this.calcRects())).filter(o => o.id !== f.id && o.id !== t.id).map(o => o.r);
    if (b.x - a.x >= 70) {
      const dx = Math.max(40, (b.x - a.x) * 0.35), mx = (a.x + b.x) / 2, my0 = (a.y + b.y) / 2;
      let bend = e.bend || 0;
      if (!bend && rs.length) {
        let found = false;
        for (const c of [0, 50, -50, 110, -110, 180, -180, 260, -260, 360, -360, 500, -500, 700, -700, 900, -900]) {
          const my = my0 + c;
          const P = [a, { x: a.x + dx, y: a.y }, { x: mx - dx * 0.8, y: my }, { x: mx, y: my }, { x: mx + dx * 0.8, y: my }, { x: b.x - dx, y: b.y }, b];
          const pts = [];
          for (let i = 0; i <= 12; i++) {
            pts.push(cub(P[0], P[1], P[2], P[3], i / 12));
            pts.push(cub(P[3], P[4], P[5], P[6], i / 12));
          }
          if (!hitsRect(pts, rs)) { bend = c; found = true; break; }
        }
        if (!found) bend = 1000;
      }
      const my = my0 + bend;
      return { d: `M${a.x},${a.y} C${a.x + dx},${a.y} ${mx - dx * 0.8},${my} ${mx},${my} S${b.x - dx},${b.y} ${b.x},${b.y}`, mid: { x: mx, y: my } };
    }
    // Backward or overlapping connections use an orthogonal lane below the blocks.
    const hf = this.els.get(f.id)?.offsetHeight || 90, ht = this.els.get(t.id)?.offsetHeight || 90;
    let low = Math.max(f.y + hf, t.y + ht) + 36;
    const x1 = a.x + 30, x2 = b.x - 30;
    if (e.bend) low += e.bend;
    else {
      const lo = Math.min(x1, x2), hi = Math.max(x1, x2);
      for (let i = 0; i < 12; i++) {
        if (!rs.some(r => r[0] < hi && r[2] > lo && low > r[1] && low < r[3])) break;
        low += 50;
      }
    }
    return { d: rounded([[a.x, a.y], [x1, a.y], [x1, low], [x2, low], [x2, b.y], [b.x, b.y]]), mid: { x: (x1 + x2) / 2, y: low } };
  }

  drawWires() {
    this.rcache = this.calcRects();
    const seen = new Set();
    for (const e of this.auto.edges) {
      const f = this.node(e.from), t = this.node(e.to);
      if (!f || !t) continue;
      seen.add(e.id);
      let w = this.wires.get(e.id);
      if (!w) {
        w = { hit: svg('path', { class: 'wire hit' }), vis: svg('path'), mid: null };
        const id = e.id;
        w.hit.addEventListener('pointerdown', ev => { ev.stopPropagation(); ev.preventDefault(); this.selectEdge(id); });
        this.wl.appendChild(w.hit); this.wl.appendChild(w.vis);
        this.wires.set(e.id, w);
      }
      const { d, mid } = this.route(e, f, t);
      w.mid = mid;
      w.hit.setAttribute('d', d); w.vis.setAttribute('d', d);
      w.vis.setAttribute('class', `wire k-${e.kind}${this.selEdge === e.id ? ' sel' : ''}`);
      w.vis.setAttribute('marker-end', `url(#ar-${e.kind})`);
    }
    for (const [id, w] of this.wires) if (!seen.has(id)) { w.hit.remove(); w.vis.remove(); this.wires.delete(id); }
    this.placeEdgeUI();
  }

  placeEdgeUI() {
    const e = this.edge(), w = e && this.wires.get(e.id);
    if (!e || !w || !w.mid) { this.eui.hidden = true; return; }
    this.eui.hidden = false;
    this.eui.style.transform = `translate(${w.mid.x * this.vp.k + this.vp.x}px,${w.mid.y * this.vp.k + this.vp.y}px)`;
    this.eui.querySelectorAll('button[data-k]').forEach(b => { b.hidden = e.kind === 'target'; b.classList.toggle('on', b.dataset.k === e.kind); });
  }

  // ---------- nodes ----------
  addEl(n) {
    const def = BLOCKS[n.kind];
    const el = document.createElement('div');
    el.dataset.id = n.id;
    if (!def) {
      el.className = 'node unknown';
      el.innerHTML = `<b>Unknown block</b><div>${esc(n.kind)}</div>`;
      this.nodesEl.appendChild(el); this.els.set(n.id, el); this.place(n.id);
      return;
    }
    n.cfg = { ...structuredClone(def.defaults), ...(n.cfg || {}) };
    el.className = `node cat-${def.cat}${n.collapsed ? ' collapsed' : ''}`;
    el.style.setProperty('--c', CATS[def.cat].color);
    const isT = def.cat === 'tgt';
    const ports = isT
      ? `<i class="port out tgt" data-t="target" title="Drag to a Condition or Action to bind this target"></i>`
      : `<i class="port in" title="Input"></i>
         <i class="port out if" data-t="if" title="IF: follow when satisfied"></i>
         <i class="port out else" data-t="else" title="ELSE: follow when not satisfied"></i>
         <i class="port out after" data-t="after" title="AFTER: follow when finished"></i>
         <b class="pl if">IF</b><b class="pl else">ELSE</b><b class="pl after">AFTER</b>`;
    el.innerHTML = `
      <div class="n-head"><button class="n-chev" title="Collapse / expand settings">▾</button><span class="n-icon">${def.icon}</span><span class="n-title">${def.title}</span><span class="n-dot"></span></div>
      <div class="n-sum"></div><div class="n-detail"></div><div class="n-prog" hidden><i></i></div>
      <div class="n-body"></div><div class="n-warn"></div>${ports}`;
    el.querySelector('.n-chev').addEventListener('click', () => this.toggleCollapse(n));
    el.querySelector('.n-head').addEventListener('dblclick', e => { if (!e.target.closest('.n-chev')) this.toggleCollapse(n); });
    this.buildFields(n, el, def);
    this.nodesEl.appendChild(el); this.els.set(n.id, el);
    this.place(n.id); this.refresh(n);
    if (this.states[n.id]) this.paintState(n.id, this.states[n.id]);
  }

  toggleCollapse(n) {
    n.collapsed = !n.collapsed;
    this.els.get(n.id)?.classList.toggle('collapsed', n.collapsed);
    this.scheduleWires(); this.changed();
  }

  buildFields(n, el, def) {
    const body = el.querySelector('.n-body');
    for (const f of def.fields) {
      const row = document.createElement('label');
      row.className = 'fld'; row.dataset.key = f.key;
      row.innerHTML = `<span class="fl">${f.label}${f.unit ? ` <em>${f.unit}</em>` : ''}</span>`;
      let inp;
      if (f.type === 'select') {
        inp = document.createElement('select');
        inp.innerHTML = f.options.map(([v, l]) => `<option value="${v}">${l}</option>`).join('');
        inp.value = n.cfg[f.key];
      } else if (f.type === 'bool') {
        inp = document.createElement('input'); inp.type = 'checkbox'; inp.checked = !!n.cfg[f.key];
        row.classList.add('chk'); row.prepend(inp);
      } else {
        inp = document.createElement('input');
        inp.type = f.type === 'number' ? 'number' : 'text';
        if (f.type === 'number') { inp.min = f.min; inp.max = f.max; inp.step = f.step || 1; }
        if (f.placeholder) inp.placeholder = f.placeholder;
        inp.spellcheck = false;
        inp.value = n.cfg[f.key] ?? '';
      }
      inp.addEventListener(f.type === 'select' || f.type === 'bool' ? 'change' : 'input', () => {
        n.cfg[f.key] = f.type === 'bool' ? inp.checked : f.type === 'number' ? (inp.value === '' ? null : Number(inp.value)) : inp.value;
        this.refresh(n); this.changed('f' + n.id + f.key);
      });
      if (f.type !== 'bool') row.appendChild(inp);
      if (f.type === 'file') {
        const b = document.createElement('button');
        b.className = 'btn'; b.textContent = 'Browse…';
        b.addEventListener('click', async () => {
          const p = await this.hooks.pickFile();
          if (p) { inp.value = p; inp.dispatchEvent(new Event('input')); }
        });
        row.appendChild(b);
      }
      if (f.help) { const h = document.createElement('small'); h.className = 'fh'; h.textContent = f.help; row.appendChild(h); }
      body.appendChild(row);
    }
    if (def.help) { const h = document.createElement('div'); h.className = 'nhelp'; h.textContent = def.help; body.appendChild(h); }
  }

  issues(n) {
    const def = BLOCKS[n.kind];
    if (!def) return ['Unknown block type'];
    const out = validate(n.kind, n.cfg);
    if (def.requires) {
      const ok = this.auto.edges.some(e => e.kind === 'target' && e.to === n.id && this.node(e.from)?.kind === def.requires);
      if (!ok) out.push(`Wire a "${BLOCKS[def.requires].title}" target into this block`);
    }
    return out;
  }

  refresh(n) {
    const el = this.els.get(n.id), def = BLOCKS[n.kind];
    if (!el || !def) return;
    el.classList.toggle('off', !!n.disabled);
    el.querySelector('.n-sum').textContent = (n.disabled ? 'Disabled · ' : '') + def.summary(n.cfg);
    el.querySelectorAll('.fld').forEach(r => {
      const f = def.fields.find(x => x.key === r.dataset.key);
      r.hidden = !!(f.show && !f.show(n.cfg));
      r.classList.toggle('bad', !!fieldError(f, n.cfg));
    });
    const issues = n.disabled ? [] : this.issues(n), w = el.querySelector('.n-warn');
    w.textContent = issues[0] || ''; w.title = issues.join('\n');
  }

  toggleDisabled() {
    const ns = this.auto.nodes.filter(n => this.sel.has(n.id) && BLOCKS[n.kind]?.cat !== 'tgt');
    if (!ns.length) return;
    const off = ns.some(n => !n.disabled);
    for (const n of ns) { n.disabled = off; this.refresh(n); }
    this.scheduleWires();
    this.changed();
  }
  refreshAll() { for (const n of this.auto.nodes) this.refresh(n); }

  paintState(id, st) {
    const el = this.els.get(id); if (!el) return;
    for (const s of ['idle', 'running', 'waiting', 'done', 'failed']) el.classList.remove('st-' + s);
    el.classList.add('st-' + st.state);
    el.querySelector('.n-detail').textContent = st.detail || '';
    const pr = el.querySelector('.n-prog');
    if (pr) {
      if (st.progress != null) { pr.hidden = false; pr.querySelector('i').style.width = Math.round(clamp(st.progress, 0, 1) * 100) + '%'; }
      else pr.hidden = true;
    }
  }

  // ---------- add / delete / clipboard ----------
  addNode(kind, wp, pending) {
    const def = BLOCKS[kind];
    const n = { id: uid(), kind, x: Math.round(wp.x), y: Math.round(wp.y), collapsed: false, disabled: false, cfg: structuredClone(def.defaults) };
    if (pending) { n.x = Math.round(wp.x + 30); n.y = Math.round(wp.y - PORT.in); }
    this.auto.nodes.push(n); this.addEl(n);
    if (pending) this.connect(pending.from, n.id, pending.kind);
    this.select([n.id]); this.refreshAll(); this.changed();
  }

  deleteSel() {
    if (this.selEdge) {
      this.auto.edges = this.auto.edges.filter(e => e.id !== this.selEdge);
      this.selEdge = null;
    }
    if (this.sel.size) {
      const ids = this.sel;
      this.auto.nodes = this.auto.nodes.filter(n => !ids.has(n.id));
      this.auto.edges = this.auto.edges.filter(e => !ids.has(e.from) && !ids.has(e.to));
      for (const id of ids) { this.els.get(id)?.remove(); this.els.delete(id); delete this.states[id]; }
      this.sel = new Set();
    }
    this.refreshAll(); this.scheduleWires(); this.changed();
  }

  copy() {
    if (!this.sel.size) return;
    const ids = this.sel;
    this.clip = {
      nodes: this.auto.nodes.filter(n => ids.has(n.id)).map(n => structuredClone(n)),
      edges: this.auto.edges.filter(e => ids.has(e.from) && ids.has(e.to)).map(e => structuredClone(e)),
    };
  }

  paste(at) {
    const c = this.clip;
    if (!c || !c.nodes.length) return;
    const minX = Math.min(...c.nodes.map(n => n.x)), minY = Math.min(...c.nodes.map(n => n.y));
    const ox = at ? Math.round(at.x - minX) : 40, oy = at ? Math.round(at.y - minY) : 40;
    const map = new Map(), created = [];
    for (const n of c.nodes) {
      const m = structuredClone(n); m.id = uid(); m.x = n.x + ox; m.y = n.y + oy;
      map.set(n.id, m.id); created.push(m.id);
      this.auto.nodes.push(m); this.addEl(m);
    }
    for (const e of c.edges) this.auto.edges.push({ ...structuredClone(e), id: uid(), from: map.get(e.from), to: map.get(e.to) });
    if (!at) c.nodes.forEach(n => { n.x += ox; n.y += oy; });
    this.select(created); this.refreshAll(); this.scheduleWires(); this.changed();
  }

  duplicate() { this.copy(); this.paste(null); }

  // ---------- menus ----------
  ctxmenu(e) {
    e.preventDefault();
    const nodeEl = e.target.closest('.node');
    if (nodeEl) {
      const id = nodeEl.dataset.id;
      if (!this.sel.has(id)) this.select([id]);
      this.nodeMenu(e.clientX, e.clientY, id);
    } else this.openMenu(e.clientX, e.clientY, null);
  }

  placeMenu(cx, cy, w, h) {
    const r = this.cv.getBoundingClientRect();
    this.menu.style.left = clamp(cx - r.left, 4, Math.max(4, r.width - w - 4)) + 'px';
    this.menu.style.top = clamp(cy - r.top, 4, Math.max(4, r.height - h - 4)) + 'px';
    this.menu.hidden = false;
  }
  hideMenu() { this.menu.hidden = true; this.menu.innerHTML = ''; this.menu.onclick = null; }

  nodeMenu(cx, cy, id) {
    const n = this.node(id), m = this.menu, isT = BLOCKS[n.kind]?.cat === 'tgt';
    m.className = 'ctx glass';
    m.innerHTML = `<div class="ml">
      <div class="mi" data-a="test"><span><b>Test this block</b><small>Runs it once with its current settings and wired targets</small></span></div>
      ${isT ? '' : `<div class="mi" data-a="off"><span><b>${n.disabled ? 'Enable' : 'Disable'} block</b><small>B · a disabled block is skipped; only its AFTER wires continue</small></span></div>`}
      <div class="mi" data-a="dup"><span><b>Duplicate</b><small>Ctrl+D</small></span></div>
      <div class="mi" data-a="col"><span><b>${n.collapsed ? 'Expand' : 'Collapse'} settings</b></span></div>
      <div class="mi" data-a="del"><span><b>Delete</b><small>Del</small></span></div></div>`;
    this.placeMenu(cx, cy, 300, 260);
    m.onclick = e => {
      const it = e.target.closest('.mi'); if (!it) return;
      this.hideMenu();
      const a = it.dataset.a;
      if (a === 'test') this.hooks.testBlock(id);
      else if (a === 'off') this.toggleDisabled();
      else if (a === 'dup') this.duplicate();
      else if (a === 'del') this.deleteSel();
      else this.toggleCollapse(n);
    };
  }

  openMenu(cx, cy, pending) {
    const wp = this.toWorld(cx, cy), m = this.menu;
    m.className = 'ctx glass'; m.onclick = null;
    m.innerHTML = `<input class="mq" placeholder="Search blocks…" spellcheck="false"><div class="ml"></div>`;
    this.placeMenu(cx, cy, 300, 390);
    const inp = m.querySelector('.mq'), list = m.querySelector('.ml');
    let idx = 0, items = [];
    const render = () => {
      const q = inp.value.trim().toLowerCase();
      items = Object.entries(BLOCKS).filter(([, b]) =>
        (!pending || b.cat !== 'tgt') && `${b.title} ${b.desc} ${CATS[b.cat].label}`.toLowerCase().includes(q));
      idx = Math.min(idx, Math.max(0, items.length - 1));
      list.innerHTML = items.map(([k, b], i) =>
        `<div class="mi${i === idx ? ' on' : ''}" data-k="${k}"><span class="mic" style="color:${CATS[b.cat].color}">${b.icon}</span><span><b>${b.title}</b><small>${CATS[b.cat].label} · ${b.desc}</small></span></div>`
      ).join('') || '<div class="mempty">No matching blocks</div>';
    };
    const pick = k => { this.hideMenu(); this.addNode(k, wp, pending); };
    inp.addEventListener('input', () => { idx = 0; render(); });
    inp.addEventListener('keydown', e => {
      if (e.key === 'ArrowDown') { idx = Math.min(items.length - 1, idx + 1); render(); e.preventDefault(); }
      else if (e.key === 'ArrowUp') { idx = Math.max(0, idx - 1); render(); e.preventDefault(); }
      else if (e.key === 'Enter' && items[idx]) pick(items[idx][0]);
      else if (e.key === 'Escape') this.hideMenu();
      e.stopPropagation();
    });
    list.addEventListener('click', e => { const it = e.target.closest('.mi'); if (it) pick(it.dataset.k); });
    render(); inp.focus();
  }
}
