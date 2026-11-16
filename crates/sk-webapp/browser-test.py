#!/usr/bin/env python3
"""The web app end to end in headless Chrome against a real hub on this computer.

    cargo build -p sk-hub
    python3 crates/sk-webapp/browser-test.py [chrome path]

Needs Python's playwright package and Chrome. Starts a hub in a temporary folder on
127.0.0.1 (ports 15443/15080/15081 must be free), sets up a household through the admin port,
pairs from the QR code's link, adds an expense (with its live split), proposes a percentage rule
with the sliders, records a payment offline and sends it when back online. Screenshots go to
./webshots. Not part of `cargo xtask ci` (CI has no browser).
"""
import json, os, subprocess, sys, tempfile, time, urllib.request, datetime
from playwright.sync_api import sync_playwright

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HUB = os.path.join(ROOT, 'target', 'debug', 'sharekeep-hub' + ('.exe' if os.name == 'nt' else ''))
CHROME = sys.argv[1] if len(sys.argv) > 1 else '/usr/bin/google-chrome'
out = 'webshots'
os.makedirs(out, exist_ok=True)
data = tempfile.mkdtemp()
subprocess.run([HUB, 'setup', '--data', data, '--address', '127.0.0.1'], check=True, capture_output=True)
hub = subprocess.Popen([HUB, 'run', '--data', data], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)
token = open(os.path.join(data, 'admin.token')).read().strip()

def admin(method, path, body=None):
    req = urllib.request.Request('http://127.0.0.1:15081/admin/v1/' + path, method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    return json.load(urllib.request.urlopen(req))

try:
    admin('POST', 'pin', {'pin': '482916'})
    book = admin('POST', 'books', {'name': 'Home', 'hyperbook': True})['book']['id']
    first = datetime.date.today().replace(day=1).isoformat()
    n = 0
    def rec(kind, body, member=None):
        global n
        n += 1
        admin('POST', f'books/{book}/records', {'client_id': f'a{n}', 'kind': kind, 'body': body, 'member': member})
    for i, name in [(1, 'Jerry'), (2, 'Sam'), (3, 'Alex')]:
        rec('member-add', {'id': i, 'name': name})
    rec('agreement-add', {'id': 1, 'name': 'Apartment', 'members': {'1': first, '2': first, '3': first},
        'schedule': {'Monthly': {'start': first, 'day': {'Day': 1}, 'every': 1}}})
    admin('POST', f'books/{book}/settings', {'key': 'one_keeper', 'value': 1})
    rec('rule-proposal', {'agreement': 1, 'rule_id': 1, 'effective': first, 'categories': ['Groceries'],
        'rule': {'name': 'Shared', 'method': 'Equal', 'members': [1, 2, 3]}}, member=1)
    rec('rule-proposal', {'agreement': 1, 'rule_id': 2, 'effective': first, 'categories': ['Heating'],
        'rule': {'name': 'Heating', 'method': 'Days', 'members': [1, 2, 3]}}, member=1)
    code = admin('POST', 'pairing', {'book_id': book, 'member': 2})
    errors = []
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=CHROME, args=['--ignore-certificate-errors'])
        ctx = b.new_context(viewport={'width': 400, 'height': 820}, device_scale_factor=2, ignore_https_errors=True)
        pg = ctx.new_page()
        pg.on('console', lambda m: errors.append(m.text) if m.type in ('error', 'warning') else None)
        pg.on('pageerror', lambda e: errors.append(str(e)))
        # the QR code's certificate page
        pg.goto(code['qr'])
        assert 'Step 2' in pg.content()
        pg.screenshot(path=f'{out}/00-cert-page.png', full_page=True)
        pg.goto(code['url'])
        # EZ Start: the code came with the link, so step 2 (the PIN) is first on screen
        pg.wait_for_selector('text=Step 2 of 3')
        pg.screenshot(path=f'{out}/01-pair.png')
        pg.fill('input[type=password]', '12')
        pg.click('text=Next')
        pg.wait_for_selector('text=6 to 12 digits.')
        pg.fill('input[type=password]', '482916')
        pg.click('text=Next')
        pg.wait_for_selector('text=Step 3 of 3')
        pg.fill('label:has-text("Device name") input', "Sam's phone")
        pg.click('text=Join the household')
        pg.wait_for_selector('text=Apartment')
        pg.screenshot(path=f'{out}/02-home.png')
        pg.click('h2:has-text("Apartment")')
        pg.click('text=+ Expense')
        pg.fill('label:has-text("Amount") input', '83.47')
        pg.fill('label:has-text("Category") input', 'Groceries')
        pg.wait_for_selector('.sheet .kv')
        pg.screenshot(path=f'{out}/03-expense.png')
        pg.click('.sheet button.btn:has-text("Save")')
        pg.wait_for_selector('text=Groceries')
        time.sleep(0.5)
        pg.screenshot(path=f'{out}/04-activity.png')
        pg.click('#tabs >> text=Rules')
        pg.click('text=Propose a rule')
        pg.fill('label:has-text("Name") input', 'Internet')
        pg.click('.sheet button.chip:has-text("Percentages")')
        sl = pg.locator('input[type=range]').first
        sl.fill('60')
        time.sleep(0.6)
        vals = pg.locator('.sheet .slider .row > span').all_inner_texts()
        print('percent rows', vals)
        pg.screenshot(path=f'{out}/05-rule.png', full_page=False)
        pg.click('.sheet button.btn:has-text("Propose")')
        time.sleep(1)
        pg.click('#tabs >> text=Members')
        pg.screenshot(path=f'{out}/06-members.png')
        pg.click('#tabs >> text=Periods')
        pg.screenshot(path=f'{out}/07-periods.png')
        pg.click('#nav >> text=Approvals')
        pg.screenshot(path=f'{out}/08-approvals.png')
        pg.click('#nav >> text=Settings')
        pg.screenshot(path=f'{out}/09-settings.png')
        # a recurring bill: added under Rules, due today, recorded from Activity
        pg.click('#nav >> text=Home')
        pg.click('h2:has-text("Apartment")')
        pg.click('#tabs >> text=Rules')
        pg.click('text=+ Recurring bill')
        pg.fill('.sheet label:has-text("Name") input', 'Internet')
        pg.fill('.sheet label:has-text("About how much") input', '69.99')
        pg.click('.sheet button.btn:has-text("Add")')
        pg.wait_for_selector('#page >> text=Internet')
        pg.click('#tabs >> text=Activity')
        pg.wait_for_selector('text=Due')
        pg.screenshot(path=f'{out}/12-due.png')
        pg.click('.card.warn button.link:has-text("Record")')
        pg.wait_for_selector('.sheet >> text=Record Internet')
        pg.wait_for_selector('.sheet .kv')
        pg.click('.sheet button.btn:has-text("Save")')
        time.sleep(1.0)
        assert pg.locator('#page h2:has-text("Due")').count() == 0, 'recorded: nothing due'
        # a deposit: made, then Sam puts his part in; then a leave proposed
        pg.click('#tabs >> text=Members')
        pg.click('text=+ Deposit or pot')
        pg.fill('.sheet label:has-text("Name") input', 'Rental security')
        pg.click('.sheet button.btn:has-text("Add")')
        pg.wait_for_selector('#page >> text=Rental security')
        pg.click('#page button.link:has-text("Put in")')
        pg.fill('.sheet label:has-text("Amount") input', '500.00')
        pg.click('.sheet button.btn:has-text("Save")')
        pg.wait_for_selector('#page >> text=$500.00')
        pg.click('text=Someone leaves')
        pg.click('.sheet button.btn:has-text("Propose")')
        time.sleep(1.0)
        pg.screenshot(path=f'{out}/13-members.png', full_page=True)
        # a held deposit paid back
        pg.click('#page button.link:has-text("More")')
        pg.fill('.sheet label:has-text("Amount") input', '100.00')
        pg.click('.sheet button.btn:has-text("Save")')
        pg.wait_for_selector('#page >> text=$400.00')
        # money kept back from Sam's part
        pg.click('#page button.link:has-text("More")')
        pg.click('.sheet button.chip:has-text("Kept back")')
        pg.fill('.sheet label:has-text("Amount") input', '50.00')
        pg.click('.sheet button.btn:has-text("Save")')
        pg.wait_for_selector('#page >> text=$350.00')
        # a bill divided by days present asks for each person's days
        pg.click('#tabs >> text=Activity')
        pg.click('text=+ Expense')
        pg.fill('.sheet label:has-text("Amount") input', '90.00')
        pg.fill('.sheet label:has-text("Category") input', 'Heating')
        pg.wait_for_selector('.sheet >> text=days each person was home')
        for name, days in [('Jerry', '30'), ('Sam', '30'), ('Alex', '30')]:
            pg.fill(f'.sheet .card label:has-text("{name}") input', days)
        pg.wait_for_selector('.sheet .kv >> text=$30.00')
        pg.screenshot(path=f'{out}/15-days.png')
        pg.click('.sheet button.btn:has-text("Save")')
        pg.wait_for_selector('#page >> text=Heating')
        pg.click('#tabs >> text=Members')
        # a special event and a schedule change
        pg.click('#tabs >> text=Periods')
        pg.click('text=+ Special event')
        pg.fill('.sheet label:has-text("Name") input', 'Beach weekend')
        pg.click('.sheet button.btn:has-text("Add")')
        pg.wait_for_selector('#page >> text=Beach weekend')
        pg.click('text=Change how often')
        pg.click('.sheet button.chip:has-text("Every 2 weeks")')
        pg.click('.sheet button.btn:has-text("Save")')
        time.sleep(1.0)
        pg.screenshot(path=f'{out}/14-periods.png', full_page=True)
        # an expense edited (in the event)
        pg.click('#tabs >> text=Activity')
        pg.click('#page .card button.link:has-text("Edit")')
        pg.wait_for_selector('.sheet >> text=Edit expense')
        pg.fill('.sheet label:has-text("Amount") input', '90.00')
        pg.click('.sheet button.chip:has-text("Beach weekend")')
        pg.wait_for_selector('.sheet .kv')
        pg.click('.sheet button.btn:has-text("Save")')
        # the edited entry now says it's part of the event
        pg.wait_for_selector('#page .card >> text=Beach weekend')
        # the wide PC browser
        pg.set_viewport_size({'width': 1200, 'height': 800})
        pg.click('#nav >> text=Home')
        pg.screenshot(path=f'{out}/10-pc.png')
        # offline: a payment waits in the outbox
        ctx.set_offline(True)
        pg.click('h2:has-text("Apartment")')
        pg.click('text=Record payment')
        pg.fill('.sheet label:has-text("Amount") input', '10.00')
        pg.click('.sheet button.btn:has-text("Save")')
        pg.wait_for_selector('text=waiting to send')
        pg.screenshot(path=f'{out}/11-offline.png')
        ctx.set_offline(False)
        pg.click('#sync')
        pg.wait_for_selector('text=Payment')
        time.sleep(0.5)
        assert pg.locator('text=waiting to send').count() == 0
        sw = pg.evaluate('navigator.serviceWorker.getRegistrations().then(r => r.length)')
        print('service workers', sw)
        b.close()
    unexpected = [e for e in errors if 'ERR_INTERNET_DISCONNECTED' not in e]
    print('errors', unexpected)
    assert not unexpected
finally:
    hub.terminate()
