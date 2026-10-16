const ic = p => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${p}</svg>`;
const I = {
  clock: ic('<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>'),
  chip: ic('<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>'),
  trim: ic('<path d="M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7"/>'),
  hourglass: ic('<path d="M6 2h12M6 22h12M7 2c0 6 5 6 5 10s-5 4-5 10M17 2c0 6-5 6-5 10s5 4 5 10"/>'),
  play: ic('<path d="M7 4l13 8-13 8z"/>'),
  repeat: ic('<path d="M17 2l4 4-4 4M3 11V9a3 3 0 0 1 3-3h15M7 22l-4-4 4-4M21 13v2a3 3 0 0 1-3 3H3"/>'),
  window: ic('<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/>'),
  file: ic('<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>'),
  note: ic('<path d="M4 5h16M4 12h16M4 19h10"/>'),
};

export const CATS = {
  cond: { label: 'Condition', color: '#f5b14a' },
  act: { label: 'Action', color: '#8b9cff' },
  tgt: { label: 'Target', color: '#3ddbd9' },
};

const num = v => (typeof v === 'number' && isFinite(v) ? +v.toFixed(2) : '?');
const dur = s => {
  if (typeof s !== 'number' || !isFinite(s)) return '?';
  if (s >= 3600) return `${+(s / 3600).toFixed(2)}h`;
  if (s >= 60) return `${+(s / 60).toFixed(2)}m`;
  return `${+s.toFixed(2)}s`;
};

const TIMEOUT = { key: 'timeoutSec', label: 'Timeout', type: 'number', min: 0, max: 86400, step: 1, unit: 's', help: '0 = no timeout. When it expires the block is marked failed.' };
const ONFAIL = {
  key: 'onFail', label: 'On failure', type: 'select',
  options: [['stop', 'End this branch'], ['else', 'Follow ELSE wires'], ['after', 'Follow AFTER wires'], ['abort', 'Abort automation']],
};

export const BLOCKS = {
  'cond.interval': {
    cat: 'cond', title: 'Interval', icon: I.clock,
    desc: 'Waits, then continues. As a first block it repeats.',
    defaults: { seconds: 30, repeat: true },
    fields: [
      { key: 'seconds', label: 'Every', type: 'number', min: 0.1, max: 86400, step: 1, unit: 's' },
      { key: 'repeat', label: 'Repeat when this is a first block', type: 'bool' },
    ],
    summary: c => `Every ${dur(c.seconds)}${c.repeat ? ' · repeats' : ''}`,
    help: 'As a first block it fires, runs everything wired after it, then waits again (runs never overlap). Elsewhere it is just a delay.',
  },
  'cond.ram': {
    cat: 'cond', title: 'RAM Usage', icon: I.chip,
    desc: 'Compares RAM (MB) of a wired Active process, or of the whole system, to a threshold.',
    defaults: { op: 'above', thresholdMb: 800, mode: 'check', pollSec: 1, timeoutSec: 0, onFail: 'stop' },
    fields: [
      { key: 'op', label: 'Satisfied when RAM is', type: 'select', options: [['above', 'Above'], ['below', 'Below']] },
      { key: 'thresholdMb', label: 'Threshold', type: 'number', min: 1, max: 1048576, step: 10, unit: 'MB' },
      { key: 'mode', label: 'Mode', type: 'select', options: [['check', 'Check once (IF / ELSE)'], ['wait', 'Wait until satisfied']] },
      { key: 'pollSec', label: 'Poll every', type: 'number', min: 0.25, max: 3600, step: 0.25, unit: 's', show: c => c.mode === 'wait' },
      { ...TIMEOUT, show: c => c.mode === 'wait' },
      ONFAIL,
    ],
    summary: c => `${c.op === 'below' ? 'Below' : 'Above'} ${num(c.thresholdMb)} MB · ${c.mode === 'wait' ? 'waits' + (c.timeoutSec > 0 ? ` ≤ ${dur(c.timeoutSec)}` : '') : 'checks once'}`,
    help: 'Wire an Active process target in to measure that process (working set). With no target it measures used system RAM.',
  },
  'act.trim': {
    cat: 'act', title: 'Working-Set Trim', icon: I.trim,
    desc: 'Releases part of a process\'s working set (needs an Active process target).',
    requires: 'tgt.process',
    defaults: { percent: 50, timeoutSec: 15, onFail: 'stop' },
    fields: [
      { key: 'percent', label: 'Release', type: 'number', min: 0, max: 100, step: 5, unit: '%' },
      TIMEOUT,
      ONFAIL,
    ],
    summary: c => `Release ${num(c.percent)}% of working set`,
    help: '0% does nothing. 100% fully trims the working set. 1–99% temporarily lowers the soft maximum to the remaining share, then restores the original limits. Pages move to standby memory and the process may fault them back in. This is not a leak fix. The log shows the real before/after size. Protected or elevated processes need 00047 run as administrator.',
  },
  'act.wait': {
    cat: 'act', title: 'Wait', icon: I.hourglass,
    desc: 'Pauses this path for a set time.',
    defaults: { seconds: 5 },
    fields: [{ key: 'seconds', label: 'Duration', type: 'number', min: 0, max: 86400, step: 1, unit: 's' }],
    summary: c => `Wait ${dur(c.seconds)}`,
  },
  'act.start': {
    cat: 'act', title: 'Start', icon: I.play,
    desc: 'Launches the wired File target (program, document or shortcut).',
    requires: 'tgt.file',
    defaults: { args: '', timeoutSec: 15, onFail: 'stop' },
    fields: [
      { key: 'args', label: 'Arguments (optional)', type: 'text', placeholder: '--example' },
      TIMEOUT,
      ONFAIL,
    ],
    summary: c => `Launch the File target${c.args ? ' with arguments' : ''}`,
  },
  'act.loop': {
    cat: 'act', title: 'Loop', icon: I.repeat,
    desc: 'IF = loop body (wire it back here), ELSE = exit after the last iteration.',
    defaults: { iterations: 3, delaySec: 1 },
    fields: [
      { key: 'iterations', label: 'Iterations (0 = until stopped)', type: 'number', min: 0, max: 1000000, step: 1 },
      { key: 'delaySec', label: 'Delay each iteration', type: 'number', min: 0.1, max: 86400, step: 0.5, unit: 's' },
    ],
    summary: c => `${c.iterations === 0 ? 'Unlimited' : num(c.iterations) + '×'} · ${dur(c.delaySec)} per iteration`,
    help: 'Wire IF to the first block of the body and the last body block (AFTER) back into this Loop. Wire ELSE to what runs once the loop is done. A runaway guard aborts the automation if a block fires too often.',
  },
  'act.log': {
    cat: 'act', title: 'Log message', icon: I.note,
    desc: 'Writes a message to the logs. Handy for tracing IF / ELSE paths.',
    defaults: { message: 'Hello from 00047', level: 'info' },
    fields: [
      { key: 'message', label: 'Message', type: 'text', required: true, maxLen: 500 },
      { key: 'level', label: 'Level', type: 'select', options: [['info', 'Info'], ['warn', 'Warning'], ['error', 'Error']] },
    ],
    summary: c => c.message || 'No message',
  },
  'tgt.process': {
    cat: 'tgt', title: 'Active process', icon: I.window,
    desc: 'The foreground window\'s process, or a process by name.',
    defaults: { mode: 'foreground', name: '', ignoreSelf: true },
    fields: [
      { key: 'mode', label: 'Pick', type: 'select', options: [['foreground', 'Foreground window'], ['name', 'By exe name']] },
      { key: 'name', label: 'Process name', type: 'text', placeholder: 'chrome.exe', required: true, show: c => c.mode === 'name' },
      { key: 'ignoreSelf', label: 'Fail if the foreground is 00047 itself', type: 'bool', show: c => c.mode !== 'name' },
    ],
    summary: c => (c.mode === 'name' ? (c.name || 'No name set') : 'Foreground window process'),
    help: 'Drag from the teal port to a Condition or Action to bind this target. It is resolved fresh each time the block runs.',
  },
  'tgt.file': {
    cat: 'tgt', title: 'File', icon: I.file,
    desc: 'A file or program path.',
    defaults: { path: '' },
    fields: [{ key: 'path', label: 'Full path', type: 'file', placeholder: 'C:\\Tools\\app.exe', required: true }],
    summary: c => c.path || 'No path set',
  },
};

export function fieldError(f, cfg) {
  if (f.show && !f.show(cfg)) return null;
  const v = cfg[f.key];
  if (f.type === 'number') {
    if (typeof v !== 'number' || !Number.isFinite(v)) return 'Enter a number';
    if (v < f.min || v > f.max) return `${f.min} – ${f.max}`;
  }
  if (f.type === 'text' || f.type === 'file') {
    const s = String(v ?? '').trim();
    if (f.required && !s) return 'Required';
    if (f.maxLen && s.length > f.maxLen) return `Max ${f.maxLen} characters`;
  }
  return null;
}


export function validate(kind, cfg) {
  const b = BLOCKS[kind];
  if (!b) return ['Unknown block type'];
  return b.fields.map(f => { const e = fieldError(f, cfg); return e ? `${f.label}: ${e}` : null; }).filter(Boolean);
}
