// Homestock client glue (~100 lines). HTMX does the rest.

// Steppers: <div data-stepper data-min="1"><button data-step="-1"> <input> <button data-step="1">
document.addEventListener('click', e => {
  const b = e.target.closest('[data-step]'); if (!b) return;
  const wrap = b.closest('[data-stepper]'), input = wrap.querySelector('input');
  const min = +(wrap.dataset.min ?? 0), max = wrap.dataset.max ? +wrap.dataset.max : Infinity;
  input.value = Math.min(max, Math.max(min, (+input.value || 0) + +b.dataset.step));
  input.dispatchEvent(new Event('change', { bubbles: true }));
});

function closeSheet() { document.getElementById('sheet').innerHTML = ''; history.replaceState(null, '', location.pathname + location.search.replace(/[?&]item=\d+/, '')); }
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') closeSheet();
  if (e.key === '/' && !/INPUT|TEXTAREA|SELECT/.test(e.target.tagName)) { e.preventDefault(); location.href = '/search'; }
});

// Errors: htmx 2 skips 4xx/5xx swaps by default; let the server's toast (HX-Retarget: #toast) through.
document.addEventListener('htmx:beforeSwap', e => {
  if (e.detail.xhr.status >= 400 && e.detail.xhr.getResponseHeader('HX-Retarget') === '#toast') {
    e.detail.shouldSwap = true; e.detail.isError = false;
  }
});

// Toasts: show a flash left by a failed form post, and clear any toast after a few seconds.
function showToast(message) {
  const t = Object.assign(document.createElement('div'), { className: 'toast toast-error', textContent: message });
  t.setAttribute('role', 'alert');
  document.getElementById('toast')?.replaceChildren(t);
}
let toastTimer;
function watchToasts() {
  const box = document.getElementById('toast'); if (!box) return;
  new MutationObserver(() => {
    clearTimeout(toastTimer);
    if (box.childElementCount) toastTimer = setTimeout(() => box.replaceChildren(), 4000);
  }).observe(box, { childList: true });
  const flash = document.cookie.split('; ').find(c => c.startsWith('hs_flash='));
  if (flash) {
    document.cookie = 'hs_flash=; Path=/; Max-Age=0; SameSite=Lax';
    try { showToast(decodeURIComponent(flash.slice('hs_flash='.length))); } catch {}
  }
}
if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', watchToasts); else watchToasts();

// Theme
function htmlTheme() { return document.documentElement.dataset.theme; }
function flipTheme() { document.documentElement.dataset.theme = htmlTheme() === 'dark' ? 'light' : 'dark'; }

// Drag & drop (pointer devices ≥640px). Sources: [data-drag-loc], [data-drag-item]. Targets: [data-drop-loc].
let drag = null;
const canDrag = () => matchMedia('(min-width: 640px) and (pointer: fine)').matches;
document.addEventListener('dragstart', e => {
  if (!canDrag()) { e.preventDefault(); return; }
  const s = e.target.closest('[data-drag-loc],[data-drag-item]'); if (!s) return;
  drag = s.dataset.dragLoc ? { kind: 'loc', id: s.dataset.dragLoc } : { kind: 'item', id: s.dataset.dragItem };
  e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', drag.kind + ':' + drag.id);
  s.classList.add('opacity-40');
});
document.addEventListener('dragend', e => { document.querySelectorAll('.opacity-40,.drop-ok').forEach(n => n.classList.remove('opacity-40', 'drop-ok')); drag = null; });
document.addEventListener('dragover', e => {
  const t = e.target.closest('[data-drop-loc]'); if (!t || !drag) return;
  if (drag.kind === 'loc' && t.dataset.dropLoc === drag.id) return;   // server also rejects descendants
  if (drag.kind === 'item' && t.dataset.dropLoc === 'root') return;
  e.preventDefault(); t.classList.add('drop-ok');
});
document.addEventListener('dragleave', e => { const t = e.target.closest('[data-drop-loc]'); if (t) t.classList.remove('drop-ok'); });
document.addEventListener('drop', e => {
  const t = e.target.closest('[data-drop-loc]'); if (!t || !drag) return; e.preventDefault();
  const current = document.getElementById('contents')?.dataset.currentLoc;
  const scope = new URLSearchParams(location.search).get('scope');   // absent → server uses the saved preference
  const url = drag.kind === 'loc' ? `/loc/${drag.id}/move` : `/items/${drag.id}/move`;
  const values = drag.kind === 'loc' ? { parent: t.dataset.dropLoc } : { location_id: t.dataset.dropLoc };
  if (current) values.current = current;
  if (scope) values.scope = scope;
  htmx.ajax('POST', url, { values, target: '#contents', swap: 'innerHTML' }).then(() => htmx.trigger(document.body, 'tree-changed'));
});

// Share shopping list: Web Share → fallback sheet (Email + Copy)
async function shareList() {
  const text = await (await fetch('/shopping/share.txt')).text();
  if (navigator.share) {
    try { await navigator.share({ title: 'Shopping list', text }); return; }
    catch (err) { if (err.name === 'AbortError') return; }
  }
  htmx.ajax('GET', '/shopping/share-sheet', { target: '#sheet' });
}
async function copyText(text) {
  try { await navigator.clipboard.writeText(text); }
  catch { const ta = Object.assign(document.createElement('textarea'), { value: text }); document.body.append(ta); ta.select(); document.execCommand('copy'); ta.remove(); }
}
