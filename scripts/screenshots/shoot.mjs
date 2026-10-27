// Capture the documentation screenshots from a Homestock with the demo household loaded.
// Usage: node shoot.mjs BASE_URL OUT_DIR   (run.sh sets everything up and calls this)
// FIREFOX names the browser binary (default: firefox on PATH).
import puppeteer from 'puppeteer-core';
import { execFileSync } from 'node:child_process';

const [base, out] = process.argv.slice(2);
const firefox = process.env.FIREFOX || execFileSync('sh', ['-c', 'command -v firefox']).toString().trim();
const desktop = [1280, 800];
const phone = [390, 844];
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const browser = await puppeteer.launch({ browser: 'firefox', executablePath: firefox, headless: true });
const page = await browser.newPage();

async function post(path, fields) {
  await page.evaluate(async (p, f) => {
    await fetch(p, { method: 'POST', body: new URLSearchParams(f), redirect: 'manual' });
  }, path, fields);
}

// Follow location links by name from All locations, e.g. ['Garage', 'Steel shelving'].
async function locationUrl(names) {
  let url = '/loc';
  for (const name of names) {
    await page.goto(base + url, { waitUntil: 'networkidle0' });
    url = await page.evaluate((n) => {
      const link = [...document.querySelectorAll('a[href^="/loc/"]')].find((a) => a.textContent.includes(n));
      if (!link) throw new Error(`no location link for ${n}`);
      return link.getAttribute('href');
    }, name);
  }
  return url;
}

async function click(text, selector = 'button, a') {
  await page.evaluate((t, s) => {
    const el = [...document.querySelectorAll(s)].find((e) => e.textContent.includes(t) && e.offsetParent);
    if (!el) throw new Error(`nothing to click containing ${t}`);
    el.click();
  }, text, selector);
  await pause(900);
}

async function shot(name, url, [width, height], then) {
  await page.setViewport({ width, height, deviceScaleFactor: 2 });
  await page.goto(base + url, { waitUntil: 'networkidle0' });
  // Headless Firefox draws classic scrollbars; real phones and most desktops overlay them.
  await page.addStyleTag({ content: '* { scrollbar-width: none !important }' });
  if (then) await then();
  await pause(700);
  await page.screenshot({ path: `${out}/${name}.png` });
  console.log(`  ${name}.png`);
}

await page.goto(base + '/login');
await post('/login', { name: 'Alex', password: 'demo-password-1' });
await post('/me/theme', { theme: 'light' });

const shelving = await locationUrl(['Garage', 'Steel shelving']);
const toolBox = await locationUrl(['Garage', 'Steel shelving', 'Tool box']);

await shot('location', shelving, desktop);
await shot('item', toolBox, desktop, () => click('Cordless drill', '[hx-get^="/items/"]'));
await shot('search', '/search?q=batteris', desktop); // misspelt on purpose: search is typo-tolerant
await shot('in-use', '/in-use', desktop);
await shot('mobile-quick-add', toolBox, phone, () => click('Add item'));

await post('/me/theme', { theme: 'dark' });
await shot('shopping-dark', '/shopping', desktop);
await shot('mobile-shopping-dark', '/shopping', phone);

await browser.close();
