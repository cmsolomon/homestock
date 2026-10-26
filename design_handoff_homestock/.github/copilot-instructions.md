# Copilot instructions — Homestock

Self-hosted home inventory for a 2–6 person household, served over HTTPS on the LAN. The full spec is in `design_handoff_homestock/README.md`. The HTML files in `design/` are visual and behavioural references; do not ship them.

## Stack (decided)
Rust stable · axum 0.8 + tokio (current_thread) · rusqlite (bundled, backup) with SQLite WAL · askama templates · HTMX (vendored) + small vanilla JS · Tailwind v4 via standalone CLI · argon2 sessions · Docker image in a Proxmox LXC behind Caddy (tls internal).
Architecture, routes, backups and Docker deployment: `IMPLEMENTATION.md`. The active Rust application is in the repository's `src/`; `starter/` is reference material.

## Offline
The app must work with no internet connection. Never add CDN links, web fonts, remote scripts or external API calls. Tailwind runs only at build time (standalone CLI → `static/app.css`). HTMX is vendored in `static/`.

## UX
The UX in `design_handoff_homestock/README.md` and the prototype is final. Don't add fields or screens it doesn't have (e.g. there is no item description field; use notes).

## Rust conventions
- All DB access goes through `Db::call(|conn| …)` (spawn_blocking). Multi-statement writes use a transaction.
- Call `repo::items::reindex(&tx, &ids)` after any write touching item name/notes/tags/location, or after moving a box (whole subtree).
- Handlers return `Result<Html<String>, AppError>`; render with askama. With `HX-Request` return the partial; without it return the full page.
- Errors become toasts via `HX-Retarget: #toast`. Auth failures send `HX-Redirect: /login`.
- Count updates (tabs, sidebar badges) are sent with `hx-swap-oob`.
- No `unwrap()` in handlers. No ORM. Write SQL by hand with `prepare_cached`.
- CPU-heavy work (image resize, CSV, backup) runs in `spawn_blocking`.

## Must-follow rules
- Organise everything by location. `location.parent_id` gives unlimited nesting. Moving a box moves its subtree; reject cycles server-side (409).
- An item owns `qty`. Check-outs are claims: `available = qty − Σ checkout.qty`. Never duplicate items for a check-out.
- Partial moves split the record; moving into a location with a same-name item (case-insensitive) merges into it. Checked-out units cannot be moved.
- `low = qty > 0 && min_qty > 0 && available <= min_qty`. `used_up = qty === 0`, and the record is kept.
- Quick add locks the current location. Enter saves, resets the name/qty/photo/min/notes, keeps location and (optionally) tags, and refocuses the name field. Cancel discards only the unsaved draft.
- Shopping list flow: To buy → tick → Bought → "Put away in {box}" adds qty back. "Not buying" removes the entry without restocking.
- Share list: `navigator.share({title, text})`, fallback sheet with Email (`mailto:`) + Copy text. No SMS.
- Auth: name tiles + password, argon2id, HttpOnly cookie. Roles are admin / member. An admin reset issues a temp password and sets `must_change_pw`. Never allow removing the last admin.
- Check-outs are deleted on return; there is no history table.
- Theme is persisted per user (`user.theme`) and rendered server-side into `<html data-theme>`.
- Backups: online SQLite backup only when dirty (`PRAGMA data_version`) and idle; stage complete pending backups locally (keep three). A separate LXC systemd timer syncs them and photos to the NAS only when the mount and sentinel are present; app startup must not depend on NAS availability.

## UI rules
- Use the tokens in README → Design tokens via CSS variables on `:root` / `[data-theme=dark]`, exposed through Tailwind v4 `@theme inline`. Do not hard-code colours in components.
- Font: Helvetica Neue / system UI. Use `tabular-nums` for every quantity.
- Radii: 14 buttons/inputs, 18 cards, 24 sheets, full for pills/badges.
- Touch targets ≥ 44px; steppers 52–56px; inputs use a 16px font.
- Breakpoints: <640 bottom tabs + sheets; 640–1023 drawer + FAB; ≥1024 persistent 256px sidebar. Drag-and-drop only at ≥640; Move… sheet everywhere.
- Every fixed-height button gets `whitespace-nowrap`.
- Empty states use dashed boxes with a headline and an action. Search uses a 300ms debounce plus skeleton rows.
- Camera: `<input type="file" accept="image/*" capture="environment">` visually hidden inside a `<label>`.
