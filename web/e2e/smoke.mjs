// Browser smoke test: walks through the main features in a real Chromium
// against a fresh RustCloud server (empty database).
//
// Run (the Playwright image ships the browsers):
//   docker run --rm --network host -v "$PWD/web/e2e:/e2e" -w /e2e \
//     mcr.microsoft.com/playwright:v1.64.1-noble \
//     sh -c 'npm i --no-save playwright@1.64.1 >/dev/null && BASE_URL=http://localhost:8080 node smoke.mjs'

import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { chromium } from 'playwright';

const base = process.env.BASE_URL ?? 'http://localhost:8080';
const browser = await chromium.launch();
const errors = [];

function watch(page) {
  page.on('pageerror', (error) => errors.push(`page error: ${error.message}`));
  page.on('console', (message) => {
    // A failed request (e.g. 401 from /auth/me before login) is expected; script errors are not.
    if (message.type() === 'error' && !message.text().startsWith('Failed to load resource')) {
      errors.push(`console: ${message.text()}`);
    }
  });
  return page;
}

async function step(name, action) {
  process.stdout.write(`- ${name} … `);
  await action();
  console.log('ok');
}

/** With SHOTS=<dir>, saves a screenshot for looking at the UI. */
async function shot(page, name) {
  if (process.env.SHOTS) await page.screenshot({ path: `${process.env.SHOTS}/${name}.png` });
}

const context = await browser.newContext({ locale: 'en-US' });
const page = watch(await context.newPage());
const row = (name) => page.locator('tr', { hasText: name });
const menu = async (name, item) => {
  await row(name).locator('summary').click();
  await row(name).getByRole('button', { name: item }).click();
};

try {
  await step('first visit offers setup', async () => {
    await page.goto(base);
    await page.getByText('Welcome to RustCloud').waitFor();
    await shot(page, '1-setup');
  });

  await step('create the admin account', async () => {
    await page.getByLabel('Username').fill('admin');
    await page.getByLabel('Password').fill('password123');
    await page.getByRole('button', { name: 'Create account' }).click();
    await page.getByText('This folder is empty').waitFor();
  });

  await step('create a folder', async () => {
    await page.getByRole('button', { name: 'New folder' }).click();
    await page.getByLabel('Folder name').fill('Docs');
    await page.getByRole('button', { name: 'Create', exact: true }).click();
    await row('Docs').waitFor();
  });

  await step('upload a file', async () => {
    await page.locator('input[type=file][multiple]').setInputFiles({
      name: 'hello.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('hello world'),
    });
    await row('hello.txt').waitFor();
    await page.getByText('Done').waitFor();
  });

  await step('rename it', async () => {
    await menu('hello.txt', 'Rename');
    await page.getByRole('textbox', { name: 'Name' }).fill('greeting.txt');
    await page.getByRole('button', { name: 'Save' }).click();
    await row('greeting.txt').waitFor();
  });

  await step('move it into the folder', async () => {
    await menu('greeting.txt', 'Move');
    await page.locator('dialog').getByRole('button', { name: 'Docs' }).click();
    await page.getByRole('button', { name: 'Choose this folder' }).click();
    await row('greeting.txt').waitFor({ state: 'detached' });
    await page.getByRole('button', { name: 'Docs' }).click();
    await row('greeting.txt').waitFor();
    await row('greeting.txt').locator('summary').click();
    await shot(page, '2-folder-menu');
    await page.getByRole('columnheader', { name: 'Name' }).click(); // outside the menu: it closes
    await row('greeting.txt').getByRole('button', { name: 'Rename' }).waitFor({ state: 'hidden' });
  });

  await step('preview a text file', async () => {
    await row('greeting.txt').getByRole('button', { name: 'greeting.txt' }).click();
    const preview = page.locator('dialog.preview');
    await preview.locator('pre', { hasText: 'hello world' }).waitFor();
    await shot(page, '2b-preview');
    await page.keyboard.press('Escape');
    await preview.waitFor({ state: 'hidden' });
  });

  let link = '';
  await step('create a public link', async () => {
    await menu('greeting.txt', 'Share');
    await page.getByRole('button', { name: 'Create link' }).click();
    const field = page.locator('dialog input[readonly]');
    await field.waitFor();
    link = await field.inputValue();
    assert.match(link, /#\/s\/[\w-]+$/);
    await shot(page, '3-share-dialog');
    await page.locator('dialog').getByRole('button', { name: 'Close' }).click();
  });

  await step('open the link without logging in', async () => {
    const visitor = watch(await (await browser.newContext({ locale: 'en-US' })).newPage());
    await visitor.goto(link);
    await visitor.getByText('Shared with you').waitFor();
    await visitor.getByRole('heading', { name: 'greeting.txt' }).waitFor();
    await visitor.locator('pre', { hasText: 'hello world' }).waitFor();
    await shot(visitor, '4-public');
    const href = await visitor.getByRole('link', { name: 'Download file' }).getAttribute('href');
    const response = await visitor.request.get(new URL(href, base).toString());
    assert.equal(await response.text(), 'hello world');
  });

  await step('search finds the file', async () => {
    await page.getByPlaceholder('Search files…').fill('greet');
    await page.keyboard.press('Enter');
    await page.getByText('Search: “greet”').waitFor();
    await row('greeting.txt').waitFor();
  });

  await step('links, settings and users pages open', async () => {
    await page.getByRole('link', { name: 'Public links' }).click();
    await page.getByText('greeting.txt').waitFor();
    await page.locator('summary', { hasText: 'admin' }).click();
    await page.getByRole('link', { name: 'Settings' }).click();
    await page.getByText('of', { exact: false }).or(page.getByText('no limit')).first().waitFor();
    await page.getByText('No app has access to your files.').waitFor();
    await page.getByRole('link', { name: 'Administration' }).click();
    await page.getByRole('heading', { name: 'New user' }).waitFor();
    await shot(page, '5-users');
  });

  await step('administration changes server settings', async () => {
    await page.getByRole('link', { name: 'Accounts' }).click();
    const quota = page.getByLabel('Storage quota for new accounts');
    await quota.fill('100');
    await page.getByRole('button', { name: 'Save' }).click();
    await page.getByText('Saved').waitFor();
    await shot(page, '6-settings');
    await page.getByRole('link', { name: 'Files' }).first().click();
    await page.getByRole('link', { name: 'Administration' }).click();
    await page.getByRole('link', { name: 'Accounts' }).click();
    assert.equal(await page.getByLabel('Storage quota for new accounts').inputValue(), '100', 'the value was stored');
  });

  await step('plugins: disable one, it leaves the side panel, enable it again', async () => {
    await page.getByRole('link', { name: 'Plugins' }).click();
    const notes = page.locator('li', { hasText: 'comes with the server' }).filter({ hasText: 'Notes' });
    await notes.getByText('Running').waitFor();
    await shot(page, '6b-plugins');
    await notes.getByRole('button', { name: 'Disable' }).click();
    await notes.getByText('Disabled').waitFor();
    await page.locator('nav.sidebar').getByRole('link', { name: 'Notes' }).waitFor({ state: 'detached' });
    await notes.getByRole('button', { name: 'Enable' }).click();
    await notes.getByText('Running').waitFor();
    await page.locator('nav.sidebar').getByRole('link', { name: 'Notes' }).waitFor();

    // Something that isn't a plugin archive is refused with the reason.
    await page.locator('input[type=file][accept^=".zip"]').setInputFiles({
      name: 'plugin.zip',
      mimeType: 'application/zip',
      buffer: Buffer.from('not a zip'),
    });
    await page.getByText('not a zip archive', { exact: false }).waitFor();
  });

  await step('storage shows its size and measures its speed', async () => {
    await page.locator('.tabs').getByRole('link', { name: 'Files' }).click();
    await page.getByText('Local disk').waitFor();
    await page.getByText(/write [\d.]+ MiB\/s, read [\d.]+ MiB\/s/).waitFor({ timeout: 60_000 });
    await shot(page, '8-storage');
  });

  await step('the journal shows what happened', async () => {
    await page.locator('.tabs').getByRole('link', { name: 'Journal' }).click();
    await page.getByRole('cell', { name: 'Uploaded “hello.txt”' }).waitFor();
    await page.getByRole('cell', { name: 'Logged in' }).first().waitFor();
    await shot(page, '9-journal');
  });

  await step('hidden files show only when turned on', async () => {
    const filesApp = page.locator('nav.sidebar').getByRole('link', { name: 'Files' });
    await filesApp.click();
    await page.getByRole('button', { name: 'New folder' }).click();
    await page.getByLabel('Folder name').fill('.secret');
    await page.getByRole('button', { name: 'Create', exact: true }).click();
    await row('Docs').waitFor();
    if (await row('.secret').count()) throw new Error('.secret is shown while hidden files are off');

    await page.locator('summary', { hasText: 'admin' }).click();
    await page.getByRole('link', { name: 'Settings' }).click();
    await page.getByLabel('Show hidden files').check();
    await filesApp.click();
    await row('.secret').waitFor();
    await page.locator('tr.hidden-file', { hasText: '.secret' }).waitFor();
    page.once('dialog', (dialog) => dialog.accept());
    await menu('.secret', 'Delete');
    await row('.secret').waitFor({ state: 'detached' });
    // Hidden files off again: apps' .appdata would show in the root otherwise.
    await page.locator('summary', { hasText: 'admin' }).click();
    await page.getByRole('link', { name: 'Settings' }).click();
    await page.getByLabel('Show hidden files').uncheck();
  });

  await step('notes: choose a folder, write a note, find it in the files', async () => {
    await page.locator('nav.sidebar').getByRole('link', { name: 'Notes' }).click();
    const notes = page.frameLocator('iframe[title="Notes"]');
    await notes.getByRole('button', { name: 'Choose a folder' }).click();
    const dialog = page.getByRole('dialog', { name: '“Notes” asks for a folder' });
    await dialog.getByRole('button', { name: 'Allow' }).click();

    await notes.getByRole('button', { name: 'New', exact: true }).click();
    await notes.locator('textarea').fill('# Shopping\n- milk\n- bread');
    await notes.getByText('Saved', { exact: true }).waitFor();
    await notes.locator('.item .title', { hasText: 'Shopping' }).waitFor();
    await notes.getByRole('button', { name: 'Preview' }).click();
    await notes.locator('.preview h1', { hasText: 'Shopping' }).waitFor();
    await shot(page, '12-notes');

    // A second note, deleted after a question asked inside the page.
    await notes.getByRole('button', { name: 'New', exact: true }).click();
    await notes.locator('textarea').fill('# Scratch');
    await notes.getByText('Saved', { exact: true }).waitFor();
    await notes.locator('.toolbar').getByRole('button', { name: 'Delete' }).click();
    await notes.locator('dialog').getByRole('button', { name: 'Delete' }).click();
    await notes.locator('.item .title', { hasText: 'Scratch' }).waitFor({ state: 'detached' });

    await page.locator('nav.sidebar').getByRole('link', { name: 'Files' }).click();
    await row('Notes').getByRole('button', { name: 'Notes' }).click();
    await row('New note.md').waitFor();
    // Leave the root as the next steps expect it.
    await page.getByRole('link', { name: 'Home' }).click();
    page.once('dialog', (d) => d.accept());
    await menu('Notes', 'Delete');
    await row('Notes').waitFor({ state: 'detached' });
  });

  await step('calendar: add an event, open it, delete it', async () => {
    await page.locator('nav.sidebar').getByRole('link', { name: 'Calendar' }).click();
    const calendar = page.frameLocator('iframe[title="Calendar"]');
    await calendar.getByRole('button', { name: 'New event' }).click();
    await calendar.getByLabel('Title').fill('Dentist');
    await calendar.getByRole('button', { name: 'Save' }).click();
    const event = calendar.locator('.event', { hasText: 'Dentist' });
    await event.waitFor();
    await shot(page, '13-calendar');

    await event.click();
    await calendar.getByRole('button', { name: 'Delete' }).click();
    await calendar.locator('dialog.ask').getByRole('button', { name: 'Delete' }).click();
    await event.waitFor({ state: 'detached' });

    await calendar.getByRole('button', { name: 'Connect a device' }).click();
    await calendar.getByText('/app/calendar/dav/').waitFor();
    await shot(page, '14-calendar-connect');
    await calendar.getByRole('button', { name: 'Close' }).click();
  });

  await step('calendar: share it with another user, who sees it', async () => {
    const created = await page.evaluate(async () => {
      const response = await fetch('/api/v1/admin/users', {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'X-Requested-With': 'rustcloud' },
        body: JSON.stringify({ username: 'bob', password: 'password123', display_name: 'Bob', is_admin: false }),
      });
      return response.status;
    });
    assert.equal(created, 201);
    const calendar = page.frameLocator('iframe[title="Calendar"]');
    await calendar.locator('.calendar').getByRole('button', { name: 'Calendar' }).click();
    await calendar.getByRole('button', { name: 'Share…' }).click();
    const dialog = page.getByRole('dialog', { name: 'Share “Calendar”' });
    await dialog.getByText('Not shared with anyone yet.').waitFor();
    await dialog.getByPlaceholder('Username').fill('bob');
    await dialog.getByRole('button', { name: 'Share' }).click();
    await dialog.getByText('Bob', { exact: true }).waitFor();
    await shot(page, '15-calendar-share');
    await dialog.getByRole('button', { name: 'Close' }).click();
    await calendar.getByRole('button', { name: 'Cancel' }).click();

    const other = await browser.newContext({ locale: 'en-US' });
    const bob = watch(await other.newPage());
    await bob.goto(base);
    await bob.getByLabel('Username').fill('bob');
    await bob.getByLabel('Password').fill('password123');
    await bob.getByRole('button', { name: 'Log in' }).click();
    await bob.locator('nav.sidebar').getByRole('link', { name: 'Calendar' }).click();
    const bobs = bob.frameLocator('iframe[title="Calendar"]');
    await bobs.getByText('shared by admin').waitFor();
    await shot(bob, '16-calendar-shared');
    await other.close();
  });

  await step('app passwords: create one for the calendar and revoke it', async () => {
    await page.locator('summary', { hasText: 'admin' }).click();
    await page.getByRole('link', { name: 'Settings' }).click();
    const card = page.locator('section', { has: page.getByRole('heading', { name: 'App passwords' }) });
    await card.getByLabel('Name').fill('Phone');
    await card.locator('label', { hasText: 'Calendar' }).locator('select').selectOption('write');
    await card.getByRole('button', { name: 'Create' }).click();
    await card.getByText('Copy the password now').waitFor();
    await card.getByText('Calendar: read and change').waitFor();
    await shot(page, '17-app-passwords');
    await card.getByRole('button', { name: 'Done' }).click();
    await card.getByRole('button', { name: 'Revoke' }).click();
    await card.getByText('Calendar: read and change').waitFor({ state: 'detached' });

    const shared = page.locator('section', { has: page.getByRole('heading', { name: 'Shared app data' }) });
    await shared.getByText('“Calendar” with Bob').waitFor();
    await shared.getByRole('button', { name: 'Revoke' }).click();
    await shared.getByText('Nothing shared yet.', { exact: false }).waitFor();
  });

  // Needs the echo test app installed (crates/wasm-host/tests/apps/echo, with a
  // ui entry `/app/echo`); run with ECHO_APP=1.
  if (process.env.ECHO_APP === '1') {
    await step('a WASM app opens in the panel and talks through the bridge', async () => {
      await page.locator('nav.sidebar').getByRole('link', { name: 'Echo' }).click();
      const frame = page.frameLocator('iframe[title="Echo"]');
      await frame.getByText('Hello, admin! en api:blocked').waitFor();
      await shot(page, '10-app');

      await frame.getByRole('button', { name: 'Ask for a folder' }).click();
      const dialog = page.getByRole('dialog', { name: '“Echo” asks for a folder' });
      await dialog.getByText('Create the folder “Echo”').waitFor();
      await shot(page, '11-access');
      await dialog.getByRole('button', { name: 'Allow' }).click();
      await frame.getByText('got Echo, granted: Echo').waitFor();

      // The folder the app got; leave the root as the next steps expect it.
      await page.locator('nav.sidebar').getByRole('link', { name: 'Files' }).click();
      page.once('dialog', (dialog) => dialog.accept());
      await menu('Echo', 'Delete');
      await row('Echo').waitFor({ state: 'detached' });
    });
  }

  await step('the side panel collapses to icons', async () => {
    await page.getByRole('button', { name: 'Collapse' }).click();
    await page.locator('.sidebar.collapsed').waitFor();
    await page.getByTitle('Expand').click();
  });

  await step('upload a folder with subfolders', async () => {
    await page.getByRole('link', { name: 'Files' }).first().click();
    const dir = join(mkdtempSync(join(tmpdir(), 'rc-')), 'trip');
    mkdirSync(join(dir, 'day1'), { recursive: true });
    writeFileSync(join(dir, 'notes.txt'), 'packing list');
    writeFileSync(join(dir, 'day1', 'photo.txt'), 'sunrise');
    await page.getByTestId('folder-input').setInputFiles(dir);
    await row('trip').waitFor();
    await page.getByRole('button', { name: 'trip' }).click();
    await row('notes.txt').waitFor();
    await row('day1').waitFor();
    await page.getByRole('link', { name: 'Home' }).click();
  });

  await step('drag a folder onto another and move it', async () => {
    await row('trip').dragTo(row('Docs'));
    await shot(page, '7-drop-menu');
    await page.getByRole('menuitem', { name: 'Move' }).click();
    await row('trip').waitFor({ state: 'detached' });
    await page.getByRole('button', { name: 'Docs' }).click();
    await row('trip').waitFor();
    await page.getByRole('link', { name: 'Home' }).click();
  });

  await step('delete the folder', async () => {
    await page.getByRole('link', { name: 'Files' }).first().click();
    page.once('dialog', (dialog) => dialog.accept());
    await menu('Docs', 'Delete');
    await page.getByText('This folder is empty').waitFor();
  });

  await step('log out', async () => {
    await page.locator('summary', { hasText: 'admin' }).click();
    await page.getByRole('button', { name: 'Log out' }).click();
    await page.getByRole('button', { name: 'Log in' }).waitFor();
  });

  assert.deepEqual(errors, [], 'no errors in the browser console');
  console.log('\nAll good.');
} catch (error) {
  await page.screenshot({ path: 'failure.png', fullPage: true }).catch(() => {});
  console.log('FAILED');
  console.error(error);
  if (errors.length) console.error('Browser errors:', errors);
  process.exitCode = 1;
} finally {
  await browser.close();
}
