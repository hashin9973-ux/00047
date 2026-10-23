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
  proc: ic('<circle cx="12" cy="12" r="4"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M16.9 16.9l2.1 2.1M4.9 19.1l2.1-2.1M16.9 7.1l2.1-2.1"/>'),
  stop: ic('<rect x="5" y="5" width="14" height="14" rx="2"/><path d="M9 9h6v6H9z"/>'),
  bell: ic('<path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9M13.73 21a2 2 0 0 1-3.46 0"/>'),
  activity: ic('<path d="M3 12h4l3-8 4 16 3-8h4"/>'),
  'app-window': ic('<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M7 6.5h.01M10 6.5h.01"/>'),
  battery: ic('<rect x="3" y="7" width="17" height="10" rx="2"/><path d="M23 10v4M7 10v4"/>'),
  'battery-charging': ic('<rect x="3" y="7" width="17" height="10" rx="2"/><path d="M23 10v4M12 8l-3 5h4l-1 3 4-6h-4z"/>'),
  'calendar-clock': ic('<rect x="3" y="4" width="18" height="17" rx="2"/><path d="M8 2v4M16 2v4M3 9h10"/><circle cx="16.5" cy="15.5" r="3.5"/><path d="M16.5 13.5v2l1.5 1"/>'),
  clipboard: ic('<rect x="5" y="5" width="14" height="17" rx="2"/><rect x="9" y="2" width="6" height="5" rx="1"/><path d="M8 11h8M8 15h8"/>'),
  eraser: ic('<path d="m4 15 9-10a2 2 0 0 1 3 0l4 4a2 2 0 0 1 0 3l-8 8H8z"/><path d="m9 10 6 6M13 20h8"/>'),
  'external-link': ic('<path d="M14 4h6v6M20 4l-9 9"/><path d="M18 13v5a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h5"/>'),
  'file-archive': ic('<path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><path d="M14 3v6h6M10 10h2v2h-2zM10 14h2v2h-2zM10 18h2"/>'),
  'file-pen': ic('<path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h6M14 3v6h6M14 3l6 6"/><path d="m13 17 5-5 3 3-5 5-4 1z"/>'),
  'file-text': ic('<path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><path d="M14 3v6h6M8 13h8M8 17h8"/>'),
  files: ic('<path d="M8 7V4a2 2 0 0 1 2-2h9l3 3v11a2 2 0 0 1-2 2h-3"/><path d="M3 8a2 2 0 0 1 2-2h9l3 3v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'),
  'folder-open': ic('<path d="M3 6a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v2H8l-2 9-3-1z"/><path d="M6 19l2-9h13l-3 8a2 2 0 0 1-2 1z"/>'),
  'folder-plus': ic('<path d="M3 6a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M12 10v6M9 13h6"/>'),
  'folder-search': ic('<path d="M3 6a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v3"/><path d="M3 8v10a2 2 0 0 0 2 2h6"/><circle cx="16" cy="15" r="3"/><path d="m18.3 17.3 2.2 2.2"/>'),
  'folder-tree': ic('<path d="M3 5a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v3"/><path d="M3 7v11a2 2 0 0 0 2 2h5M14 12v3M14 15h4v4M14 15h-3v4"/><rect x="12" y="10" width="4" height="4" rx="1"/>'),
  gauge: ic('<path d="M4 19a9 9 0 1 1 16 0"/><path d="m12 13 5-5M12 13l-1 3"/><circle cx="12" cy="13" r="1"/>'),
  globe: ic('<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a15 15 0 0 1 0 18M12 3a15 15 0 0 0 0 18"/>'),
  'hard-drive': ic('<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 14h18M7 16.5h.01M11 16.5h.01"/>'),
  image: ic('<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="8.5" cy="9" r="1.5"/><path d="m21 15-5-5L5 20"/>'),
  keyboard: ic('<rect x="2" y="5" width="20" height="14" rx="2"/><path d="M6 9h.01M9 9h.01M12 9h.01M15 9h.01M18 9h.01M6 12h.01M9 12h.01M12 12h.01M15 12h.01M18 12h.01M8 15h8"/>'),
  layers: ic('<path d="m12 3 9 5-9 5-9-5z"/><path d="m3 12 9 5 9-5M3 16l9 5 9-5"/>'),
  lock: ic('<rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v3"/>'),
  'message-square': ic('<path d="M21 11.5a8.5 8.5 0 0 1-9 8.5 9 9 0 0 1-4-.9L3 21l1.9-4.5A8.5 8.5 0 1 1 21 11.5z"/>'),
  'file-json': ic('<path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><path d="M14 3v6h6M8 13h2v-2H8v4h2M13 11h2v2h-2v2h2"/>'),
  regex: ic('<path d="M4 5h5M6.5 2.5v5M13 5l3 3-3 3M16 8h4M4 17l3-3 3 3M7 14v6M13 16h7M13 20h5"/>'),
  'file-spreadsheet': ic('<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18M3 15h18M9 3v18M15 3v18"/>'),
  square: ic('<rect x="5" y="5" width="14" height="14" rx="1"/>'),
  'skip-forward': ic('<path d="M5 5l10 7-10 7zM19 5v14"/>'),

  'monitor-down': ic('<rect x="3" y="3" width="18" height="13" rx="2"/><path d="M8 21h8M12 16v5M8 8l4 4 4-4"/>'),
  'mouse-pointer-2': ic('<path d="m5 3 14 11-7 1-3 6z"/>'),
  network: ic('<circle cx="12" cy="5" r="2"/><circle cx="5" cy="18" r="2"/><circle cx="19" cy="18" r="2"/><path d="m11 7-5 9M13 7l5 9M7 18h10"/>'),
  plus: ic('<path d="M12 5v14M5 12h14"/>'),
  power: ic('<path d="M12 2v10M6 5a8 8 0 1 0 12 0"/>'),
  'refresh-cw': ic('<path d="M20 7v5h-5M4 17v-5h5"/><path d="M5.5 9A7 7 0 0 1 18 6l2 1M4 17l2 1a7 7 0 0 0 12.5-3"/>'),
  settings: ic('<circle cx="12" cy="12" r="3"/><path d="m19 13 2 1-2 4-2-1-2 1v2H9v-2l-2-1-2 1-2-4 2-1v-2l-2-1 2-4 2 1 2-1V4h6v2l2 1 2-1 2 4-2 1z"/>'),
  sun: ic('<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>'),
  timer: ic('<circle cx="12" cy="13" r="8"/><path d="M12 9v4l3 2M9 2h6M12 2v3"/>'),
  'trash-2': ic('<path d="M3 6h18M8 6V4h8v2M6 6l1 15h10l1-15M10 10v7M14 10v7"/>'),
  usb: ic('<path d="M12 19V4M9 7l3-3 3 3M12 12l-4 4M12 15l4 4M8 16H6a2 2 0 1 0 0 4h2M16 19h2a2 2 0 1 0 0-4h-2"/><circle cx="12" cy="21" r="1"/>'),
  variable: ic('<path d="M5 5c3 0 3 14 6 14s3-14 8-14M4 19h4M16 5h4"/>'),
  'volume-2': ic('<path d="M3 9v6h4l5 4V5L7 9zM16 9a5 5 0 0 1 0 6M18 6a9 9 0 0 1 0 12"/>'),
  wifi: ic('<path d="M3 9a14 14 0 0 1 18 0M6 12a9 9 0 0 1 12 0M9 15a4 4 0 0 1 6 0M12 19h.01"/>'),
};

export const CATS = {
  cond: { label: 'Condition', color: '#f5b14a' },
  act: { label: 'Action', color: '#8b9cff' },
  tgt: { label: 'Input', color: '#3ddbd9' },
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
    desc: 'Compares RAM (MB) of a wired Process Input, or of the whole system, to a threshold.',
    defaults: { op: 'above', thresholdMb: 800, mode: 'check', pollSec: 1, stableForSec: 0, timeoutSec: 0, onFail: 'stop' },
    fields: [
      { key: 'op', label: 'Satisfied when RAM is', type: 'select', options: [['above', 'Above'], ['below', 'Below']] },
      { key: 'thresholdMb', label: 'Threshold', type: 'number', min: 1, max: 1048576, step: 10, unit: 'MB' },
      { key: 'mode', label: 'Mode', type: 'select', options: [['check', 'Check once (IF / ELSE)'], ['wait', 'Wait until satisfied'], ['wait_change', 'Wait until threshold state changes']] },
      { key: 'pollSec', label: 'Poll every', type: 'number', min: 0.25, max: 3600, step: 0.25, unit: 's', show: c => c.mode !== 'check' },
      { key: 'stableForSec', label: 'Require condition to remain satisfied for', type: 'number', min: 0, max: 3600, step: 0.5, unit: 's', show: c => c.mode !== 'check' },
      { ...TIMEOUT, show: c => c.mode !== 'check' },
      ONFAIL,
    ],
    summary: c => `${c.op === 'below' ? 'Below' : 'Above'} ${num(c.thresholdMb)} MB · ${c.mode === 'wait' ? 'waits' + (c.timeoutSec > 0 ? ` ≤ ${dur(c.timeoutSec)}` : '') : c.mode === 'wait_change' ? 'waits for a change' : 'checks once'}`,
    help: 'Connect a Process Input to measure that process working set. With no input, this condition measures used system RAM. State-change mode watches for the selected threshold condition to flip.',
  },
  'cond.proc_running': {
    cat: 'cond', title: 'Process Running', icon: I.proc,
    desc: 'Checks whether a process (by exe name) is running, or waits for it to start, stop, or change state.',
    defaults: { name: '', mode: 'check', pollSec: 2, stableForSec: 0, timeoutSec: 0, onFail: 'stop' },
    fields: [
      { key: 'name', label: 'Process name', type: 'text', placeholder: 'chrome.exe', required: true },
      { key: 'mode', label: 'Mode', type: 'select', options: [['check', 'Check once (IF running / ELSE not)'], ['wait_start', 'Wait until it starts'], ['wait_stop', 'Wait until it stops'], ['wait_change', 'Wait until running state changes']] },
      { key: 'pollSec', label: 'Poll every', type: 'number', min: 0.25, max: 3600, step: 0.25, unit: 's', show: c => c.mode !== 'check' },
      { key: 'stableForSec', label: 'Require condition to remain satisfied for', type: 'number', min: 0, max: 3600, step: 0.5, unit: 's', show: c => c.mode !== 'check' },
      { ...TIMEOUT, show: c => c.mode !== 'check' },
      ONFAIL,
    ],
    summary: c => !c.name ? 'No process set' : c.mode === 'wait_start' ? `Wait for ${c.name} to start` : c.mode === 'wait_stop' ? `Wait for ${c.name} to stop` : c.mode === 'wait_change' ? `Wait for ${c.name} state change` : `Is ${c.name} running?`,
    help: 'Include the .exe extension. Needs no input block. Stable duration helps avoid short-lived process-state flicker.',
  },
  'act.stop_proc': {
    cat: 'act', title: 'Stop Process', icon: I.stop,
    desc: 'Closes a process by exe name, or the wired Process Input.',
    defaults: { mode: 'name', name: '', force: false, timeoutSec: 10, onFail: 'stop' },
    fields: [
      { key: 'mode', label: 'Pick process', type: 'select', options: [['name', 'By exe name'], ['target', 'From wired Process Input']] },
      { key: 'name', label: 'Process name', type: 'text', placeholder: 'notepad.exe', required: true, show: c => c.mode === 'name' },
      { key: 'force', label: 'Force-kill immediately', type: 'bool' },
      TIMEOUT,
      ONFAIL,
    ],
    summary: c => `Stop ${c.mode === 'name' ? (c.name || 'no name set') : 'Process Input'}${c.force ? ' (force)' : ''}`,
    help: 'Without force it sends a close request to the process windows, waits 5 s, then force-kills. By name only the first match is stopped. Protected processes need 00047 run as administrator. It refuses to stop 00047 itself.',
  },
  'act.notify': {
    cat: 'act', title: 'Notification', icon: I.bell,
    desc: 'Shows a Windows toast notification.',
    defaults: { title: '00047', body: 'Hello', sound: true },
    fields: [
      { key: 'title', label: 'Title', type: 'text', required: true, maxLen: 64 },
      { key: 'body', label: 'Message', type: 'text', required: true, maxLen: 256 },
      { key: 'sound', label: 'Play sound', type: 'bool' },
    ],
    summary: c => c.body ? `"${c.body.slice(0, 40)}${c.body.length > 40 ? '…' : ''}"` : 'No message',
  },
  'act.trim': {
    cat: 'act', title: 'Working-Set Trim', icon: I.trim,
    desc: 'Releases part of a process\'s working set (needs a Process Input).',
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
    desc: 'Entry point for this workflow. It does not launch a program.',
    defaults: {},
    fields: [],
    summary: () => 'Workflow entry point',
    help: 'Connect the first step from this block. Use Launch File to launch a program. A Loop block automatically returns execution here without a return wire.',
  },
  'act.launch': {
    cat: 'act', title: 'Launch File', icon: I.file,
    desc: 'Launches the program, document, or shortcut selected by a File Input.',
    requires: 'tgt.file',
    defaults: { args: '', timeoutSec: 15, onFail: 'stop' },
    fields: [
      { key: 'args', label: 'Arguments (optional)', type: 'text', placeholder: '--example' },
      TIMEOUT,
      ONFAIL,
    ],
    summary: c => `Launch file${c.args ? ' · with arguments' : ''}`,
    help: 'Connect a File Input to the teal input/output binding for this action, then wire the flow through Launch File normally.',
  },
  'act.loop': {
    cat: 'act', title: 'Loop', icon: I.repeat,
    desc: 'Returns execution to the Start block. No return wire is needed.',
    defaults: {},
    fields: [],
    summary: () => 'Return to Start',
    help: 'Place this at the end of a flow to repeat it. When reached, it jumps to the automation’s single enabled Start block automatically. It has no output wires.',
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
    cat: 'tgt', title: 'Process Input', icon: I.window,
    desc: 'Provides the foreground window\'s process, or a process chosen by name.',
    defaults: { mode: 'foreground', name: '', ignoreSelf: true },
    fields: [
      { key: 'mode', label: 'Pick', type: 'select', options: [['foreground', 'Foreground window'], ['name', 'By exe name']] },
      { key: 'name', label: 'Process name', type: 'text', placeholder: 'chrome.exe', required: true, show: c => c.mode === 'name' },
      { key: 'ignoreSelf', label: 'Fail if the foreground is 00047 itself', type: 'bool', show: c => c.mode !== 'name' },
    ],
    summary: c => (c.mode === 'name' ? (c.name || 'No name set') : 'Foreground window process'),
    help: 'Connect a Condition or Action flow output into the left input to place this Process Input in the workflow. The teal output connects to the next Condition or Action, binds this process, and continues execution.',
  },
  'tgt.file': {
    cat: 'tgt', title: 'File Input', icon: I.file,
    desc: 'Provides a file or program path.',
    defaults: { path: '' },
    fields: [{ key: 'path', label: 'Full path', type: 'file', placeholder: 'C:\\Tools\\app.exe', required: true }],
    summary: c => c.path || 'No path set',
    help: 'Connect Start or another Condition/Action flow output into the left input to place this File Input in the workflow. The teal output connects to the next Condition or Action, binds this file, and continues execution.',
  },
};

export function fieldError(f, cfg) {
  if (f.show && !f.show(cfg)) return null;
  const v = cfg[f.key];
  if (f.type === 'number') {
    if (typeof v !== 'number' || !Number.isFinite(v)) return 'Enter a number';
    if (v < f.min || v > f.max) return `${f.min} – ${f.max}`;
  }
  if (f.type === 'time' && !/^([01]\d|2[0-3]):[0-5]\d$/.test(String(v ?? ''))) return 'Use HH:MM';
  if (f.type === 'text' || f.type === 'textarea' || f.type === 'file') {
    const s = String(v ?? '').trim();
    if (f.required && !s) return 'Required';
    if (f.maxLen && s.length > f.maxLen) return `Max ${f.maxLen} characters`;
  }
  if (f.type === 'bool' && f.required && !v) return 'Required';
  return null;
}


export function validate(kind, cfg) {
  const b = BLOCKS[kind];
  if (!b) return ['Unknown block type'];
  return b.fields.map(f => { const e = fieldError(f, cfg); return e ? `${f.label}: ${e}` : null; }).filter(Boolean);
}


// ---------- declarative specs (native spec-driven blocks + user block packs) ----------
const PRESETS = { timeout: TIMEOUT, onFail: ONFAIL };
const showFn = s => {
  if (!s || typeof s !== 'object') return undefined;
  return c => {
    const v = c[s.key];
    if ('eq' in s) return v === s.eq;
    if ('ne' in s) return v !== s.ne;
    if (Array.isArray(s.in)) return s.in.includes(v);
    return true;
  };
};
const tpl = (t, c) => String(t).replace(/\{(\w+)(\?([^}]*))?\}/g, (_, k, q, txt) => (q ? (c[k] ? txt : '') : (c[k] ?? '')));

export function compileSpec(sp) {
  const cat = sp.category === 'condition' ? 'cond' : sp.category === 'target' ? 'tgt' : 'act';
  const fields = (sp.fields || []).map(f => {
    const o = { ...(f.preset ? PRESETS[f.preset] : {}), ...f };
    delete o.preset;
    if (o.show && typeof o.show === 'object') o.show = showFn(o.show);
    return o;
  });
  const defaults = {};
  for (const f of fields) {
    defaults[f.key] = f.default !== undefined ? f.default
      : f.type === 'bool' ? false
      : f.type === 'number' ? (f.min ?? 0)
      : f.type === 'select' ? f.options[0][0] : '';
  }
  Object.assign(defaults, sp.defaults || {});
  const labelled = c => {
    const o = { ...c };
    for (const f of fields) if (f.type === 'select') { const m = f.options.find(x => x[0] === c[f.key]); if (m) o[f.key] = m[1]; }
    return o;
  };
  return {
    cat, title: sp.title, icon: I[sp.icon] || I.note, desc: sp.description || '',
    defaults, fields, summary: c => tpl(sp.summary || sp.title, labelled(c)), help: sp.help, requires: sp.requires, origin: sp.origin,
  };
}

/** Adds blocks described by backend specs. Built-in JS definitions win over a spec with the same kind. */
export function registerSpecs(specs) {
  for (const sp of specs) if (sp.kind && !BLOCKS[sp.kind]) BLOCKS[sp.kind] = compileSpec(sp);
}
