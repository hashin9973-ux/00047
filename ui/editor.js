import { BLOCKS, CATS, validate, fieldError } from './blocks.js';

const W = 260; // must match .node width in styles.css
const PORT = { in: 21, if: 20, else: 40, after: 60, tgt: 21 };
const KINDS = {
  if: { label: 'IF', color: '#4ade80' },
  else: { label: 'ELSE', color: '#ff6b81' },
  after: { label: 'AFTER', color: '#5aa9ff' },
  target: { label: 'INPUT', color: '#3ddbd9' },
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

const GAP = 20; // minimum space kept between blocks
const polyPts = poly => {
  const out = [];
  for (let i = 0; i < poly.length - 1; i++) {
    const [x0, y0] = poly[i], [x1, y1] = poly[i + 1];
    const n = Math.max(1, Math.ceil(Math.hypot(x1 - x0, y1 - y0) / 10));
    for (let k = 0; k <= n; k++) out.push({ x: x0 + (x1 - x0) * k / n, y: y0 + (y1 - y0) * k / n });
  }
  return out;
};
const countHits = (pts, rs) => {
  let n = 0;
  for (const p of pts) for (const r of rs) if (p.x > r[0] && p.x < r[2] && p.y > r[1] && p.y < r[3]) { n++; break; }
  return n;
};
// candidate offsets around a point, nearest first (used to find free canvas space)
const OFFS = (() => {
  const a = [];
  for (let i = -20; i <= 20; i++) for (let j = -20; j <= 20; j++) a.push([i * 24, j * 24]);
  return a.sort((p, q) => Math.hypot(p[0], p[1]) - Math.hypot(q[0], q[1]));
})();

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
          <div class="eu-bar glass"><button data-k="if">IF</button><button data-k="else">ELSE</button><button data-k="after">AFTER</button><button data-del="1" title="Disconnect wire">Disconnect ✕</button></div>
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
  setActive(v) {
    this.active = v;
    if (v) this.scheduleWires();
  }

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

  rectOf(n) { return { x: n.x, y: n.y, w: W, h: this.hOf(n.id) }; }
  collide(a, b) { return a.x < b.x + b.w + GAP && a.x + a.w + GAP > b.x && a.y < b.y + b.h + GAP && a.y + a.h + GAP > b.y; }

  /** Would moving the dragged group by (dx, dy) overlap a block that is not part of the drag? */
  blocked(d, dx, dy) {
    for (const [id, o] of d.orig) {
      const r = { x: o.x + dx, y: o.y + dy, w: W, h: this.hOf(id) };
      for (const q of this.auto.nodes) {
        if (d.orig.has(q.id) || d.ignore.has(q.id)) continue;
        if (this.collide(r, this.rectOf(q))) return true;
      }
    }
    return false;
  }

  /** Moves the group toward the pointer in small steps, sliding along obstacles instead of passing through. */
  stepGroup(d, tx, ty) {
    let cx = d.cx, cy = d.cy;
    const n = Math.max(1, Math.ceil(Math.hypot(tx - cx, ty - cy) / 12));
    const sx = (tx - cx) / n, sy = (ty - cy) / n;
    for (let i = 0; i < n; i++) {
      if (!this.blocked(d, cx + sx, cy + sy)) { cx += sx; cy += sy; }
      else if (sx && !this.blocked(d, cx + sx, cy)) cx += sx;
      else if (sy && !this.blocked(d, cx, cy + sy)) cy += sy;
      else break;
    }
    d.cx = cx; d.cy = cy;
  }

  freeSpot(x, y, w, h, ignore) {
    const others = this.auto.nodes.filter(n => !ignore.has(n.id)).map(n => this.rectOf(n));
    for (const [ox, oy] of OFFS) {
      const r = { x: x + ox, y: y + oy, w, h };
      if (!others.some(q => this.collide(r, q))) return { x: r.x, y: r.y };
    }
    return { x, y };
  }

  /** After a block grows (expanded / fields shown), push blocks below it out of the way. */
  pushApart(id, depth = 0) {
    if (depth > 40) return;
    const n = this.node(id); if (!n) return;
    const r = this.rectOf(n);
    for (const o of this.auto.nodes) {
      if (o.id === id) continue;
      const q = this.rectOf(o);
      if (this.collide(r, q) && q.y + q.h / 2 >= r.y + r.h / 2) {
        o.y = r.y + r.h + GAP; this.place(o.id); this.pushApart(o.id, depth + 1);
      }
    }
  }

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
  cutWire(id, clientX, clientY) {
    const wire = this.wires.get(id), edge = this.auto.edges.find(e => e.id === id);
    if (!wire || !edge || wire.cutting) return;
    const sim = wire.physics;
    const particles = sim?.particles;
    if (particles?.length > 2) {
      const target = Number.isFinite(clientX) && Number.isFinite(clientY)
        ? this.toWorld(clientX, clientY)
        : wire.mid;
      let cutIndex = 1, best = Infinity;
      for (let i = 1; i < particles.length - 1; i++) {
        const d = Math.hypot(particles[i].x - target.x, particles[i].y - target.y);
        if (d < best) { best = d; cutIndex = i; }
      }
      const cut = { ...particles[cutIndex], px: particles[cutIndex].x, py: particles[cutIndex].y };
      const makeTail = points => {
        if (points.length < 2) return;
        const copied = points.map(p => ({ ...p }));
        copied[copied.length - 1].px = copied[copied.length - 1].x;
        copied[copied.length - 1].py = copied[copied.length - 1].y;
        this.wireDebris.push({ particles: copied, rest: sim.rest, born: performance.now(), path: svg('path', { class: 'wire wire-debris k-' + edge.kind }) });
        this.wl.appendChild(this.wireDebris[this.wireDebris.length - 1].path);
      };
      makeTail(particles.slice(0, cutIndex + 1).map(p => ({ ...p })).concat([{ ...cut }]));
      makeTail([{ ...cut }, ...particles.slice(cutIndex + 1).map(p => ({ ...p }))]);
    }
    const blade = svg('text', { class: 'wire-scissor', 'text-anchor': 'middle', 'dominant-baseline': 'central' });
    blade.textContent = '✂';
    const cutPoint = particles?.length
      ? particles.reduce((best, p) => !best || Math.hypot(p.x - this.toWorld(clientX, clientY).x, p.y - this.toWorld(clientX, clientY).y) < Math.hypot(best.x - this.toWorld(clientX, clientY).x, best.y - this.toWorld(clientX, clientY).y) ? p : best, null)
      : wire.mid || {x:0,y:0};
    blade.setAttribute('x', cutPoint.x); blade.setAttribute('y', cutPoint.y);
    this.wl.appendChild(blade);
    wire.cutting = true;
    wire.vis.classList.add('cutting'); wire.hit.style.pointerEvents = 'none';
    requestAnimationFrame(() => { wire.vis.classList.add('cut-animate'); blade.classList.add('snip'); });
    setTimeout(() => {
      blade.remove();
      this.auto.edges = this.auto.edges.filter(e => e.id !== id);
      if (this.selEdge === id) this.selEdge = null;
      this.refreshAll(); this.scheduleWires(); this.changed();
    }, 360);
  }

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
      const orig = new Map([...this.sel].map(i => { const n = this.node(i); return [i, { x: n.x, y: n.y }]; }));
      const ignore = new Set(); // blocks already touching the dragged ones do not block the drag
      for (const [oid, o] of orig) {
        const r = { x: o.x, y: o.y, w: W, h: this.hOf(oid) };
        for (const q of this.auto.nodes) if (!orig.has(q.id) && this.collide(r, this.rectOf(q))) ignore.add(q.id);
      }
      this.drag = { t: 'nodes', sx: e.clientX, sy: e.clientY, moved: false, id, mod, orig, ignore, cx: 0, cy: 0 };
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
      const tx = (e.clientX - d.sx) / this.vp.k, ty = (e.clientY - d.sy) / this.vp.k;
      if (e.altKey) { d.cx = tx; d.cy = ty; } else this.stepGroup(d, tx, ty); // Alt = move through other blocks
      for (const [id, o] of d.orig) { const n = this.node(id); n.x = Math.round(o.x + d.cx); n.y = Math.round(o.y + d.cy); this.place(id); }
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
      if (ed) {
        ed.bend = Math.round(d.orig + (e.clientY - d.sy) / this.vp.k);
        const wire = this.wires.get(ed.id);
        if (wire) wire.physics = null;
        this.scheduleWires();
      }
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
      if (ca !== 'tgt' || cb === 'tgt' || B.kind === 'act.start' || B.kind === 'act.loop') {
        this.hooks.toast('An Input can only bind to a Condition or Action that consumes it, such as Visible Windows or Launch File.');
        return false;
      }
    } else {
      if (!['if', 'else', 'after'].includes(kind) || ca === 'tgt') {
        this.hooks.toast('Use IF / ELSE / AFTER outputs for flow, and the teal output for an Input connection.');
        return false;
      }
if (A.kind === 'act.loop') {
        this.hooks.toast('Loop returns to Start automatically and has no output wires.');
        return false;
      }
    }

    if (this.auto.edges.some(e => e.from === from && e.to === to && e.kind === kind)) return false;
    this.auto.edges.push({ id: uid(), from, to, kind, bend: 0 });
    this.refreshAll(); this.scheduleWires(); this.changed();
    return true;
  }

  // ---------- wires ----------
  scheduleWires() { if (!this.raf) this.raf = requestAnimationFrame(() => { this.raf = 0; this.drawWires(); }); }

  // Verlet rope simulation: only wire particles receive gravity; blocks remain fixed unless dragged.
  initWirePhysics(e, a, b) {
    const distance = Math.hypot(b.x - a.x, b.y - a.y);
    const segments = clamp(Math.ceil(distance / 24), 8, 32);
    const slack = clamp(distance * 0.10, 6, 36);
    const rest = (distance + slack) / segments;
    const particles = [];
    for (let i = 0; i <= segments; i++) {
      const t = i / segments;
      const sag = Math.sin(Math.PI * t) * (Math.min(20, distance * 0.04) + (e.bend || 0));
      const x = a.x + (b.x - a.x) * t;
      const y = a.y + (b.y - a.y) * t + sag;
      particles.push({ x, y, px: x, py: y });
    }
    return { particles, rest, segments, a: { ...a }, b: { ...b } };
  }

  collideWireParticle(p, rects, anchors = []) {
    const radius = 4;
    // Keep a small clear lead-in at each connector, but let the cable collide with the rest of both connected blocks.
    if (anchors.some(a => Math.hypot(p.x - a.x, p.y - a.y) < 25)) return;
    for (const r of rects) {
      if (p.x < r[0] - radius || p.x > r[2] + radius || p.y < r[1] - radius || p.y > r[3] + radius) continue;
      const left = Math.abs(p.x - (r[0] - radius));
      const right = Math.abs((r[2] + radius) - p.x);
      const top = Math.abs(p.y - (r[1] - radius));
      const bottom = Math.abs((r[3] + radius) - p.y);
      const edge = Math.min(left, right, top, bottom);
      if (edge === left) p.x = r[0] - radius;
      else if (edge === right) p.x = r[2] + radius;
      else if (edge === top) p.y = r[1] - radius;
      else p.y = r[3] + radius;
      p.px = p.x; p.py = p.y;
    }
  }

  simulateWire(e, f, t, w, dt) {
    const a = { x: f.x + W, y: f.y + PORT[e.kind === 'target' ? 'tgt' : e.kind] };
    const b = { x: t.x, y: t.y + PORT.in };
    if (!w.physics) w.physics = this.initWirePhysics(e, a, b);
    const sim = w.physics, ps = sim.particles;
    // Carry the rope ends with their ports while preserving the rope's momentum.
    const da = { x: a.x - sim.a.x, y: a.y - sim.a.y };
    const db = { x: b.x - sim.b.x, y: b.y - sim.b.y };
    for (let i = 1; i < ps.length - 1; i++) {
      const u = i / (ps.length - 1);
      ps[i].x += da.x * (1 - u) + db.x * u;
      ps[i].y += da.y * (1 - u) + db.y * u;
      // Heavier cable: stronger gravity, more damping, and less jitter from endpoint motion.
      const vx = (ps[i].x - ps[i].px) * 0.91;
      const vy = (ps[i].y - ps[i].py) * 0.91;
      ps[i].px = ps[i].x;
      ps[i].py = ps[i].y;
      const scale = clamp(dt / 16.667, 0.5, 2);
      ps[i].x += vx;
      ps[i].y += vy + 0.62 * scale * scale;
    }
    const last = ps.length - 1;
    ps[0].x = a.x; ps[0].y = a.y; ps[0].px = a.x; ps[0].py = a.y;
    ps[last].x = b.x; ps[last].y = b.y; ps[last].px = b.x; ps[last].py = b.y;

    // Include connected blocks too, so the cable cannot pass through their bodies.
    const rects = this.calcRects().map(o => o.r);
    const anchors = [a, b];
    const distance = Math.hypot(b.x - a.x, b.y - a.y);
    sim.rest = (distance + clamp(distance * 0.10, 6, 36)) / last;
    for (let iteration = 0; iteration < 9; iteration++) {
      // Fixed-length segments constrain the cable while gravity creates a natural sag.
      for (let i = 0; i < last; i++) {
        const p = ps[i], q = ps[i + 1];
        const dx = q.x - p.x, dy = q.y - p.y;
        const distance = Math.hypot(dx, dy) || 0.001;
        const correction = (distance - sim.rest) / distance;
        const cx = dx * correction, cy = dy * correction;
        if (i === 0) { q.x -= cx; q.y -= cy; }
        else if (i + 1 === last) { p.x += cx; p.y += cy; }
        else { p.x += cx * 0.5; p.y += cy * 0.5; q.x -= cx * 0.5; q.y -= cy * 0.5; }
      }
      ps[0].x = a.x; ps[0].y = a.y;
      ps[last].x = b.x; ps[last].y = b.y;
      for (let i = 1; i < last; i++) this.collideWireParticle(ps[i], rects, anchors);
    }
    sim.a = { ...a }; sim.b = { ...b };
    let d = `M${ps[0].x},${ps[0].y}`;
    for (let i = 1; i < last; i++) {
      const p = ps[i], q = ps[i + 1];
      d += ` Q${p.x},${p.y} ${(p.x + q.x) / 2},${(p.y + q.y) / 2}`;
    }
    d += ` Q${ps[last - 1].x},${ps[last - 1].y} ${b.x},${b.y}`;
    return { d, mid: { x: ps[Math.floor(last / 2)].x, y: ps[Math.floor(last / 2)].y } };
  }

  hOf(id) { return this.els.get(id)?.offsetHeight || 90; }

  calcRects() {
    return this.auto.nodes.map(n => ({ id: n.id, r: [n.x - 10, n.y - 10, n.x + W + 10, n.y + this.hOf(n.id) + 10] }));
  }

  route(e, f, t) {
    const a = { x: f.x + W, y: f.y + PORT[e.kind === 'target' ? 'tgt' : e.kind] };
    const b = { x: t.x, y: t.y + PORT.in };
    const rs = (this.rcache || (this.rcache = this.calcRects())).filter(o => o.id !== f.id && o.id !== t.id).map(o => o.r);
    const bend = e.bend || 0, dxp = b.x - a.x;

    if (dxp >= 60) {
      const mx = (a.x + b.x) / 2, my0 = (a.y + b.y) / 2;
      const direct = () => {
        const c = clamp(dxp * 0.45, 28, 110);
        const P = [a, { x: a.x + c, y: a.y }, { x: b.x - c, y: b.y }, b];
        const pts = [];
        for (let i = 0; i <= 20; i++) pts.push(cub(P[0], P[1], P[2], P[3], i / 20));
        return { d: `M${a.x},${a.y} C${P[1].x},${P[1].y} ${P[2].x},${P[2].y} ${b.x},${b.y}`, mid: cub(P[0], P[1], P[2], P[3], 0.5), pts };
      };
      const via = off => {
        const c = clamp(dxp * 0.3, 24, 90), my = my0 + off;
        const P = [a, { x: a.x + c, y: a.y }, { x: mx - c * 0.8, y: my }, { x: mx, y: my }, { x: mx + c * 0.8, y: my }, { x: b.x - c, y: b.y }, b];
        const pts = [];
        for (let i = 0; i <= 12; i++) { pts.push(cub(P[0], P[1], P[2], P[3], i / 12)); pts.push(cub(P[3], P[4], P[5], P[6], i / 12)); }
        return { d: `M${a.x},${a.y} C${P[1].x},${P[1].y} ${P[2].x},${P[2].y} ${mx},${my} S${P[5].x},${P[5].y} ${b.x},${b.y}`, mid: { x: mx, y: my }, pts };
      };
      if (bend) return via(bend);
      let best = direct(), bestScore = countHits(best.pts, rs) * 1000;
      if (bestScore) {
        for (const off of [30, -30, 60, -60, 100, -100, 150, -150]) {
          const c = via(off), s = countHits(c.pts, rs) * 1000 + Math.abs(off);
          if (s < bestScore) { best = c; bestScore = s; if (s < 1000) break; }
        }
      }
      return best;
    }

    // Close or backward blocks: compact orthogonal route through the gap between them, else around them.
    const x1 = a.x + 22, x2 = b.x - 22;
    const fTop = f.y, fBot = f.y + this.hOf(f.id), tTop = t.y, tBot = t.y + this.hOf(t.id);
    const lanes = [];
    if (tTop > fBot + 14) lanes.push((fBot + tTop) / 2);
    else if (fTop > tBot + 14) lanes.push((tBot + fTop) / 2);
    const hasGap = lanes.length === 1;
    const over = Math.min(fTop, tTop) - 26;
    lanes.push(Math.max(fBot, tBot) + 26, over);
    const mk = lane => {
      const poly = [[a.x, a.y], [x1, a.y], [x1, lane], [x2, lane], [x2, b.y], [b.x, b.y]];
      return { d: rounded(poly, 12), mid: { x: (x1 + x2) / 2, y: lane }, pts: polyPts(poly) };
    };
    if (bend) return mk(lanes[0] + bend);
    let best = null;
    for (let li = 0; li < lanes.length; li++) {
      const lane = lanes[li], maxK = hasGap && li === 0 ? 1 : 5, dir = lane === over ? -1 : 1;
      for (let k = 0; k < maxK; k++) {
        const y = lane + dir * 28 * k, c = mk(y);
        const s = countHits(c.pts, rs) * 1000 + Math.abs(y - (a.y + b.y) / 2) * 0.2 + (li === 0 ? 0 : 5);
        if (!best || s < best.s) best = { c, s };
        if (s < 1000) break;
      }
      if (best && best.s < 1000) break;
    }
    return best.c;
  }

  drawWires() {
    const now = performance.now();
    if (!this.wireDebris) this.wireDebris = [];
    this.updateWireDebris(now);
    const dt = this.lastPhysicsTime ? Math.min(32, now - this.lastPhysicsTime) : 16.667;
    this.lastPhysicsTime = now;
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
        w.hit.addEventListener('pointerenter', () => { if (this.wires.get(id)) this.wires.get(id).vis.classList.add('hover'); });
        w.hit.addEventListener('pointerleave', () => { if (this.wires.get(id)) this.wires.get(id).vis.classList.remove('hover'); });
        w.hit.addEventListener('contextmenu', ev => { ev.preventDefault(); ev.stopPropagation(); this.cutWire(id, ev.clientX, ev.clientY); });
        this.wl.appendChild(w.hit); this.wl.appendChild(w.vis);
        this.wires.set(e.id, w);
      }
      const { d, mid } = this.simulateWire(e, f, t, w, dt);
      w.mid = mid;
      w.hit.setAttribute('d', d); w.vis.setAttribute('d', d);
      w.vis.setAttribute('class', `wire k-${e.kind}${this.selEdge === e.id ? ' sel' : ''}`);
      w.vis.setAttribute('marker-end', `url(#ar-${e.kind})`);
    }
    for (const [id, w] of this.wires) if (!seen.has(id)) { w.hit.remove(); w.vis.remove(); this.wires.delete(id); }
    this.placeEdgeUI();
    // Keep the rope simulation alive only while the editor is visible.
    if (this.active && (this.auto.edges.length || this.wireDebris.length) && !this.raf) {
      this.raf = requestAnimationFrame(() => { this.raf = 0; this.drawWires(); });
    }
  }

  updateWireDebris(now) {
    if (!this.wireDebris) this.wireDebris = [];
    const gravity = 0.62;
    for (let i = this.wireDebris.length - 1; i >= 0; i--) {
      const tail = this.wireDebris[i], ps = tail.particles, last = ps.length - 1;
      const age = now - tail.born;
      if (age > 2200 || ps.length < 2) {
        tail.path.remove();
        this.wireDebris.splice(i, 1);
        continue;
      }
      for (let j = 0; j <= last; j++) {
        const p = ps[j];
        const vx = (p.x - p.px) * 0.94, vy = (p.y - p.py) * 0.94;
        p.px = p.x; p.py = p.y;
        p.x += vx; p.y += vy + gravity;
      }
      for (let iteration = 0; iteration < 4; iteration++) {
        for (let j = 0; j < last; j++) {
          const p = ps[j], q = ps[j + 1];
          const dx = q.x - p.x, dy = q.y - p.y;
          const d = Math.hypot(dx, dy) || 0.001;
          const correction = (d - tail.rest) / d;
          p.x += dx * correction * 0.5; p.y += dy * correction * 0.5;
          q.x -= dx * correction * 0.5; q.y -= dy * correction * 0.5;
        }
      }
      let d = `M${ps[0].x},${ps[0].y}`;
      for (let j = 1; j < last; j++) {
        const p = ps[j], q = ps[j + 1];
        d += ` Q${p.x},${p.y} ${(p.x + q.x) / 2},${(p.y + q.y) / 2}`;
      }
      d += ` L${ps[last].x},${ps[last].y}`;
      tail.path.setAttribute('d', d);
      tail.path.style.opacity = String(Math.max(0, 1 - age / 2200));
    }
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
    const isStart = n.kind === 'act.start';
    const isLoop = n.kind === 'act.loop';
    const flowOut = `<i class="port out if" data-t="if" title="IF: continue on success / satisfied"></i>
         <i class="port out else" data-t="else" title="ELSE: follow when not satisfied or configured failure"></i>
         <i class="port out after" data-t="after" title="AFTER: timeout/completion route"></i>
         <b class="pl if">IF</b><b class="pl else">ELSE</b><b class="pl after">AFTER</b>`;
    const ports = isT
      ? `<i class="port in tgt-in" title="Flow input"></i>
         <i class="port out tgt" data-t="target" title="Input output: bind this input and continue the workflow"></i>`
      : isLoop
        ? `<i class="port in" title="Flow input. Loop returns automatically to Start."></i>`
        : `<i class="port in" title="Flow input. Multiple incoming wires may converge here."></i>${flowOut}`;
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
    if (!n.collapsed) this.pushApart(n.id);
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
        inp.type = f.type === 'number' ? 'number' : f.type === 'time' ? 'time' : 'text';
        if (f.type === 'number') { inp.min = f.min; inp.max = f.max; inp.step = f.step || 1; }
        if (f.placeholder) inp.placeholder = f.placeholder;
        inp.spellcheck = false;
        inp.value = n.cfg[f.key] ?? '';
      }
      inp.addEventListener(f.type === 'select' || f.type === 'bool' ? 'change' : 'input', () => {
        n.cfg[f.key] = f.type === 'bool' ? inp.checked : f.type === 'number' ? (inp.value === '' ? null : Number(inp.value)) : inp.value;
this.refresh(n); if (!n.collapsed) this.pushApart(n.id); this.changed('f' + n.id + f.key);
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

  hasTarget(kind, id) {
    const stack = [id], seen = new Set();
    while (stack.length) {
      const current = stack.pop();
      if (seen.has(current)) continue;
      seen.add(current);
      for (const e of this.auto.edges) {
        if (e.to !== current) continue;
        const src = this.node(e.from);
        if (!src) continue;
        if (e.kind === 'target' && src.kind === kind) return true;
        if (['if', 'else', 'after'].includes(e.kind)) {
          if (BLOCKS[src.kind]?.cat === 'tgt') {
            if (src.kind === kind) return true;
          } else {
            stack.push(src.id);
          }
        }
      }
    }
    // Preserve the supported form where a required Input is placed directly after the consumer.
    return this.auto.edges.some(e => e.from === id && e.kind !== 'target' && this.node(e.to)?.kind === kind);
  }

  issues(n) {
    const def = BLOCKS[n.kind];
    if (!def) return ['Unknown block type'];
    const out = validate(n.kind, n.cfg);
    if (def.requires && !this.hasTarget(def.requires, n.id)) {
      out.push(`Connect the required "${BLOCKS[def.requires].title}" to this block or earlier in its flow`);
    }
    if (n.kind === 'act.loop') {
      const starts = this.auto.nodes.filter(x => x.kind === 'act.start' && !x.disabled);
      if (starts.length !== 1) out.push('Loop requires exactly one enabled Start block in this automation');
      if (!this.auto.edges.some(e => e.to === n.id && ['if', 'else', 'after'].includes(e.kind))) {
        out.push('Connect the last block in the flow to Loop; Loop returns to Start automatically.');
      }
      if (this.auto.edges.some(e => e.from === n.id && ['if', 'else', 'after'].includes(e.kind))) {
        out.push('Loop has no output wires now; disconnect its old outgoing wires. It jumps to Start automatically.');
      }
    }
    return out;
  }

  refresh(n) {
    const el = this.els.get(n.id), def = BLOCKS[n.kind];
    if (!el || !def) return;
    el.classList.toggle('off', !!n.disabled);
    const showElse = def.cat === 'cond' || ['else', 'after', 'abort'].includes(n.cfg.onFail);
    const showAfter = def.cat === 'cond' || Number(n.cfg.timeoutSec) > 0 || n.cfg.onFail === 'after';
    for (const [selector, visible] of [['.port.out.else', showElse], ['.pl.else', showElse], ['.port.out.after', showAfter], ['.pl.after', showAfter]]) {
      const port = el.querySelector(selector);
      if (port) port.hidden = !visible;
    }
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
    const p = this.freeSpot(n.x, n.y, W, this.hOf(n.id), new Set([n.id]));
    n.x = Math.round(p.x); n.y = Math.round(p.y); this.place(n.id);
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
      nodes: this.auto.nodes.filter(n => ids.has(n.id)).map(n => Object.assign(structuredClone(n), { _h: this.hOf(n.id) })),
      edges: this.auto.edges.filter(e => ids.has(e.from) && ids.has(e.to)).map(e => structuredClone(e)),
    };
  }

  paste(at) {
    const c = this.clip;
    if (!c || !c.nodes.length) return;
    const minX = Math.min(...c.nodes.map(n => n.x)), minY = Math.min(...c.nodes.map(n => n.y));
    const bx = at ? at.x - minX : 40, by = at ? at.y - minY : 40;
    const fits = (ox, oy) => !c.nodes.some(n => {
      const r = { x: n.x + ox, y: n.y + oy, w: W, h: n._h || 90 };
      return this.auto.nodes.some(q => this.collide(r, this.rectOf(q)));
    });
    let ox = Math.round(bx), oy = Math.round(by);
    for (const [dx, dy] of OFFS) { if (fits(bx + dx, by + dy)) { ox = Math.round(bx + dx); oy = Math.round(by + dy); break; } }
    const map = new Map(), created = [];
    for (const n of c.nodes) {
      const m = structuredClone(n); delete m._h;
      m.id = uid(); m.x = n.x + ox; m.y = n.y + oy;
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
      <div class="mi" data-a="test"><span><b>Test this block</b><small>Runs it once with its current settings and wired inputs</small></span></div>
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
