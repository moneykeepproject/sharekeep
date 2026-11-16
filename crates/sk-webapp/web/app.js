// ShareKeep's web app. Plain JavaScript, no outside code. Everything shown is worked out on
// the hub (/api/v1/view); new expenses, payments and approvals wait in an outbox on this
// device while the hub can't be reached. Text always goes in with textContent.
'use strict';

const SYNC_EVERY_MS = 30000;
const METHODS = ['Equal', 'Percentages', 'Shares', 'By income'];

// ----- small helpers -------------------------------------------------------------------------

const $ = (sel) => document.querySelector(sel);

/** An element: h('div', {class: 'card', onclick: f}, child, 'text', …). */
function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'class') el.className = v;
    else if (k === 'value') el.value = v;
    else if (k === 'checked') el.checked = !!v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat()) {
    if (kid === undefined || kid === null || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

const store = {
  get(k, d) {
    try { const v = localStorage.getItem('sk.' + k); return v === null ? d : JSON.parse(v); } catch { return d; }
  },
  set(k, v) { try { localStorage.setItem('sk.' + k, JSON.stringify(v)); } catch { /* private mode */ } },
  del(k) { try { localStorage.removeItem('sk.' + k); } catch { /* private mode */ } },
};

function randomId() {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

let toastTimer = 0;
function toast(msg) {
  const t = $('#toast');
  t.textContent = msg;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 5000);
}

/** Cents from "12.50" for the book's currency (no decimals for JPY, KRW…), or null. */
function parseAmount(text, currency) {
  const t = String(text).trim().replace(/[,\s$€£¥]/g, '');
  const decimals = ['JPY', 'KRW', 'VND', 'CLP', 'ISK'].includes(currency) ? 0 : 2;
  const re = decimals ? /^\d+(\.\d{1,2})?$/ : /^\d+$/;
  if (!re.test(t)) return null;
  const [whole, frac = ''] = t.split('.');
  return Number(whole) * 10 ** decimals + Number((frac + '00').slice(0, decimals) || 0);
}

function formatPlain(cents, currency) {
  const decimals = ['JPY', 'KRW', 'VND', 'CLP', 'ISK'].includes(currency) ? 0 : 2;
  return (cents / 10 ** decimals).toFixed(decimals);
}

function signClass(sign) { return sign > 0 ? 'pos' : sign < 0 ? 'neg' : 'zero'; }

// ----- state ---------------------------------------------------------------------------------

const S = {
  token: store.get('token', null),
  member: store.get('member', null),
  book: store.get('book', null),
  device: store.get('device', ''),
  view: store.get('view', null),
  outbox: store.get('outbox', []),
  refused: store.get('refused', []),
  page: 'home',
  agreement: null,
  tab: 'activity',
  offline: false,
  problem: '',
  busy: false,
  pairCode: null,
};

function saveQueue() {
  store.set('outbox', S.outbox);
  store.set('refused', S.refused);
}

// ----- the hub --------------------------------------------------------------------------------

class HubError extends Error {
  constructor(message, status) { super(message); this.status = status; }
}

/** A call to the hub. A network failure is a HubError with status 0. */
async function api(method, path, body) {
  let r;
  try {
    r = await fetch(path, {
      method,
      cache: 'no-store',
      headers: Object.assign(
        { Accept: 'application/json' },
        S.token ? { Authorization: 'Bearer ' + S.token } : {},
        body ? { 'Content-Type': 'application/json' } : {},
      ),
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch {
    throw new HubError("The household hub can't be reached.", 0);
  }
  let data = {};
  try { data = await r.json(); } catch { /* not JSON */ }
  if (!r.ok) throw new HubError(data.error || 'The hub answered ' + r.status + '.', r.status);
  return data;
}

function retryable(e) { return e.status === 0 || e.status >= 500 || e.status === 408 || e.status === 429; }

/** Send what waits, in order. Stops at the first one that may work later. */
async function flush() {
  while (S.outbox.length) {
    const item = S.outbox[0];
    try {
      await api('POST', '/api/v1/records', item);
      S.outbox.shift();
    } catch (e) {
      if (retryable(e)) { saveQueue(); throw e; }
      S.outbox.shift();
      S.refused.push({ kind: item.kind, reason: e.message });
    }
    saveQueue();
  }
}

async function sync() {
  if (!S.token || S.busy) return;
  S.busy = true;
  render();
  try {
    await flush();
    const v = await api('GET', '/api/v1/view');
    S.view = v;
    store.set('view', v);
    S.offline = false;
    S.problem = '';
  } catch (e) {
    S.offline = e.status === 0;
    S.problem = e.message;
    if (e.status === 401) {
      S.problem = 'This device was removed from the household, or the hub forgot it. Pair it again.';
    }
  } finally {
    S.busy = false;
    render();
  }
}

/** Queue something that may wait offline (expense, payment, approval), then send it. */
function queue(kind, body) {
  S.outbox.push({ client_id: randomId(), kind, body });
  saveQueue();
  return sync();
}

/** Send something that needs the hub now (a rule proposal, a dispute, a delete, a close). */
async function sendNow(kind, body) {
  try {
    await api('POST', '/api/v1/records', { client_id: randomId(), kind, body });
    await sync();
    return true;
  } catch (e) {
    toast(e.status === 0 ? e.message + " This change needs the hub: try again when you're home." : e.message);
    return false;
  }
}

// ----- pairing -------------------------------------------------------------------------------

function readPairCode() {
  const m = location.hash.match(/pair=([A-Za-z0-9]{8})/);
  if (m) {
    S.pairCode = m[1].toUpperCase();
    history.replaceState(null, '', location.pathname);
  }
}

async function pair(code, pin, device) {
  try {
    const a = await api('POST', '/api/v1/pair', { code: code.replace(/[-\s]/g, '').toUpperCase(), pin, device });
    S.token = a.token;
    S.member = a.member;
    S.book = a.book;
    S.device = device;
    S.pairCode = null;
    S.view = null;
    S.outbox = [];
    S.refused = [];
    store.set('token', a.token);
    store.set('member', a.member);
    store.set('book', a.book);
    store.set('device', device);
    saveQueue();
    S.page = 'home';
    await sync();
  } catch (e) {
    toast(e.message);
  }
}

function forget() {
  for (const k of ['token', 'member', 'book', 'view', 'outbox', 'refused']) store.del(k);
  Object.assign(S, { token: null, member: null, book: null, view: null, outbox: [], refused: [], page: 'home' });
  render();
}

// ----- screens -------------------------------------------------------------------------------

function agreementView() {
  return S.view && S.view.agreements.find((a) => a.id === S.agreement);
}

function render() {
  const paired = !!S.token;
  const ag = agreementView();
  const inAgreement = paired && S.page === 'agreement' && ag;
  $('#title').textContent = inAgreement ? ag.name : (S.book && S.book.name) || 'ShareKeep';
  $('#back').hidden = !inAgreement;
  $('#sync').hidden = !paired;
  $('#sync').textContent = S.busy ? 'Syncing…' : 'Sync';
  $('#nav').hidden = !paired;
  for (const b of document.querySelectorAll('#nav button')) {
    const p = b.dataset.page;
    b.classList.toggle('on', p === S.page || (p === 'home' && S.page === 'agreement'));
    const badge = b.querySelector('.badge');
    if (badge) {
      const n = p === 'approvals' ? (S.view ? S.view.approvals.length : 0) : S.refused.length;
      badge.hidden = !n;
      badge.textContent = n;
    }
  }
  const tabs = $('#tabs');
  tabs.hidden = !inAgreement;
  tabs.replaceChildren(...(inAgreement ? [['activity', 'Activity'], ['members', 'Members'], ['rules', 'Rules'], ['periods', 'Periods']]
    .map(([k, l]) => h('button', { class: 'chip' + (S.tab === k ? ' on' : ''), onclick: () => { S.tab = k; render(); } }, l)) : []));

  let parts;
  if (!paired) parts = joinPage();
  else if (inAgreement) parts = [...status(), ...agreementPage(ag)];
  else if (S.page === 'approvals') parts = [...status(), ...approvalsPage()];
  else if (S.page === 'settings') parts = [...status(), ...settingsPage()];
  else parts = [...status(), ...homePage()];
  // optional parts are null: leave them out
  $('#page').replaceChildren(...parts.flat().filter((x) => x));
}

function status() {
  const out = [];
  if (S.problem) {
    out.push(h('div', { class: 'banner warn' }, S.offline
      ? "Offline: the household hub can't be reached. New expenses, payments and approvals wait here and are sent when you're home."
      : S.problem));
  }
  if (S.outbox.length) out.push(h('div', { class: 'banner warn' }, S.outbox.length + ' waiting to send to the hub.'));
  return out;
}

function joinPage() {
  let code = S.pairCode || '';
  let pin = '';
  let device = S.device || defaultDeviceName();
  return [
    h('h2', {}, 'Join your household'),
    h('p', { class: 'hint' }, S.pairCode
      ? 'Almost there. Type the household PIN and a name for this device.'
      : 'On the hub computer, open ShareKeep > Hub > Pair a device and scan its QR code with this phone, or type the code here.'),
    h('div', { class: 'card' },
      S.pairCode ? h('div', {}, h('div', { class: 'hint' }, 'Pairing code'), h('div', { class: 'code' }, S.pairCode.slice(0, 4) + '-' + S.pairCode.slice(4)))
        : field('Pairing code', '', (v) => { code = v; }, { placeholder: 'ABCD-EFGH', autocapitalize: 'characters' }),
      field('Household PIN', '', (v) => { pin = v; }, { type: 'password', inputmode: 'numeric', placeholder: '6 to 12 digits' }),
      field('Name this device', device, (v) => { device = v; }, { placeholder: "Sam's phone" }),
      h('button', { class: 'btn', onclick: (e) => {
        if (!code.replace(/[-\s]/g, '').match(/^[A-Za-z0-9]{8}$/)) return toast('Type the 8-letter pairing code.');
        if (!pin.match(/^\d{6,12}$/)) return toast('Type the household PIN: 6 to 12 digits.');
        if (!device.trim()) return toast("Name this device, like Sam's phone.");
        e.target.disabled = true;
        pair(code, pin, device.trim()).finally(() => { e.target.disabled = false; });
      } }, 'Pair this device')),
  ];
}

function defaultDeviceName() {
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return 'iPhone';
  if (/iPad/.test(ua)) return 'iPad';
  if (/Android/.test(ua)) return 'Android phone';
  if (/Windows/.test(ua)) return 'Windows PC';
  if (/Mac/.test(ua)) return 'Mac';
  return '';
}

function field(label, value, onInput, attrs) {
  const input = h('input', Object.assign({ value, oninput: (e) => onInput(e.target.value) }, attrs || {}));
  return h('label', { class: 'field' }, label, input);
}

function homePage() {
  const v = S.view;
  if (!v) return [h('p', { class: 'hint' }, S.busy ? 'Loading…' : 'Nothing to show yet.')];
  if (!v.cards.length) {
    return [h('div', { class: 'card' }, h('h2', {}, 'Nothing to show yet'),
      h('p', { class: 'hint' }, 'Your agreements appear here once the hub computer adds you to one.'))];
  }
  return v.cards.map((c) => h('div', {
    class: 'card click ' + (c.sign > 0 ? 'owed' : c.sign < 0 ? 'owes' : ''),
    role: 'button',
    tabindex: '0',
    onclick: () => { S.agreement = c.id; S.page = 'agreement'; S.tab = 'activity'; render(); window.scrollTo(0, 0); },
  },
  h('div', { class: 'row' }, h('h2', { class: 'grow' }, c.name),
    c.waiting ? h('span', { class: 'badge-pill warn' }, c.waiting + ' to approve') : null),
  h('div', { class: 'big ' + signClass(c.sign) }, c.headline),
  c.members.map((m) => h('div', { class: 'kv' }, h('span', { class: 'hint' }, m.name), h('span', { class: 'v ' + signClass(m.sign) }, m.balance))),
  h('p', { class: 'hint' }, 'This period: ' + c.period)));
}

function agreementPage(a) {
  if (S.tab === 'members') {
    return [
      h('div', { class: 'card' }, h('h2', {}, 'Balances'),
        a.members.map((m) => h('div', { class: 'kv' }, h('span', {}, m.name), h('span', { class: 'v ' + signClass(m.sign) }, m.balance))),
        h('p', { class: 'hint' }, 'Positive: the household owes them. Negative: they owe the household.')),
      h('div', { class: 'card' }, h('h2', {}, 'To settle up'),
        a.settle.length ? a.settle.map((s) => h('div', {}, s)) : h('p', { class: 'hint' }, 'Everyone is settled up.')),
    ];
  }
  if (S.tab === 'rules') {
    return [
      h('button', { class: 'btn', onclick: () => ruleSheet(a) }, 'Propose a rule'),
      ...a.rules.map((r) => h('div', { class: 'card' },
        h('div', { class: 'row' }, h('h2', { class: 'grow' }, r.name), h('span', { class: 'badge-pill' }, r.method)),
        h('p', { class: 'hint' }, r.detail))),
      a.rules.length ? null : h('p', { class: 'hint' }, 'No rules yet. Propose one: it takes effect once everyone approves.'),
    ];
  }
  if (S.tab === 'periods') {
    return a.periods.map((p) => h('div', { class: 'card ' + (p.closed ? '' : p.can_close ? 'warn' : 'info') },
      h('h2', {}, p.label), h('p', { class: 'hint' }, p.state),
      p.can_close ? h('button', { class: 'btn', onclick: () => sendNow('period-close', { agreement: a.id, date: p.first }) }, 'Close this period') : null,
      p.closed ? h('button', { class: 'btn secondary', onclick: () => statement(a, p.first) }, 'Save the statement (PDF)') : null));
  }
  return [
    h('div', { class: 'row' },
      h('button', { class: 'btn', onclick: () => expenseSheet(a) }, '+ Expense'),
      h('button', { class: 'btn secondary', onclick: () => paymentSheet(a) }, 'Record payment')),
    h('p', { class: 'hint' }, 'This period: ' + a.period),
    ...a.activity.map((e) => h('div', { class: 'card' + (e.disputed ? ' warn' : '') },
      h('div', { class: 'row' },
        h('div', { class: 'grow' }, h('div', { class: 'v' }, e.title), h('p', { class: 'hint' }, e.date + ' · ' + e.detail + (e.disputed ? ' · disputed' : ''))),
        h('strong', {}, e.amount)),
      e.editable ? h('div', { class: 'row end' },
        h('button', { class: 'link amber', onclick: () => sendNow('dispute', { target: e.seq, resolved: e.disputed }) }, e.disputed ? 'Resolve' : 'Dispute'),
        h('button', { class: 'link red', onclick: () => { if (confirmDelete(e)) sendNow('entry-delete', { target: e.seq }); } }, 'Delete')) : null)),
    a.activity.length ? null : h('p', { class: 'hint' }, 'No expenses yet. Add the first one with + Expense.'),
  ];
}

function confirmDelete(e) {
  // a plain confirm() is a modal the app avoids; a second tap within 4 s confirms instead
  const key = 'del' + e.seq;
  if (S.armed === key) { S.armed = null; return true; }
  S.armed = key;
  toast('Tap Delete again to delete "' + e.title + '".');
  setTimeout(() => { if (S.armed === key) S.armed = null; }, 4000);
  return false;
}

function approvalsPage() {
  const list = S.view ? S.view.approvals : [];
  return [
    h('h2', {}, 'Waiting for you'),
    ...list.map((a) => h('div', { class: 'card warn' },
      h('strong', {}, a.title), h('p', { class: 'hint' }, a.detail),
      h('div', { class: 'row' },
        h('button', { class: 'btn', onclick: () => queue('approval', { proposal: a.seq, approve: true }) }, 'Approve'),
        h('button', { class: 'btn danger', onclick: () => queue('approval', { proposal: a.seq, approve: false }) }, 'Decline')))),
    list.length ? null : h('p', { class: 'hint' }, 'Nothing waits for your approval.'),
  ];
}

function settingsPage() {
  return [
    h('div', { class: 'card' }, h('h2', {}, 'This device'),
      kv('You are', S.view ? S.view.me_name : ''), kv('Book', S.book ? S.book.name : ''), kv('Device', S.device),
      kv('Waiting to send', String(S.outbox.length)),
      h('button', { class: 'btn secondary', onclick: sync }, 'Sync now')),
    S.refused.length ? h('div', { class: 'card owes' }, h('h2', {}, 'Not accepted by the hub'),
      S.refused.map((r) => h('div', {}, r.kind + ': ' + r.reason)),
      h('button', { class: 'link', onclick: () => { S.refused = []; saveQueue(); render(); } }, 'Dismiss')) : null,
    h('div', { class: 'card' }, h('h2', {}, 'Install'),
      h('p', { class: 'hint' }, 'Add ShareKeep to the home screen: on an iPhone, Share > Add to Home Screen; on Android or a PC, the browser\'s Install app menu item.')),
    h('div', { class: 'card' }, h('h2', {}, 'Leave'),
      h('p', { class: 'hint' }, 'Forget the pairing on this device. Anything still waiting to send is lost. The hub computer can also remove the device.'),
      h('button', { class: 'btn danger', onclick: () => { if (S.armed === 'forget') { forget(); } else { S.armed = 'forget'; toast('Tap again to forget this device.'); } } }, 'Forget this device')),
    h('p', { class: 'hint' }, 'ShareKeep, part of the MoneyKeep family. MIT OR Apache-2.0.'),
  ];
}

function kv(k, v) { return h('div', { class: 'kv' }, h('span', { class: 'hint' }, k), h('span', { class: 'v' }, v)); }

async function statement(a, first) {
  try {
    const r = await fetch('/api/v1/statements/' + a.id + '/' + first, { headers: { Authorization: 'Bearer ' + S.token }, cache: 'no-store' });
    if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || 'The statement isn\'t ready.');
    const url = URL.createObjectURL(await r.blob());
    const link = h('a', { href: url, download: a.name + ' statement ' + first + '.pdf' });
    document.body.append(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 30000);
  } catch (e) {
    toast(e.message === 'Failed to fetch' ? "The household hub can't be reached." : e.message);
  }
}

// ----- sheets --------------------------------------------------------------------------------

function openSheet(title, build) {
  const scrim = $('#sheet');
  const sheet = scrim.querySelector('.sheet');
  const close = () => { scrim.hidden = true; sheet.replaceChildren(); };
  scrim.onclick = (e) => { if (e.target === scrim) close(); };
  const body = h('div', { class: 'contents' });
  sheet.replaceChildren(h('div', { class: 'head' }, h('h2', {}, title), h('button', { class: 'link', onclick: close }, 'Close')), body);
  scrim.hidden = false;
  build(body, close);
}

function chipRow(people, chosen, pick) {
  return h('div', { class: 'chips' }, people.map((p) => h('button', { class: 'chip' + (p.id === chosen ? ' on' : ''), onclick: () => pick(p.id) }, p.name)));
}

function expenseSheet(a) {
  const cur = S.view.currency;
  const e = { amount: '', category: '', date: S.view.today, note: '', payer: S.member };
  openSheet('New expense', (body, close) => {
    const preview = h('div', {});
    const err = h('p', { class: 'error' });
    const save = h('button', { class: 'btn', disabled: true }, 'Save');
    let timer = 0;
    const payers = h('div', {});
    const body_ = () => ({
      agreement: a.id, date: e.date, amount: parseAmount(e.amount, cur), payer: { member: e.payer },
      category: e.category.trim(), note: e.note.trim() || undefined,
    });
    const check = () => {
      const b = body_();
      let why = '';
      if (!b.amount) why = 'Type the amount, like 12.50.';
      else if (!b.category) why = 'Type a category, like Groceries.';
      else if (!/^\d{4}-\d{2}-\d{2}$/.test(b.date)) why = 'Pick the date.';
      preview.replaceChildren();
      err.textContent = why;
      save.disabled = true;
      if (why) return;
      clearTimeout(timer);
      timer = setTimeout(async () => {
        try {
          const p = await api('POST', '/api/v1/preview', { expense: b });
          preview.replaceChildren(...p.rows.map((r) => kv(r.name, r.amount)));
          err.textContent = '';
          save.disabled = false;
        } catch (x) {
          // offline: no preview, but it can still wait in the outbox
          err.textContent = x.status === 0 ? 'Offline: the split shows once the hub has it.' : x.message;
          save.disabled = x.status !== 0;
        }
      }, 250);
    };
    const drawPayers = () => payers.replaceChildren(chipRow(a.people, e.payer, (id) => { e.payer = id; drawPayers(); check(); }));
    drawPayers();
    save.onclick = () => { close(); queue('expense', body_()); };
    body.append(
      field('Amount', '', (v) => { e.amount = v; check(); }, { inputmode: 'decimal', placeholder: '0.00' }),
      field('Category', '', (v) => { e.category = v; check(); }, { placeholder: 'Groceries' }),
      field('Date', e.date, (v) => { e.date = v; check(); }, { type: 'date' }),
      field('Note (optional)', '', (v) => { e.note = v; }),
      h('label', { class: 'field' }, 'Paid by'), payers,
      h('div', { class: 'card' }, h('h2', {}, 'How it divides'), preview, err),
      save);
    check();
  });
}

function paymentSheet(a) {
  const cur = S.view.currency;
  const others = a.people.filter((p) => p.id !== S.member);
  const p = { from: S.member, to: a.i_owe ? a.i_owe[0] : (others[0] || {}).id, amount: a.i_owe ? formatPlain(a.i_owe[1], cur) : '' };
  openSheet('Record a payment', (body, close) => {
    const from = h('div', {});
    const to = h('div', {});
    const draw = () => {
      from.replaceChildren(chipRow(a.people, p.from, (id) => { p.from = id; draw(); }));
      to.replaceChildren(chipRow(a.people, p.to, (id) => { p.to = id; draw(); }));
    };
    draw();
    body.append(
      field('Amount', p.amount, (v) => { p.amount = v; }, { inputmode: 'decimal', placeholder: '0.00' }),
      h('label', { class: 'field' }, 'From'), from, h('label', { class: 'field' }, 'To'), to,
      h('button', { class: 'btn', onclick: () => {
        const amount = parseAmount(p.amount, cur);
        if (!amount) return toast('Type the amount, like 25.00.');
        if (!p.to || p.from === p.to) return toast('Pick two different people.');
        close();
        queue('payment', { agreement: a.id, date: S.view.today, from: p.from, to: p.to, amount });
      } }, 'Save'));
  });
}

// The rule editor: the same behaviour as the desktop app's RuleDraft (sk-view).
function ruleSheet(a) {
  const d = {
    name: '', categories: '', method: 0,
    rows: a.people.map((p) => ({ id: p.id, name: p.name, value: 1, included: true, locked: false })),
  };
  const setMethod = (m) => {
    d.method = m;
    const n = Math.max(1, d.rows.filter((r) => r.included).length);
    for (const r of d.rows) { r.locked = false; r.value = m === 1 ? (r.included ? 100 / n : 0) : 1; }
    if (m === 1) roundPercent();
  };
  const roundPercent = () => {
    for (const r of d.rows) r.value = r.included ? Math.max(0, Math.round(r.value)) : 0;
    const total = d.rows.reduce((s, r) => s + r.value, 0);
    let i = -1;
    d.rows.forEach((r, j) => { if (r.included && !r.locked) i = j; });
    if (i < 0) d.rows.forEach((r, j) => { if (r.included) i = j; });
    if (i >= 0) d.rows[i].value = Math.max(0, d.rows[i].value + 100 - total);
  };
  const setValue = (i, v) => {
    if (d.method !== 1) { d.rows[i].value = Math.max(0, Math.round(v)); return; }
    const locked = d.rows.reduce((s, r, j) => s + (j !== i && r.included && r.locked ? r.value : 0), 0);
    v = Math.min(Math.max(v, 0), 100 - locked);
    d.rows[i].value = v;
    const others = d.rows.map((r, j) => j).filter((j) => j !== i && d.rows[j].included && !d.rows[j].locked);
    const left = 100 - locked - v;
    const current = others.reduce((s, j) => s + d.rows[j].value, 0);
    for (const j of others) d.rows[j].value = current > 0 ? d.rows[j].value / current * left : left / others.length;
    if (!others.length) d.rows[i].value = 100 - locked;
    roundPercent();
  };
  const rule = () => {
    const weights = (scale) => Object.fromEntries(d.rows.map((r) => [String(r.id), Math.max(0, Math.round(r.value * scale))]));
    const method = [() => 'Equal', () => ({ Percent: weights(100) }), () => ({ Shares: weights(1) }), () => ({ Income: weights(1) })][d.method]();
    return { name: d.name.trim(), method, members: d.rows.map((r) => r.id), excluded: d.rows.filter((r) => !r.included).map((r) => r.id) };
  };
  setMethod(0);
  openSheet('Propose a rule', (body, close) => {
    const methods = h('div', {});
    const rows = h('div', { class: 'card' });
    const examples = h('div', { class: 'card' });
    const propose = h('button', { class: 'btn', disabled: true }, 'Propose');
    let timer = 0;
    const refreshExamples = () => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        try {
          const x = await api('POST', '/api/v1/examples', { rule: rule() });
          examples.replaceChildren(h('h2', {}, 'Examples'), ...x.examples.map((e) => h('div', {}, h('strong', {}, e.amount), e.rows.map((r) => kv(r.name, r.amount)))));
          propose.disabled = !d.name.trim();
        } catch (e) {
          examples.replaceChildren(h('h2', {}, 'Examples'), h('p', { class: 'error' }, e.message));
          propose.disabled = true;
        }
      }, 150);
    };
    const draw = () => {
      methods.replaceChildren(h('div', { class: 'chips' }, METHODS.map((m, i) => h('button', { class: 'chip' + (d.method === i ? ' on' : ''), onclick: () => { setMethod(i); draw(); } }, m))));
      const pct = d.method === 1;
      rows.replaceChildren(h('h2', {}, pct ? 'Percentages (always total 100%)' : 'Who shares'), ...d.rows.map((r, i) => h('div', { class: 'slider' },
        h('div', { class: 'row' },
          h('label', { class: 'switch grow' }, h('input', { type: 'checkbox', checked: r.included, onchange: () => { r.included = !r.included; if (pct) setMethod(1); draw(); } }), r.name),
          h('span', {}, !r.included ? 'opted out' : pct ? Math.round(r.value) + '%' : d.method === 0 ? '' : Math.round(r.value) + ' share(s)'),
          pct && r.included ? h('button', { class: 'link' + (r.locked ? ' amber' : ''), onclick: () => { r.locked = !r.locked; draw(); } }, r.locked ? 'Locked' : 'Lock') : null),
        r.included && d.method !== 0 ? h('input', {
          type: 'range', min: 0, max: pct ? 100 : 10, step: 1, value: r.value, disabled: r.locked, 'aria-label': r.name,
          oninput: (e) => { setValue(i, Number(e.target.value)); draw(); },
        }) : null)));
      refreshExamples();
    };
    body.append(
      field('Name', '', (v) => { d.name = v; propose.disabled = !v.trim(); }, { placeholder: 'Internet' }),
      field("Categories it's the default for (comma-separated)", '', (v) => { d.categories = v; }, { placeholder: 'Rent, Utilities' }),
      h('label', { class: 'field' }, 'Method'), methods, rows, examples,
      h('p', { class: 'hint' }, 'Everyone else in the agreement approves it before it takes effect. It starts with the next period (' + a.next_period + ').'),
      propose);
    propose.onclick = async () => {
      const categories = d.categories.split(',').map((c) => c.trim()).filter(Boolean);
      const ok = await sendNow('rule-proposal', { agreement: a.id, rule_id: a.next_rule, effective: a.next_period, rule: rule(), categories });
      if (ok) { close(); toast('Proposed. It takes effect once everyone approves.'); }
    };
    draw();
  });
}

// ----- start ---------------------------------------------------------------------------------

function start() {
  readPairCode();
  window.addEventListener('hashchange', () => { readPairCode(); if (S.pairCode && S.token) toast('This device is already paired. Forget it in Settings to pair it again.'); render(); });
  $('#back').addEventListener('click', () => { S.page = 'home'; render(); });
  $('#sync').addEventListener('click', sync);
  for (const b of document.querySelectorAll('#nav button')) {
    b.addEventListener('click', () => { S.page = b.dataset.page; render(); window.scrollTo(0, 0); });
  }
  document.addEventListener('visibilitychange', () => { if (!document.hidden) sync(); });
  window.addEventListener('online', sync);
  setInterval(() => { if (!document.hidden) sync(); }, SYNC_EVERY_MS);
  if ('serviceWorker' in navigator && location.protocol === 'https:') {
    navigator.serviceWorker.register('/sw.js').catch(() => { /* fine without it */ });
  }
  render();
  sync();
}

start();
