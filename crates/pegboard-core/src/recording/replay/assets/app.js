// The replay page of one recording. It draws what `data` says (the server judges which
// screen goes with which action and tab: model.rs `views`) and makes no judgment of its own.
// Everything shown comes from the recording's own files (page URLs, error texts, what a
// person did) and is put in as text only (`textContent`); nothing is ever parsed as HTML.
'use strict';

const $ = (id) => document.getElementById(id);

// ---- words ----

const NO_SCREEN = {
  pruned: '화면 지워짐',
  capped: '화면 기록 멈춤(녹화 하나 1GB)',
  off: '화면 기록 꺼짐',
  missing: '화면 없음',
};

const SIGN = { failed: '✗', denied: '⛔', human: '👤', ok: '✓' };
const KIND_WORD = { failed: '실패', denied: '거부', human: '사람 개입' };

// Only names the server's frame route takes are ever asked for.
const FRAME_NAME = /^[0-9A-F]{32}\/[0-9]{6,10}\.jpg$/;

// ---- state ----

const state = {
  model: null,
  actions: [],
  views: [],
  incidentKind: new Map(), // seq -> failed | denied | human
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
  // A typed value the log did not keep is shown as it was logged, with a lock.
  return [action.command].concat(action.args || []).join(' ') + (action.redacted ? ' 🔒' : '');
}

// ---- rendering ----

function renderHead() {
  const m = state.model.manifest || {};
  $('title').textContent = '세션 "' + (m.sessionName || m.sessionId || '') + '"';
  document.title = 'pegboard 재생 · ' + (m.sessionName || m.recordingId || '');
  const commands = state.actions.filter((a) => a.kind !== 'human').length;
  const count = (k) => (state.model.incidents || []).filter((i) => i.kind === k).length;
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
  const known = times.filter((t) => !Number.isNaN(t));
  const least = known.reduce((a, b) => Math.min(a, b), Infinity);
  const most = known.reduce((a, b) => Math.max(a, b), -Infinity);
  let start = Date.parse(m.startedAt);
  let end = Date.parse(m.endedAt);
  if (Number.isNaN(start)) start = known.length ? least : 0;
  if (Number.isNaN(end)) end = known.length ? Math.max(most, start) : start;
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

function renderTabs(view, shown) {
  const box = $('tabs');
  box.replaceChildren();
  (state.model.tabs || []).forEach((target, n) => {
    const tv = view && view.tabs ? view.tabs[target] : null;
    const url = tv && tv.url;
    const b = el('button', 'tab', n + 1 + ': ' + (url ? hostOf(url) : target.slice(0, 8)));
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

function renderExtras(view) {
  const box = $('extra-lines');
  box.replaceChildren();
  const lines = [];
  const slice = (all, range) => (Array.isArray(range) ? (all || []).slice(range[0], range[1]) : []);
  for (const n of slice(state.model.network, view && view.network)) {
    const status = n.error ? n.error : n.status === null || n.status === undefined ? '-' : String(n.status);
    lines.push(['network', clock(n.ts) + ' ' + n.method + ' ' + n.url + ' ' + status]);
  }
  for (const c of slice(state.model.console, view && view.console)) {
    lines.push(['console', clock(c.ts) + ' console.' + c.level + ': ' + c.text]);
  }
  for (const [kind, text] of lines) {
    const li = el('li', kind, text);
    li.dataset.kind = kind;
    box.append(li);
  }
  $('extras').hidden = lines.length === 0;
}

function showNoFrame(reason) {
  const img = $('frame');
  img.hidden = true;
  img.removeAttribute('src');
  delete img.dataset.name;
  const p = $('no-frame');
  p.textContent = NO_SCREEN[reason] || NO_SCREEN.missing;
  p.dataset.reason = NO_SCREEN[reason] ? reason : 'missing';
  p.hidden = false;
}

// The tab shown for action `i`: the one chosen, else the action's own, else the first.
function shownTab(i) {
  const a = state.actions[i];
  const view = state.views[i];
  const tabs = state.model.tabs || [];
  if (state.tab) return state.tab;
  if (a && a.targetId && view && view.tabs && view.tabs[a.targetId]) return a.targetId;
  return tabs[0] || null;
}

function show() {
  const a = state.actions[state.index];
  if (!a) {
    showNoFrame('missing');
    return;
  }
  const view = state.views[state.index];
  const target = shownTab(state.index);
  const tv = view && view.tabs && target ? view.tabs[target] : null;
  // `after` is sent only when it differs from `before`.
  const screen = (tv && (state.side === 'after' ? tv.after || tv.before : tv.before)) || { kind: 'missing' };
  const img = $('frame');
  if (screen.kind === 'frame' && FRAME_NAME.test(screen.name)) {
    $('no-frame').hidden = true;
    img.dataset.name = screen.name;
    img.hidden = false;
    img.src = 'frames/' + screen.name;
  } else {
    showNoFrame(screen.kind);
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
  renderTabs(view, target);
  renderExtras(view);
  document.body.dataset.seq = String(a.seq);
  document.body.dataset.side = state.side;
  document.body.dataset.tab = target || '';
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
  state.views = Array.isArray(model.views) ? model.views : [];
  for (const inc of model.incidents || []) state.incidentKind.set(inc.seq, inc.kind);
  renderHead();
  renderList();
  renderTimeline();
  const first = state.actions.findIndex((a) => a.seq === model.startSeq);
  $('frame').addEventListener('error', () => {
    const img = $('frame');
    if (img.hidden) return;
    const asked = img.dataset.name;
    // A frame the server will not give: gone, or the whole link (ended, recording deleted)?
    // The answer counts only while that frame is still the one shown.
    const still = () => !img.hidden && img.dataset.name === asked;
    fetch('data', { method: 'HEAD', cache: 'no-store', credentials: 'omit' })
      .then((r) => {
        if (!still()) return;
        if (r.ok) showNoFrame('missing');
        else fail();
      })
      .catch(() => {
        if (still()) fail();
      });
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
