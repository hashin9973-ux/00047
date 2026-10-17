import { Editor } from './editor.js';
import { BLOCKS, registerSpecs } from './blocks.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__?.window?.getCurrentWindow?.();

const $ = (s, r = document) => r.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const pad = n => String(n).padStart(2, '0');
const uid = () => (crypto.randomUUID ? crypto.randomUUID() : 'a' + Math.random().toString(36).slice(2) + Date.now().toString(36));
const fmtTime = ts => { const d = new Date(ts); return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${String(d.getMilliseconds()).padStart(3, '0')}`; };
const fmtDur = ms => {
  let s = Math.max(0, Math.floor(ms / 1000)); const h = Math.floor(s / 3600); s %= 3600; const m = Math.floor(s / 60); s %= 60;
  return h ? `${h}h ${pad(m)}m ${pad(s)}s` : m ? `${m}m ${pad(s)}s` : `${s}s`;
};
const fmtAgo = ts => {
  const s = Math.floor((Date.now() - ts) / 1000);
  if (s < 5) return 'just now'; if (s < 60) return `${s}s ago`; if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`; return new Date(ts).toLocaleDateString();
};

const LABEL = { running: 'Running', stopped: 'Stopped', stopping: 'Stopping…', completed: 'Completed', failed: 'Failed' };
const S = { autos: [], rt: { autos: {}, nodes: {} }, logs: [], view: 'overview', editId: null, vis: true, hist: [], tab: 'log', lf: { q: '', level: '', auto: '' }, q: '', edSt: '' };
const stOf = id => S.rt.autos[id]?.status || 'stopped';
const isRun = id => ['running', 'stopping'].includes(stOf(id));
const active = () => S.vis && !document.hidden;

 
// Custom transparent window caption controls (separate from the in-app tabs).
const windowTitlebar = $('#window-titlebar');
const maximizeButton = $('#window-maximize');
const runWindowAction = action => {
  try {
    return Promise.resolve(action()).catch(e => toast('Window control failed: ' + (e?.message || e), 'err'));
  } catch (e) {
    toast('Window control failed: ' + (e?.message || e), 'err');
    return Promise.resolve();
  }
};
if (appWindow && windowTitlebar) {
  const updateMaximizeButton = async () => {
    try {
      const maximized = await appWindow.isMaximized();
      maximizeButton.textContent = maximized ? '❐' : '□';
      maximizeButton.title = maximized ? 'Restore' : 'Maximize';
      maximizeButton.setAttribute('aria-label', maximized ? 'Restore window' : 'Maximize window');
    } catch { /* Window may be transitioning during shutdown. */ }
  };
  $('#window-minimize').addEventListener('click', () => runWindowAction(() => appWindow.minimize()));
  maximizeButton.addEventListener('click', () => runWindowAction(() => appWindow.toggleMaximize()).then(updateMaximizeButton));
  $('#window-close').addEventListener('click', () => runWindowAction(() => appWindow.close()));
  windowTitlebar.addEventListener('dblclick', e => {
    if (e.target.closest('.window-controls')) return;
    runWindowAction(() => appWindow.toggleMaximize()).then(updateMaximizeButton);
  });
  updateMaximizeButton();
}

// ---------- toast / modal ----------
function toast(msg, kind = '') {
  const t = document.createElement('div');
  t.className = 'toast ' + kind; t.textContent = msg;
  $('#toasts').appendChild(t);
  setTimeout(() => t.remove(), kind === 'err' ? 8000 : 4500);
}
function modal({ title, body, buttons }) {
  return new Promise(res => {
    const m = $('#modal');
    m.innerHTML = `<div class="mbox glass"><h3>${esc(title)}</h3><p>${esc(body)}</p><div class="mbtn">${buttons.map(([v, l, c]) => `<button class="btn ${c || ''}" data-v="${v}">${esc(l)}</button>`).join('')}</div></div>`;
    m.hidden = false;
    m.onclick = e => {
      const b = e.target.closest('button');
      if (b || e.target === m) { m.hidden = true; m.innerHTML = ''; res(b ? b.dataset.v : 'cancel'); }
    };
  });
}

// ---------- auto-save ----------
const ind = $('#save-ind');
const setInd = (t, c) => { ind.textContent = t; ind.className = 'save ' + c; };
const pending = new Map();
let saveTimer = 0, chain = Promise.resolve(), busy = 0;

function markDirty(a) {
  pending.set(a.id, a);
  setInd('Saving…', 'busy');
  clearTimeout(saveTimer);
  saveTimer = setTimeout(flushSaves, 600);
}
function flushSaves() {
  clearTimeout(saveTimer);
  const items = [...pending.values()];
  pending.clear();
  if (!items.length) return chain;
  const snaps = items.map(a => JSON.parse(JSON.stringify(a)));
  busy++;
  chain = chain.then(async () => {
    let err = null;
    for (const s of snaps) {
      try { await invoke('save_automation', { auto: s }); }
      catch (e) {
        err = String(e);
        const live = S.autos.find(a => a.id === s.id);
        if (live && !pending.has(live.id)) pending.set(live.id, live);
      }
    }
    busy--;
    if (err) setInd(`Save failed: ${err} (click to retry)`, 'err');
    else if (!busy && !pending.size) setInd('Saved', 'ok');
  });
  return chain;
}
ind.addEventListener('click', () => { if (ind.classList.contains('err')) flushSaves(); });
window.addEventListener('beforeunload', () => { flushSaves(); });

// ---------- editor ----------
const editor = new Editor($('#ed-host'), {
  onChange: a => {
    markDirty(a);
    if (isRun(a.id)) $('#ed-note').textContent = 'Running a saved snapshot. Restart to apply edits.';
  },
  toast,
  pickFile: async () => { try { return await invoke('pick_file'); } catch (e) { toast('File picker failed: ' + e, 'err'); return null; } },
  testBlock: id => testBlock(id),
});

async function testBlock(nodeId) {
  const a = S.autos.find(x => x.id === S.editId);
  const n = a && a.nodes.find(x => x.id === nodeId);
  if (!n) return;
if (BLOCKS[n.kind]?.cat === 'act') {
    const c = await modal({
      title: 'Test this block?',
      body: "Actions run for real (for example a trim or a launch). The test uses the block's current settings and wired targets.",
      buttons: [['cancel', 'Cancel'], ['go', 'Run test', 'primary']],
    });
    if (c !== 'go') return;
  }
  toast('Testing…');
  try {
    const result = await invoke('test_block', { auto: JSON.parse(JSON.stringify(a)), nodeId });
    toast('Test result: ' + result);
  } catch (e) {
    toast('Test failed: ' + String(e?.message || e).split('\n').join(' • '), 'err');
  }
}
$('#ed-undo').addEventListener('click', () => editor.doUndo());
$('#ed-redo').addEventListener('click', () => editor.doRedo());


// ---------- log filter + CSV export ----------
const logMatch = e => {
  const { q, level, auto } = S.lf;
  if (level && e.level !== level) return false;
  if (auto === '__app' ? e.autoId !== '' : auto && e.autoId !== auto) return false;
  if (q && !`${e.msg} ${e.autoName} ${e.nodeLabel || ''}`.toLowerCase().includes(q)) return false;
  return true;
};
const csvCell = v => {
  let s = String(v ?? '');
  if (/^[=+\-@]/.test(s)) s = "'" + s;
  return /[",\r\n]/.test(s) ? '"' + s.replace(/"/g, '""') + '"' : s;
};
async function exportLogs() {
  const rows = S.logs.filter(logMatch);
  if (!rows.length) { toast('No log entries to export'); return; }
  const day = ts => { const d = new Date(ts); return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${fmtTime(ts)}`; };
  const csv = ['time,level,automation,block,message',
    ...rows.map(e => [day(e.ts), e.level, e.autoName, e.nodeLabel || '', e.msg].map(csvCell).join(','))].join('\r\n') + '\r\n';
  try { if (await invoke('export_logs', { csv })) toast(`Exported ${rows.length} entries`); }
  catch (e) { toast('Log export failed: ' + e, 'err'); }
}

// ---------- Ctrl+K command palette ----------
const pal = document.createElement('div');
pal.id = 'cmdk'; pal.hidden = true;
pal.innerHTML = '<div class="ck-box glass"><input class="ck-in" placeholder="Type a command…" spellcheck="false"><div class="ck-list"></div></div>';
document.body.appendChild(pal);
const ckIn = $('.ck-in', pal), ckList = $('.ck-list', pal);
const ck = { items: [], i: 0 };

function ckCommands() {
  const go = v => () => { if (S.view === 'editor') flushSaves(); show(v); };
  const c = [
{ t: 'Manage custom blocks…', k: 'Blocks', f: openBlocksManager },
    { t: 'New automation', k: 'Action', f: newAuto },
    { t: 'Import automation…', k: 'Action', f: importAuto },
    { t: 'Go to Overview', k: 'View', f: go('overview') },
    { t: 'Go to Automations', k: 'View', f: go('automations') },
    { t: 'Go to Logs', k: 'View', f: go('logs') },
    { t: 'Export logs (CSV)…', k: 'Logs', f: exportLogs },
  ];
  if (S.view === 'editor') c.unshift(
    { t: 'Add block…', k: 'Editor', f: () => editor.openMenuCenter() },
    { t: 'Undo', k: 'Editor', f: () => editor.doUndo() },
    { t: 'Redo', k: 'Editor', f: () => editor.doRedo() },
    { t: 'Fit view', k: 'Editor', f: () => editor.fit() },
  );
  for (const a of S.autos) {
    c.push({ t: `Open: ${a.name}`, k: 'Automation', f: () => openEditor(a.id) });
    c.push(isRun(a.id)
      ? { t: `Stop: ${a.name}`, k: 'Automation', f: () => control('stop', a.id) }
      : { t: `Start: ${a.name}`, k: 'Automation', f: () => control('start', a.id) });
    c.push({ t: `Restart: ${a.name}`, k: 'Automation', f: () => control('restart', a.id) });
  }
  return c;
}
function ckRender() {
  const terms = ckIn.value.toLowerCase().split(/\s+/).filter(Boolean);
  ck.items = ckCommands().filter(c => terms.every(t => `${c.t} ${c.k}`.toLowerCase().includes(t))).slice(0, 50);
  ck.i = Math.min(ck.i, Math.max(0, ck.items.length - 1));
  ckList.innerHTML = ck.items.map((c, i) => `<div class="ck-item${i === ck.i ? ' on' : ''}" data-i="${i}"><span>${esc(c.t)}</span><small>${esc(c.k)}</small></div>`).join('')
    || '<div class="ck-empty">No matching commands</div>';
  ckList.querySelector('.on')?.scrollIntoView({ block: 'nearest' });
}
const ckClose = () => { pal.hidden = true; };
function ckOpen() {
  if (!$('#modal').hidden) return;
  ck.i = 0; ckIn.value = ''; pal.hidden = false; ckRender(); ckIn.focus();
}
function ckRun(i) { const c = ck.items[i]; if (!c) return; ckClose(); setTimeout(c.f, 0); }
ckIn.addEventListener('input', () => { ck.i = 0; ckRender(); });
ckIn.addEventListener('keydown', e => {
  if (e.key === 'ArrowDown') { ck.i = Math.min(ck.items.length - 1, ck.i + 1); ckRender(); e.preventDefault(); }
  else if (e.key === 'ArrowUp') { ck.i = Math.max(0, ck.i - 1); ckRender(); e.preventDefault(); }
  else if (e.key === 'Enter') { ckRun(ck.i); e.preventDefault(); }
  else if (e.key === 'Escape') ckClose();
  e.stopPropagation();
});
ckList.addEventListener('click', e => { const it = e.target.closest('.ck-item'); if (it) ckRun(+it.dataset.i); });
pal.addEventListener('pointerdown', e => { if (e.target === pal) ckClose(); });
window.addEventListener('keydown', e => {
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'k') {
    e.preventDefault();
    if (pal.hidden) ckOpen(); else ckClose();
  }
}, true);


const HELP = `<div class="help">
<b>Add a block:</b> right-click the canvas (or press + Block), type to search, Enter to place.<br>
<b>Connect:</b> drag from a colored port — green IF, red ELSE, blue AFTER. Press 1 / 2 / 3 while dragging to switch type. Drop on empty space to create a connected block. Teal port = bind a Target block.<br>
<b>Wires:</b> click a wire to change its type, delete it, or drag its white dot to reroute.<br>
<b>Select:</b> click, Ctrl/Shift-click, or Shift/Ctrl-drag a box. <b>Pan:</b> drag empty space, middle-drag, or Space-drag. <b>Zoom:</b> scroll wheel.<br>
<b>Keys:</b> Ctrl+C / Ctrl+V (keeps internal wires), Ctrl+D duplicate, Ctrl+A select all, B disable, Del delete, Esc deselect. Blocks cannot overlap while dragging; hold Alt to move through other blocks.<br>
<b>Logic:</b> a satisfied block follows IF + AFTER wires; an unsatisfied one follows ELSE + AFTER. A failed block follows its "On failure" setting. Several wires from one port run in parallel.
</div>`;

function logRow(e) {
  return `<div class="lr ${e.level}"><span class="t">${fmtTime(e.ts)}</span><span class="lv">${e.level}</span><span class="an">${esc(e.autoName)}</span><span class="bl">${esc(e.nodeLabel || '')}</span><span class="ms">${esc(e.msg)}</span></div>`;
}

function editorStatus() {
  const id = S.editId; if (!id) return;
  const st = stOf(id), run = isRun(id), p = $('#ed-status');
  p.className = 'pill ' + st; p.textContent = LABEL[st] || st;
  $('[data-ed=start]').disabled = run;
  $('[data-ed=stop]').disabled = !run;
  if (!run || (st === 'running' && S.edSt !== 'running')) $('#ed-note').textContent = '';
  S.edSt = st;
}

function renderDock() {
  if (S.view !== 'editor') return;
  document.querySelectorAll('#dock-tabs button').forEach(b => b.classList.toggle('on', b.dataset.tab === S.tab));
  const dock = $('#ed-dock'), closed = dock.classList.contains('closed');
  $('#dock-toggle').textContent = closed ? '▴' : '▾';
  if (closed) return;
  const el = $('#dock-body');
  if (S.tab === 'log') {
    const rows = [];
    for (let i = S.logs.length - 1; i >= 0 && rows.length < 150; i--) if (S.logs[i].autoId === S.editId) rows.push(logRow(S.logs[i]));
    el.innerHTML = rows.join('') || '<div class="empty">No events yet. Start the automation to see live logs.</div>';
  } else if (S.tab === 'history') {
    el.innerHTML = S.hist.length
      ? S.hist.map(r => `<div class="lr ${r.outcome === 'failed' ? 'error' : r.outcome === 'stopped' ? 'warn' : 'info'}"><span class="t">${fmtTime(r.startedAt)}</span><span class="lv">${esc(r.outcome)}</span><span class="bl">run ${r.id}</span><span class="ms">${fmtDur(r.endedAt - r.startedAt)}${r.errors ? ` · ${r.errors} errors` : ''}</span></div>`).join('')
      : '<div class="empty">No finished runs yet.</div>';
  } else el.innerHTML = HELP;
}

async function refreshHist() {
  if (!S.editId) return;
  try { S.hist = await invoke('get_history', { id: S.editId }); } catch { S.hist = []; }
  if (S.tab === 'history') renderDock();
}

function openEditor(id) {
  const a = S.autos.find(x => x.id === id); if (!a) return;
  S.editId = id; S.edSt = '';
  show('editor');
  editor.load(a);
  $('#ed-name').value = a.name;
  $('#ed-auto').checked = !!a.autoStart;
  for (const [nid, st] of Object.entries(S.rt.nodes[id] || {})) editor.setNodeState(nid, st);
  editorStatus(); refreshHist(); renderDock();
}
function closeEditor() { flushSaves(); S.editId = null; show('automations'); }
const curAuto = () => S.autos.find(a => a.id === S.editId);

$('#ed-back').addEventListener('click', closeEditor);
$('#ed-name').addEventListener('input', e => { const a = curAuto(); if (a) { a.name = e.target.value || 'Untitled automation'; markDirty(a); } });
$('#ed-auto').addEventListener('change', e => { const a = curAuto(); if (a) { a.autoStart = e.target.checked; markDirty(a); } });
$('#ed-add').addEventListener('click', () => editor.openMenuCenter());
$('#ed-fit').addEventListener('click', () => editor.fit());
document.querySelectorAll('[data-ed]').forEach(b => b.addEventListener('click', () => control(b.dataset.ed, S.editId)));
$('#dock-toggle').addEventListener('click', () => { $('#ed-dock').classList.toggle('closed'); renderDock(); });
$('#dock-tabs').addEventListener('click', e => {
  const b = e.target.closest('button'); if (!b) return;
  S.tab = b.dataset.tab; $('#ed-dock').classList.remove('closed');
  if (S.tab === 'history') refreshHist(); else renderDock();
});

// ---------- controls ----------
const ctl = id => {
  const run = isRun(id);
  return `<button class="ib" data-act="start" data-id="${esc(id)}" ${run ? 'disabled' : ''} title="Start">▶</button><button class="ib" data-act="stop" data-id="${esc(id)}" ${run ? '' : 'disabled'} title="Stop">■</button><button class="ib" data-act="restart" data-id="${esc(id)}" title="Restart">↻</button>`;
};

async function control(act, id) {
  if (!id) return;
  try {
    await flushSaves();
    if (ind.classList.contains('err') || pending.size > 0) {
      throw new Error('Your latest changes could not be saved. Fix the save error (click the red indicator to retry) before running.');
    }
    await invoke(act + '_automation', { id });
  } catch (e) { toast(String(e?.message || e).split('\n').join(' • '), 'err'); }
}


function newAuto() {
  const a = { id: uid(), name: 'Untitled automation', nodes: [], edges: [], viewport: { x: 80, y: 80, k: 1 }, autoStart: false };
  S.autos.push(a); markDirty(a); openEditor(a.id);
}
async function delAuto(id) {
  const a = S.autos.find(x => x.id === id); if (!a) return;
  const c = await modal({ title: 'Delete automation?', body: `"${a.name}" will be stopped and permanently deleted.`, buttons: [['cancel', 'Cancel'], ['del', 'Delete', 'danger']] });
  if (c !== 'del') return;
  try {
    // Finish any in-flight save before deleting so it cannot recreate this automation afterward.
    await flushSaves();
    pending.delete(id);
    await invoke('delete_automation', { id });
    S.autos = S.autos.filter(x => x.id !== id);
    delete S.rt.autos[id]; delete S.rt.nodes[id];
    if (!pending.size && ind.classList.contains('err')) setInd('Saved', 'ok');
    render();
  } catch (e) {
    if (S.autos.some(x => x.id === id)) markDirty(a);
    toast('Delete failed: ' + e, 'err');
  }
}

async function dupAuto(id) {
  const a = S.autos.find(x => x.id === id); if (!a) return;
  const c = JSON.parse(JSON.stringify(a));
  c.id = uid(); c.name = a.name + ' copy'; c.autoStart = false;
  S.autos.push(c); markDirty(c);
  try { await flushSaves(); render(); toast('Duplicated'); }
  catch (e) { toast('Duplicate could not be saved: ' + e, 'err'); }
}
async function exportAuto(id) {
  try { await flushSaves(); if (await invoke('export_automation', { id })) toast('Exported'); }
  catch (e) { toast('Export failed: ' + e, 'err'); }
}
async function importAuto() {
  try {
    const a = await invoke('import_automation');
    if (a) { S.autos.push(a); render(); toast(`Imported "${a.name}"`); }
  } catch (e) { toast('Import failed: ' + e, 'err'); }
}

document.addEventListener('click', e => {
  const b = e.target.closest('[data-act]'); if (!b) return;
  const { act, id } = b.dataset;
  if (act === 'open') openEditor(id);
  else if (act === 'new') newAuto();
  else if (act === 'delete') delAuto(id);
  else if (act === 'dup') dupAuto(id);
  else if (act === 'export') exportAuto(id);
  else if (act === 'import') importAuto();
  else if (act === 'export-logs') exportLogs();
  else if (['start', 'stop', 'restart'].includes(act)) control(act, id);
});


// ---------- views ----------
function show(view) {
  S.view = view;
  for (const v of ['overview', 'automations', 'logs', 'editor']) $('#view-' + v).hidden = v !== view;
  document.querySelectorAll('#tabs button').forEach(b => b.classList.toggle('on', b.dataset.view === (view === 'editor' ? 'automations' : view)));
  editor.setActive(view === 'editor');
  render();
}
function render() {
  if (S.view === 'overview') buildOverview();
  else if (S.view === 'automations') renderAutomations();
  else if (S.view === 'logs') renderLogs();
  else renderDock();
}
document.querySelectorAll('#tabs button').forEach(b => b.addEventListener('click', () => { if (S.view === 'editor') flushSaves(); show(b.dataset.view); }));

// Overview
function buildOverview() {
  $('#view-overview').innerHTML = `<div class="ov">
    <div class="glass card"><div class="chead"><h3>Automations</h3><button class="btn" data-act="new">+ New</button></div>
      <table class="tbl"><thead><tr><th>Name</th><th>Status</th><th>Runtime</th><th>Last run</th><th></th></tr></thead><tbody id="ov-rows"></tbody></table></div>
    <div class="side">
      <div class="glass card"><h3>Application</h3>
        <div class="res"><div><b id="r-cpu">–</b><span>CPU</span><i><u id="b-cpu"></u></i></div><div><b id="r-ram">–</b><span>RAM</span><i><u id="b-ram"></u></i></div></div>
        <small class="mu">Includes WebView2 helper processes. Sampled every 2 s only while this tab is visible.</small></div>
      <div class="glass card"><h3>Recent activity</h3><div id="ov-recent" class="recent"></div></div>
    </div></div>`;
  fillTable(); fillRecent(); tickStats();
}
function fillTable() {
  const body = $('#ov-rows'); if (!body) return;
  if (!S.autos.length) { body.innerHTML = '<tr><td colspan="5" class="empty">No automations yet.</td></tr>'; return; }
  body.innerHTML = S.autos.map(a => {
    const r = S.rt.autos[a.id] || {}, st = stOf(a.id);
    const extra = r.lastOutcome && r.lastOutcome.includes('errors') && !isRun(a.id) ? ` <small class="mu">${esc(r.lastOutcome)}</small>` : '';
    return `<tr data-act="open" data-id="${esc(a.id)}"><td>${esc(a.name)}</td><td><span class="pill ${st}">${LABEL[st] || st}</span>${extra}</td><td class="rt" data-s="${r.startedAt || ''}">${r.startedAt ? fmtDur(Date.now() - r.startedAt) : '—'}</td><td>${r.lastRun ? fmtAgo(r.lastRun) : '—'}</td><td class="ctl">${ctl(a.id)}</td></tr>`;
  }).join('');
}
function fillRecent() {
  const el = $('#ov-recent'); if (!el) return;
  const rows = [];
  for (let i = S.logs.length - 1; i >= 0 && rows.length < 8; i--) {
    const e = S.logs[i];
    const cls = e.level === 'error' ? 'er' : e.level === 'warn' ? 'wa' : '';
    rows.push(`<div class="${cls}"><span>${fmtTime(e.ts).slice(0, 8)}</span><span>${esc(e.autoName)}${e.nodeLabel ? ' · ' + esc(e.nodeLabel) : ''}: ${esc(e.msg)}</span></div>`);
  }
  el.innerHTML = rows.join('') || '<div><span></span><span>Nothing yet.</span></div>';
}
function fillRes(s) {
  const c = $('#r-cpu'); if (!c) return;
  c.textContent = s.cpu.toFixed(1) + '%';
  $('#r-ram').textContent = s.ramMb.toFixed(0) + ' MB';
  $('#b-cpu').style.width = Math.min(100, s.cpu * 10) + '%';
  $('#b-ram').style.width = Math.min(100, s.ramMb / 5) + '%';
}
async function tickStats() {
  if (S.view !== 'overview' || !active()) return;
  try { fillRes(await invoke('app_stats')); } catch { /* ignore */ }
}
let tick = 0;
setInterval(() => {
  if (!active() || S.view !== 'overview') return;
  tick++;
  if (tick % 10 === 0) { fillTable(); return; }
  document.querySelectorAll('#ov-rows .rt').forEach(td => { const s = +td.dataset.s; if (s) td.textContent = fmtDur(Date.now() - s); });
}, 1000);
setInterval(tickStats, 2000);

// Automations
function renderAutomations() {
  const root = $('#view-automations');
  if (!root.dataset.built) {
    root.innerHTML = `<div class="bar"><input id="au-q" class="search" placeholder="Search automations…" spellcheck="false"><button class="btn primary" data-act="new">+ New automation</button><button class="btn" data-act="import">Import…</button></div><div id="au-list" class="cards"></div>`;
    root.dataset.built = '1';
    $('#au-q').addEventListener('input', e => { S.q = e.target.value.toLowerCase(); fillAutos(); });
  }
  fillAutos();
}
function fillAutos() {
  const el = $('#au-list'); if (!el) return;
  const list = S.autos.filter(a => a.name.toLowerCase().includes(S.q));
  el.innerHTML = list.map(a => {
    const st = stOf(a.id), id = esc(a.id);
    return `<div class="acard glass"><h4><span>${esc(a.name)}</span><span class="pill ${st}">${LABEL[st] || st}</span></h4>
      <div class="mu">${a.nodes.length} blocks · ${a.edges.length} wires${a.autoStart ? ' · runs at launch' : ''}</div>
      <div class="row"><span>${ctl(a.id)}</span><span><button class="btn" data-act="open" data-id="${id}">Open</button><button class="btn" data-act="dup" data-id="${id}" title="Duplicate">⧉</button><button class="btn" data-act="export" data-id="${id}" title="Export to a file">⤓</button><button class="btn danger" data-act="delete" data-id="${id}">Delete</button></span></div></div>`;
  }).join('') || '<div class="empty">No automations match.</div>';
}


// Logs
function renderLogs() {
  const root = $('#view-logs');
  if (!root.dataset.built) {
    root.innerHTML = `<div class="logbar"><input id="lg-q" class="search" placeholder="Search logs…" spellcheck="false">
      <select id="lg-lv"><option value="">All levels</option><option value="info">Info</option><option value="warn">Warnings</option><option value="error">Errors</option></select>
      <select id="lg-au"></select><button class="btn" id="lg-clear">Clear</button><button class="btn" data-act="export-logs" title="Export the filtered entries to CSV">Export CSV</button><span class="mu" id="lg-count"></span></div>
      <div class="glass logs" id="lg-rows"></div>`;
    root.dataset.built = '1';
    $('#lg-q').addEventListener('input', e => { S.lf.q = e.target.value.toLowerCase(); fillLogs(); });
    $('#lg-lv').addEventListener('change', e => { S.lf.level = e.target.value; fillLogs(); });
    $('#lg-au').addEventListener('change', e => { S.lf.auto = e.target.value; fillLogs(); });
    $('#lg-clear').addEventListener('click', async () => { try { await invoke('clear_logs'); S.logs = []; fillLogs(); } catch (e) { toast(String(e), 'err'); } });
  }
  $('#lg-au').innerHTML = `<option value="">All automations</option><option value="__app">Application</option>` +
    S.autos.map(a => `<option value="${esc(a.id)}">${esc(a.name)}</option>`).join('');
  $('#lg-au').value = S.lf.auto;
  fillLogs();
}
function fillLogs() {
  const el = $('#lg-rows'); if (!el) return;
  const rows = []; let total = 0;
  for (let i = S.logs.length - 1; i >= 0; i--) {
    const e = S.logs[i];
    if (!logMatch(e)) continue;
    total++;
    if (rows.length < 500) rows.push(logRow(e));
  }
  el.innerHTML = rows.join('') || '<div class="empty">No log entries match.</div>';
  $('#lg-count').textContent = total > 500 ? `showing newest 500 of ${total}` : `${total} entries`;
}

// ---------- live events ----------
let sched = 0; const need = new Set();
function schedule(k) {
  need.add(k);
  if (sched) return;
  sched = setTimeout(() => {
    sched = 0;
    const n = new Set(need); need.clear();
    if (!active()) return;
    if (S.view === 'overview') { if (n.has('autos')) fillTable(); if (n.has('logs')) fillRecent(); }
    else if (S.view === 'automations') { if (n.has('autos')) fillAutos(); }
    else if (S.view === 'logs') { if (n.has('logs')) fillLogs(); }
    else if (S.view === 'editor') { if (n.has('autos')) editorStatus(); if (n.has('logs')) renderDock(); }
  }, 150);
}

async function resync() {
  S.rt = await invoke('get_runtime');
  S.logs = await invoke('get_logs', { since: 0, autoId: null });
}

async function bindEvents() {
  await listen('log', e => {
    S.logs.push(e.payload);
    if (S.logs.length > 5000) S.logs.splice(0, S.logs.length - 5000);
    schedule('logs');
  });
  await listen('node-state', e => {
    const { autoId, nodeId, state } = e.payload;
    (S.rt.nodes[autoId] ||= {})[nodeId] = state;
    if (S.editId === autoId) editor.setNodeState(nodeId, state);
  });
  await listen('auto-state', e => {
    const { id, runtime } = e.payload;
    S.rt.autos[id] = runtime;
    if (runtime.status === 'running') {
      S.rt.nodes[id] = {};
      if (S.editId === id) editor.clearStates();
    } else if (runtime.status !== 'stopping') {
      for (const st of Object.values(S.rt.nodes[id] || {})) if (st.state === 'running' || st.state === 'waiting') { st.state = 'idle'; st.progress = null; }
      if (S.editId === id) { editor.settle(); refreshHist(); }
    }
    schedule('autos'); schedule('logs');
  });
  await listen('ui-shown', async () => {
    S.vis = true;
    try { await resync(); } catch { /* ignore */ }
    render();
    if (S.editId) for (const [nid, st] of Object.entries(S.rt.nodes[S.editId] || {})) editor.setNodeState(nid, st);
  });
  await listen('ui-hidden', () => { S.vis = false; });
  await listen('close-requested', async () => {
    if (!$('#modal').hidden) return;
    const c = await modal({
      title: 'Close 00047?',
      body: 'Minimize to the system tray to keep your automations running, or exit and stop them all.',
      buttons: [['tray', 'Minimize to tray', 'primary'], ['exit', 'Exit'], ['cancel', 'Cancel']],
    });
    if (c === 'cancel') return;
    await flushSaves();
    if (ind.classList.contains('err')) {
      const k = await modal({ title: 'Unsaved changes', body: 'Some changes could not be saved. Continue anyway?', buttons: [['no', 'Go back'], ['yes', 'Continue', 'danger']] });
      if (k !== 'yes') return;
    }
    try { await invoke(c === 'tray' ? 'hide_to_tray' : 'exit_app'); } catch (e) { toast(String(e), 'err'); }
  });
  document.addEventListener('visibilitychange', async () => {
    if (!document.hidden && S.vis) { try { await resync(); } catch { /* ignore */ } render(); }
  });
}

// ---------- settings ----------
$('#btn-settings').addEventListener('click', async e => {
  e.stopPropagation();
  const p = $('#pop-settings');
  p.hidden = !p.hidden;
  if (!p.hidden) { try { $('#set-autostart').checked = await invoke('get_autostart'); } catch { /* ignore */ } }
});
document.addEventListener('click', e => { if (!e.target.closest('#pop-settings,#btn-settings')) $('#pop-settings').hidden = true; });
$('#set-autostart').addEventListener('change', async e => {
  const want = e.target.checked;
  try { await invoke('set_autostart', { enabled: want }); }
  catch (err) { e.target.checked = !want; toast('Could not change launch-at-sign-in: ' + err, 'err'); }
});

// ---------- custom blocks ----------
function htmlModal(title, html, buttons) {
  return new Promise(res => {
    const m = $('#modal');
    m.innerHTML = `<div class="mbox glass wide"><h3>${esc(title)}</h3><div class="mbody">${html}</div><div class="mbtn">${buttons.map(([v, l, c]) => `<button class="btn ${c || ''}" data-v="${v}">${esc(l)}</button>`).join('')}</div></div>`;
    m.hidden = false;
    m.onclick = e => {
      const b = e.target.closest('button');
      if (b || e.target === m) { m.hidden = true; m.innerHTML = ''; res(b ? b.dataset.v : 'cancel'); }
    };
  });
}

async function loadBlockSpecs() {
  try { registerSpecs((await invoke('list_block_specs')).specs); }
  catch (e) { toast('Could not load extra blocks: ' + e, 'err'); }
}

async function openBlocksManager() {
  let info;
  try { info = await invoke('list_block_specs'); } catch (e) { toast('Could not read blocks: ' + e, 'err'); return; }
  const custom = info.specs.filter(s => s.origin === 'custom');
  const list = custom.length
    ? `<ul class="bp">${custom.map(s => `<li><b>${esc(s.title)}</b> <small>${esc(s.category)} · ${esc(s.kind)}</small></li>`).join('')}</ul>`
    : '<p>No custom blocks installed yet.</p>';
  const errs = info.errors.length ? `<p class="er">${info.errors.map(esc).join('<br>')}</p>` : '';
  const c = await htmlModal('Custom blocks',
    `<p>Folder: <code>${esc(info.dir)}</code></p>${list}${errs}<p>ZIP plugin packs contain a manifest plus scripts. They can run any installed interpreter or executable with your Windows account permissions. Only install code you trust. See docs/ADDING_BLOCKS.md.</p>`,
    [['close', 'Close'], ['folder', 'Open folder'], ['install', 'Install ZIP plugin…', 'primary'], ['reload', 'Reload (restarts 00047)']]);
  if (c === 'folder') invoke('open_blocks_folder').catch(e => toast(String(e), 'err'));
  else if (c === 'install') installPack();
  else if (c === 'reload') reloadBlocks();
}

async function installPack() {
  try {
    const pv = await invoke('block_pack_preview');
    if (!pv) return;
    const rows = pv.blocks.map(b => `<li><b>${esc(b.title)}</b> <small>${esc(b.category)} · ${esc(b.kind)}</small><pre>${esc(b.command)}</pre></li>`).join('');
    const errs = pv.errors.length ? `<p class="er">Skipped: ${pv.errors.map(esc).join('<br>')}</p>` : '';
    const c = await htmlModal(`Install "${pv.file}"?`,
      `<p>This ZIP will install these blocks. The listed runtime commands can execute code with your Windows account permissions:</p><ul class="bp">${rows}</ul>${errs}`,
      [['cancel', 'Cancel'], ['go', 'Install', 'primary']]);
    if (c !== 'go') return;
    await invoke('block_pack_install', { path: pv.path });
    toast('Plugin installed. Choose "Reload" in Custom blocks to load it.');
  } catch (e) { toast('Install failed: ' + e, 'err'); }
}

async function reloadBlocks() {
  const c = await modal({ title: 'Restart 00047?', body: 'Running automations are stopped and the app restarts to load changed blocks.', buttons: [['cancel', 'Cancel'], ['go', 'Restart', 'primary']] });
  if (c !== 'go') return;
  await flushSaves();
  try { await invoke('restart_app'); } catch (e) { toast('Restart failed: ' + e, 'err'); }
}
$('#btn-blocks').addEventListener('click', () => { $('#pop-settings').hidden = true; openBlocksManager(); });

function startGlassBackdrop() {
  if (!appWindow || !/Win/i.test(navigator.userAgent)) return;

  const reflectionRoot = document.documentElement;
  reflectionRoot.style.setProperty('--liquidglass-reflection-image', 'none');

  const refreshSelect = $('#set-glass-refresh');
  const refreshStatus = $('#glass-refresh-status');
  const allowedModes = new Set(['dynamic', '60', '120', 'off']);
  let mode = 'dynamic';
  try {
    const saved = localStorage.getItem('00047.glassRefreshMode');
    // Older builds only offered 5/10/20 FPS. "Off" remains Off; old numeric
    // values migrate to Dynamic so users don't stay capped at a slow rate.
    mode = saved === 'off' ? 'off' : (allowedModes.has(saved) ? saved : 'dynamic');
  } catch { /* Use Dynamic when local storage is unavailable. */ }
  if (refreshSelect) refreshSelect.value = mode;

  let inFlight = false;
  let loggedFailure = false;
  let geometryDirty = true;
  let lastGeometrySignature = '';
  let frameRequest = 0;
  let lastScheduledAt = -Infinity;
  let metricStart = 0;
  let completedFrames = 0;
  const reflectedSurfaceSelector = '.glass.liquidglass-variant:not(input[type="checkbox"]):not(input[type="radio"])';

  const markGeometryDirty = () => { geometryDirty = true; };
  const withoutReflectionOffsets = style => String(style || '')
    .replace(/--liquidglass-reflection-size\s*:[^;]*;?/g, '')
    .replace(/--liquidglass-reflection-position\s*:[^;]*;?/g, '')
    .trim();
  const geometryObserver = new MutationObserver(records => {
    const geometryChanged = records.some(record => {
      if (
        record.type === 'attributes' &&
        record.attributeName === 'style' &&
        record.target.matches?.(reflectedSurfaceSelector)
      ) {
        return withoutReflectionOffsets(record.oldValue) !==
          withoutReflectionOffsets(record.target.getAttribute('style'));
      }
      return true;
    });
    if (geometryChanged) markGeometryDirty();
  });
  geometryObserver.observe(document.body, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeOldValue: true,
    attributeFilter: ['class', 'hidden', 'style'],
  });

  const updateSurfaceGeometry = frame => {
    const scale = window.devicePixelRatio || 1;
    const sampleWidth = frame.screenWidth / scale;
    const sampleHeight = frame.screenHeight / scale;
    const originX = (frame.screenX - frame.windowX) / scale;
    const originY = (frame.screenY - frame.windowY) / scale;

    for (const surface of document.querySelectorAll(reflectedSurfaceSelector)) {
      const rect = surface.getBoundingClientRect();
      if (
        rect.width <= 0 || rect.height <= 0 ||
        rect.right <= 0 || rect.bottom <= 0 ||
        rect.left >= window.innerWidth || rect.top >= window.innerHeight
      ) continue;

      surface.style.setProperty('--liquidglass-reflection-size', sampleWidth + 'px ' + sampleHeight + 'px');
      surface.style.setProperty('--liquidglass-reflection-position', (originX - rect.left) + 'px ' + (originY - rect.top) + 'px');
    }
    geometryDirty = false;
  };

  const showRefreshStatus = (now, gotFrame) => {
    if (!refreshStatus) return;
    if (mode === 'off') {
      refreshStatus.textContent = 'Paused · holding the last captured backdrop.';
      return;
    }
    if (gotFrame) completedFrames++;
    if (!metricStart) metricStart = now;
    const elapsed = now - metricStart;
    if (elapsed >= 1000) {
      const measured = Math.round(completedFrames * 1000 / elapsed);
      const target = mode === 'dynamic' ? 'Dynamic target' : mode + ' FPS target';
      refreshStatus.textContent = 'Measured ' + measured + ' FPS · ' + target;
      metricStart = now;
      completedFrames = 0;
    } else if (!refreshStatus.textContent || refreshStatus.textContent.startsWith('Paused')) {
      refreshStatus.textContent = 'Measuring capture throughput…';
    }
  };

  const capture = async () => {
    if (inFlight || document.hidden || mode === 'off') return;
    inFlight = true;
    try {
      const frame = await invoke('capture_glass_frame');
      if (!frame?.dataUrl || !frame.screenWidth || !frame.screenHeight) {
        throw new Error('Desktop capture returned an empty frame.');
      }

      const scale = window.devicePixelRatio || 1;
      const signature = frame.screenWidth + 'x' + frame.screenHeight + '@' + scale;
      if (geometryDirty || signature !== lastGeometrySignature) {
        updateSurfaceGeometry(frame);
        lastGeometrySignature = signature;
      }

      reflectionRoot.style.setProperty('--liquidglass-reflection-image', 'url("' + frame.dataUrl + '")');
      loggedFailure = false;
      showRefreshStatus(performance.now(), true);
    } catch (error) {
      if (!loggedFailure) {
        console.warn('LiquidGlass desktop capture unavailable:', error);
        loggedFailure = true;
      }
      showRefreshStatus(performance.now(), false);
    } finally {
      inFlight = false;
    }
  };

  const stopFrames = () => {
    if (frameRequest) window.cancelAnimationFrame(frameRequest);
    frameRequest = 0;
  };
  const tick = timestamp => {
    frameRequest = 0;
    if (document.hidden || mode === 'off') return;

    const targetInterval = mode === 'dynamic' ? 0 : 1000 / Number(mode);
    if (!inFlight && timestamp - lastScheduledAt >= targetInterval) {
      lastScheduledAt = timestamp;
      void capture();
    }
    frameRequest = window.requestAnimationFrame(tick);
  };
  const startFrames = () => {
    if (!document.hidden && mode !== 'off' && !frameRequest) {
      frameRequest = window.requestAnimationFrame(tick);
    }
  };

  const applyRefreshMode = () => {
    mode = allowedModes.has(refreshSelect?.value) ? refreshSelect.value : 'dynamic';
    try { localStorage.setItem('00047.glassRefreshMode', mode); } catch { /* Preference is optional. */ }
    metricStart = 0;
    completedFrames = 0;
    if (mode === 'off') {
      stopFrames();
      showRefreshStatus(performance.now(), false);
      return;
    }
    if (refreshStatus) refreshStatus.textContent = 'Measuring capture throughput…';
    lastScheduledAt = -Infinity;
    void capture();
    startFrames();
  };

  refreshSelect?.addEventListener('change', applyRefreshMode);
  window.addEventListener('resize', markGeometryDirty, { passive: true });
  window.addEventListener('scroll', markGeometryDirty, { passive: true, capture: true });
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      stopFrames();
    } else {
      markGeometryDirty();
      if (mode !== 'off') {
        void capture();
        startFrames();
      }
    }
  });

  if (mode === 'off') {
    mode = 'dynamic';
    void capture().finally(() => {
      mode = 'off';
      showRefreshStatus(performance.now(), false);
    });
  } else {
    if (refreshStatus) refreshStatus.textContent = 'Measuring capture throughput…';
    void capture();
    startFrames();
  }
}

function setupGlassMaterials() {
  const selector = '.glass, button, input:not([type="checkbox"]):not([type="radio"]), select, textarea, .pill, .eu-handle, .eu-bar';
  const applyMaterial = el => {
    // Keep workflow blocks and their embedded fields on the editor's original
    // canvas material. Floating canvas overlays remain LiquidGlass surfaces.
    if (el.closest('.canvas') && !el.closest('.ctx, .eu-bar, .eu-handle')) return;
    el.classList.add('glass', 'liquidglass-variant');
    el.classList.remove('liquidglass');
  };
  const apply = root => {
    if (root.nodeType !== 1) return;
    if (root.matches(selector)) applyMaterial(root);
    root.querySelectorAll?.(selector).forEach(applyMaterial);
  };
  apply(document.documentElement);
  new MutationObserver(records => {
    for (const record of records) {
      for (const node of record.addedNodes) apply(node);
    }
  }).observe(document.body, { childList: true, subtree: true });
}
// ---------- init ----------
(async function init() {
  await loadBlockSpecs();
  try {
    S.autos = await invoke('list_automations');
    await resync();
  } catch (e) { toast('Could not load data: ' + e, 'err'); }
  await bindEvents();
  show('overview');
  setupGlassMaterials();
  startGlassBackdrop();
})();
