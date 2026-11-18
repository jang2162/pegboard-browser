// The replay page of one recording. Everything shown comes from `data` (the recording's own
// files: page URLs, error texts, what a person did) and is put in as text only
// (`textContent`); nothing is ever parsed as HTML.
'use strict';

const $ = (id) => document.getElementById(id);

// ---- the rules shared with model.rs (change both) ----

// A time as a string that sorts as the time does (model.rs `ts_key`).
function tsKey(ts) {
  if (typeof ts !== 'string') return '';
  const m = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?Z$/.exec(ts);
  if (!m) return ts;
  return m[1] + '.' + (m[2] || '').padEnd(9, '0');
}

// The screen of tab `target` at `ts` (model.rs `frame_at`).
function frameAt(index, navigation, target, ts) {
  const at = tsKey(ts);
  let latest = null;
  let latestKey = null;
  for (const line of index || []) {
    const key = tsKey(line.ts);
    if (key > at) continue;
    if (line.kind === 'capped') return { kind: 'capped' };
    if (line.kind !== 'off' && !line.file) continue;
    if (latest === null || key >= latestKey) {
      latest = line;
      latestKey = key;
    }
  }
  if (latest === null) return { kind: 'missing' };
  if (latest.kind === 'off') return { kind: 'off' };
  const navigated = (navigation || []).some((n) => {
    if (n.targetId !== target || n.event !== 'navigated') return false;
    const k = tsKey(n.ts);
    return k > latestKey && k <= at;
  });
  return navigated ? { kind: 'missing' } : { kind: 'frame', line: latest };
}

// ---- words ----

const NO_SCREEN = {
  pruned: '화면 지워짐',
  capped: '화면 기록 멈춤(녹화 하나 1GB)',
  off: '화면 기록 꺼짐',
  missing: '화면 없음',
};

const SIGN = { failed: '✗', denied: '⛔', human: '👤', ok: '✓' };
const KIND_WORD = { failed: '실패', denied: '거부', human: '사람 개입' };

const FRAME_NAME = /^[0-9A-F]{32}\/[0-9]{6,10}\.jpg$/;
const TARGET_ID = /^[0-9A-F]{32}$/;

// ---- state ----

const state = {
  model: null,
  actions: [],
  incidentKind: new Map(), // seq -> failed | denied | human
  tabs: [],
  index: 0, // the selected action
  side: 'before',
  tab: null, // the tab shown; null: the selected action's
  lastMark: null, // the timeline mark clicked last, while nothing else was chosen since
};

// ---- helpers ----

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

// The local time of day of `ts`, `HH:MM:SS`.
function clock(ts) {
  const t = Date.parse(ts);
  if (Number.isNaN(t)) return String(ts || '');
  const d = new Date(t);
  const two = (n) => String(n).padStart(2, '0');
  return two(d.getHours()) + ':' + two(d.getMinutes()) + ':' + two(d.getSeconds());
}

function hostOf(url) {
  try {
    const u = new URL(url);
    return u.host || u.protocol;
  } catch (_) {
    return String(url || '');
  }
}

function kindOf(action) {
  return state.incidentKind.get(action.seq) || 'ok';
}

function describe(action) {
  if (action.kind === 'human') {
    return '사람 개입: ' + (action.what || '') + (action.certain === false ? ' (가능성)' : '');
  }
  return [action.command].concat(action.args || []).join(' ');
}

// The time "after" an action: the next action's, else the end of the recording.
function afterTs(i) {
  const next = state.actions[i + 1];
  if (next) return next.ts;
  return (state.model.manifest && state.model.manifest.endedAt) || '9999-12-31T23:59:59Z';
}

// The last URL a tab is known to have had at `ts`.
function urlOf(target, ts) {
  const at = tsKey(ts);
  let url = null;
  for (const n of state.model.navigation || []) {
    if (n.targetId === target && n.url && tsKey(n.ts) <= at) url = n.url;
  }
  return url;
}

// ---- rendering ----

function renderHead() {
  const m = state.model.manifest || {};
  $('title').textContent = '세션 "' + (m.sessionName || m.sessionId || '') + '"';
  document.title = 'pegboard 재생 · ' + (m.sessionName || m.recordingId || '');
  const commands = state.actions.filter((a) => a.kind !== 'human').length;
  const count = (k) => state.model.incidents.filter((i) => i.kind === k).length;
  const parts = [
    clock(m.startedAt) + '–' + (m.endedAt ? clock(m.endedAt) : '진행 중'),
    '명령 ' + commands,
    '실패 ' + count('failed'),
    '거부 ' + count('denied'),
    '사람 개입 ' + count('human'),
  ];
  if (m.screen === 'pruned') parts.push(NO_SCREEN.pruned);
  if (m.screen === 'capped') parts.push(NO_SCREEN.capped);
  if (m.screen === 'none') parts.push('화면 기록 없음');
  parts.push(m.recordingId || '');
  $('summary').textContent = parts.join(' · ');
}

function renderList() {
  const list = $('actions');
  list.replaceChildren();
  state.actions.forEach((a, i) => {
    const kind = kindOf(a);
    const li = el('li', kind);
    li.dataset.seq = String(a.seq);
    li.append(el('span', 'time', clock(a.ts)), el('span', 'sign', SIGN[kind]), el('span', 'what', describe(a)));
    if (a.error) li.append(el('span', 'detail', a.error));
    if (a.url) li.append(el('span', 'detail', a.url));
    li.addEventListener('click', () => select(i, 'before'));
    list.append(li);
  });
  if (state.actions.length === 0) list.append(el('li', null, '동작 없음'));
}

function renderTimeline() {
  const line = $('timeline');
  line.replaceChildren();
  const m = state.model.manifest || {};
  const times = state.actions.map((a) => Date.parse(a.ts));
  let start = Date.parse(m.startedAt);
  let end = Date.parse(m.endedAt);
  const known = times.filter((t) => !Number.isNaN(t));
  if (Number.isNaN(start)) start = known.length ? Math.min(...known) : 0;
  if (Number.isNaN(end)) end = known.length ? Math.max(...known) : start;
  const span = Math.max(end - start, 1);
  state.actions.forEach((a, i) => {
    const kind = kindOf(a);
    const t = times[i];
    const where = Number.isNaN(t)
      ? (100 * (i + 0.5)) / state.actions.length
      : Math.min(100, Math.max(0, (100 * (t - start)) / span));
    const mark = el('button', 'mark ' + kind, kind === 'ok' ? '' : SIGN[kind]);
    mark.type = 'button';
    mark.dataset.seq = String(a.seq);
    mark.dataset.kind = kind;
    mark.title = clock(a.ts) + ' ' + (KIND_WORD[kind] ? KIND_WORD[kind] + ': ' : '') + describe(a);
    mark.style.left = where + '%';
    mark.addEventListener('click', () => {
      // First the screen before; once more, the screen after.
      const again = state.lastMark === i && state.side === 'before' && state.tab === null;
      select(i, again ? 'after' : 'before');
      state.lastMark = i;
    });
    line.append(mark);
  });
}

// The tabs of the recording, in the order they first appear.
function collectTabs() {
  const first = new Map();
  const see = (target, ts) => {
    if (!target || !TARGET_ID.test(target)) return;
    const k = tsKey(ts);
    if (!first.has(target) || k < first.get(target)) first.set(target, k);
  };
  for (const a of state.actions) see(a.targetId, a.ts);
  for (const n of state.model.navigation || []) see(n.targetId, n.ts);
  for (const [target, index] of Object.entries(state.model.screens || {})) {
    see(target, index.length ? index[0].ts : '9999');
  }
  return [...first.entries()].sort((a, b) => (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0)).map((e) => e[0]);
}

function renderTabs(shown, ts) {
  const box = $('tabs');
  box.replaceChildren();
  state.tabs.forEach((target, n) => {
    const url = urlOf(target, ts);
    const label = (n + 1) + ': ' + (url ? hostOf(url) : target.slice(0, 8));
    const b = el('button', 'tab', label);
    b.type = 'button';
    b.dataset.target = target;
    b.title = url || target;
    b.setAttribute('aria-pressed', target === shown ? 'true' : 'false');
    b.addEventListener('click', () => {
      const a = state.actions[state.index];
      state.tab = a && a.targetId === target ? null : target;
      state.lastMark = null;
      show();
    });
    box.append(b);
  });
}

function renderExtras(i) {
  const box = $('extra-lines');
  box.replaceChildren();
  const from = i > 0 ? tsKey(state.actions[i - 1].ts) : '';
  const to = tsKey(afterTs(i));
  const lines = [];
  for (const n of state.model.network || []) {
    const k = tsKey(n.ts);
    if (k > from && k <= to) {
      const status = n.error ? n.error : n.status === null || n.status === undefined ? '-' : String(n.status);
      lines.push([k, clock(n.ts) + ' ' + n.method + ' ' + n.url + ' ' + status]);
    }
  }
  for (const c of state.model.console || []) {
    const k = tsKey(c.ts);
    if (k > from && k <= to) lines.push([k, clock(c.ts) + ' console.' + c.level + ': ' + c.text]);
  }
  lines.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  for (const [, text] of lines) box.append(el('li', null, text));
  $('extras').hidden = lines.length === 0;
}

// What to show for the selection: a frame name, or why there is none.
function chooseScreen(a, i, target) {
  const m = state.model.manifest || {};
  if (m.screen === 'pruned') return { none: 'pruned' };
  const named = state.side === 'before' ? a.frameBefore : a.frameAfter;
  if (target === a.targetId && named && FRAME_NAME.test(named)) {
    return { name: named, ts: a.ts, url: a.url };
  }
  const ts = state.side === 'before' ? a.ts : afterTs(i);
  const index = (state.model.screens || {})[target] || [];
  const at = frameAt(index, state.model.navigation, target, ts);
  if (at.kind === 'frame') {
    const name = target + '/' + at.line.file;
    if (FRAME_NAME.test(name)) return { name, ts: at.line.ts, url: at.line.url };
    return { none: 'missing' };
  }
  return { none: at.kind };
}

function showNoFrame(reason) {
  const img = $('frame');
  img.hidden = true;
  img.removeAttribute('src');
  const p = $('no-frame');
  p.textContent = NO_SCREEN[reason] || NO_SCREEN.missing;
  p.dataset.reason = reason;
  p.hidden = false;
}

function show() {
  const a = state.actions[state.index];
  if (!a) {
    showNoFrame('missing');
    return;
  }
  const target = state.tab || a.targetId || state.tabs[0] || null;
  const screen = target ? chooseScreen(a, state.index, target) : { none: 'missing' };
  const img = $('frame');
  if (screen.name) {
    $('no-frame').hidden = true;
    img.dataset.name = screen.name;
    img.hidden = false;
    img.src = 'frames/' + screen.name;
  } else {
    delete img.dataset.name;
    showNoFrame(screen.none);
  }
  const info = [state.side === 'before' ? '직전' : '직후', describe(a)];
  if (screen.ts) info.push(clock(screen.ts));
  if (screen.url) info.push(screen.url);
  $('frame-info').textContent = info.join(' · ');
  $('before').setAttribute('aria-pressed', state.side === 'before' ? 'true' : 'false');
  $('after').setAttribute('aria-pressed', state.side === 'after' ? 'true' : 'false');
  for (const node of document.querySelectorAll('#actions li, #timeline .mark')) {
    const current = node.dataset.seq === String(a.seq);
    node.setAttribute('aria-current', current ? 'true' : 'false');
    if (current && node.tagName === 'LI') node.scrollIntoView({ block: 'nearest' });
  }
  renderTabs(target, state.side === 'before' ? a.ts : afterTs(state.index));
  renderExtras(state.index);
  document.body.dataset.seq = String(a.seq);
  document.body.dataset.side = state.side;
}

function select(i, side) {
  if (i < 0 || i >= state.actions.length) return;
  state.index = i;
  state.side = side;
  state.tab = null;
  state.lastMark = null;
  show();
}

// ---- start ----

function start(model) {
  state.model = model;
  state.actions = Array.isArray(model.actions) ? model.actions : [];
  for (const inc of model.incidents || []) state.incidentKind.set(inc.seq, inc.kind);
  state.tabs = collectTabs();
  renderHead();
  renderList();
  renderTimeline();
  const first = state.actions.findIndex((a) => a.seq === model.startSeq);
  $('frame').addEventListener('error', () => {
    if (!$('frame').hidden) showNoFrame('missing');
  });
  $('before').addEventListener('click', () => {
    state.lastMark = null;
    state.side = 'before';
    show();
  });
  $('after').addEventListener('click', () => {
    state.lastMark = null;
    state.side = 'after';
    show();
  });
  document.addEventListener('keydown', (e) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.key === 'ArrowLeft') {
      select(state.index - 1, 'before');
      e.preventDefault();
    } else if (e.key === 'ArrowRight') {
      select(state.index + 1, 'before');
      e.preventDefault();
    }
  });
  select(first >= 0 ? first : 0, 'before');
  if (state.actions.length === 0) show();
  document.body.dataset.ready = 'true';
}

function fail() {
  const p = $('error');
  p.textContent = '녹화를 열 수 없습니다. 링크가 끝났거나(30분) 녹화가 지워졌을 수 있습니다. pegboard recording open으로 다시 여세요.';
  p.hidden = false;
  $('main').hidden = true;
  document.body.dataset.ready = 'error';
}

fetch('data', { cache: 'no-store', credentials: 'omit' })
  .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
  .then(start)
  .catch(fail);
