// ShareKeep's start-up guard, loaded before app.js. If the app can't start on this device (a
// script error, data it can't read, or app.js not running at all), the screen says what went
// wrong and offers Try again and Start over, instead of staying empty under the title.
'use strict';
(function () {
  let shown = false;

  function startOver() {
    // forget this device's pairing and saved data, the kept files and the service worker
    try {
      for (let i = localStorage.length - 1; i >= 0; i--) {
        const k = localStorage.key(i);
        if (k && k.indexOf('sk.') === 0) localStorage.removeItem(k);
      }
    } catch (e) { /* private mode */ }
    const done = [];
    if (window.caches) done.push(caches.keys().then((ks) => Promise.all(ks.map((k) => caches.delete(k)))));
    if (navigator.serviceWorker) {
      done.push(navigator.serviceWorker.getRegistrations().then((rs) => Promise.all(rs.map((r) => r.unregister()))));
    }
    Promise.all(done).catch(() => {}).then(() => { location.href = '/'; });
  }

  function show(why) {
    if (shown) return;
    shown = true;
    const page = document.getElementById('page');
    if (!page) return;
    const card = document.createElement('div');
    card.className = 'card owes';
    const t = document.createElement('h2');
    t.textContent = "ShareKeep couldn't start on this device";
    const p = document.createElement('p');
    p.className = 'hint';
    p.textContent = 'What went wrong: ' + why;
    const p2 = document.createElement('p');
    p2.className = 'hint';
    p2.textContent = 'Try again first. If that doesn’t help, Start over forgets this device’s pairing and saved copy (nothing in the household’s book is lost), and the device pairs again with a new code from the hub computer.';
    const again = document.createElement('button');
    again.className = 'btn';
    again.textContent = 'Try again';
    again.addEventListener('click', () => location.reload());
    let armed = false;
    const over = document.createElement('button');
    over.className = 'btn danger';
    over.textContent = 'Start over on this device';
    over.addEventListener('click', () => {
      if (!armed) { armed = true; over.textContent = 'Tap again to start over'; return; }
      startOver();
    });
    const help = document.createElement('a');
    help.className = 'link';
    help.href = '/help#blank';
    help.textContent = 'Help: what else to try';
    card.append(t, p, p2, again, over, help);
    page.replaceChildren(card);
  }

  window.skStartFailed = show;
  window.addEventListener('error', (e) => {
    if (window.skStarted) return;
    const where = e.filename ? ' (' + e.filename.split('/').pop() + ', line ' + e.lineno + ')' : '';
    show((e.message || 'a script error') + where);
  });
  window.addEventListener('load', () => {
    setTimeout(() => {
      if (!window.skStarted) show('the app’s script didn’t run (it may not have loaded).');
    }, 4000);
  });
})();
