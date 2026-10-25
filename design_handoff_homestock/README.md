# Handoff: Homestock — self-hosted home inventory

## Overview
Homestock is a lightweight, self-hosted web app for a household of 2–6 people, served on the home LAN and used on phone, tablet and desktop. Everything is organised by **physical location** (rooms → furniture → boxes → sub-boxes, unlimited depth). Every screen answers three questions: *where is it, how many are left, who has it.*

Core capabilities:
1. Hierarchical location browser with an on-by-default toggle for nested items, grouped by box when enabled.
2. Mobile Quick-add that **locks** the current location so 10+ items can be added back-to-back.
3. Partial-quantity check-out to a household member, with Return / Put away elsewhere / Reassign / Used up.
4. Used-up and low-stock tracking feeding a Shopping list (To buy → Bought → Put away = restock).
5. Fuzzy search with tag and status filters.
6. Camera capture for item and box photos.
7. Drag-and-drop move of boxes (with all contents) and items on pointer devices; Move… sheet everywhere.
8. CSV export, Light/Dark theme, sign-in, account (change password, sign out), household user admin.
9. Share shopping list as plain text (Web Share → fallback: Email + Copy), because the server is not reachable away from home.

## About the design files
The files in `design/` are **design references built in HTML** — a working prototype that shows the intended look and behaviour. They are **not production code to copy**. Recreate them in the target stack using its normal patterns. If no codebase exists yet, a good fit is:

- **Decided stack (owner):** Rust · Axum + Tokio · SQLite (WAL) · server-rendered Askama templates + **HTMX** · Tailwind v4 (build-time CLI) · **fully offline: no CDN or external requests at runtime** · packaged as a Docker image for a Proxmox LXC, with persistent database storage and idle-time backups to a configured directory.
- **See `IMPLEMENTATION.md`** for architecture, directory layout, routes, backup design and deployment, and `starter/` for the schema SQL and reference Rust/HTMX code (not compiled — a starting shape).
- “Optimistic UI” below means: HTMX swaps the server's fragment immediately. There is no client-side store.

How to open the references: put the `design/` folder anywhere and open `Home Inventory UX Concept.dc.html` in a browser (keep `support.js` next to it). The concept doc embeds live instances of the prototype. `Home Inventory Prototype.dc.html` is the full clickable app (demo password: `homestock`). Data is in-memory seed data and resets on reload.

## Fidelity
**High-fidelity.** Colours, type, spacing, radii, copy and interactions are final. Recreate the UI to match. The two **variant** props in the prototype (`locationView` A/B, `quickAddStyle` A/B) were explorations. The owner chose:
- **Location view: A (Sectioned)** — implement this one.
- **Quick-add: A (Sheet form)** — implement this one. B (rapid line) is optional and out of scope.

---

## Information architecture & routes

```
/                      Home: sign in/setup when signed out; inventory when signed in
/login                 Sign in (unauthenticated)
/loc                   All locations (top-level rooms)
/loc/:id               Location / container view (any depth)
  item detail          side panel (≥640) / full-screen sheet (<640), URL: ?item=:id
/in-use                Checked-out items grouped by person
/shopping?tab=buy|bought|low|used
/search?q=&tag=&status=
/household             Members + admin actions, CSV export, appearance
/account               Change password, sign out
```
Global: Quick add (+), Search (`/` key on desktop), Export CSV, Theme toggle, Account.

## Data model

```
Authoritative SQL: starter/migrations/0001_init.sql. Summary:
location  id, name, kind, parent_id NULL, photo, created_at          (adjacency list + recursive CTE)
item      id, name, notes NULL, location_id, qty ≥0, unit 'pcs', min_qty 0, photo, created_at, updated_at
tag / item_tag                                                       (many-to-many)
checkout  id, item_id, qty >0, user_id, checked_out_at               (deleted on return — no history kept)
shop      id, name, item_id NULL, qty, bought_at NULL, created_at    (one open entry per item)
user      id, name, role, password_hash (argon2id), must_change_pw, theme ('light'|'dark'|'system'), last_location_id, deleted_at
session   token, user_id, created_at, expires_at
item_fts  FTS5 trigram (name, notes, tags, path)       (app-maintained)
```
Derived values:
- `in_use(item) = Σ checkout.qty`
- `available = qty − in_use`
- `low = qty > 0 AND min_qty > 0 AND available ≤ min_qty`
- `used_up = qty = 0` (record is kept so it can be restocked)
- Nested items: `WITH RECURSIVE sub(id) AS (SELECT :id UNION ALL SELECT l.id FROM location l JOIN sub ON l.parent_id = sub.id) SELECT * FROM item WHERE location_id IN sub`

Quantity rules:
- Check-outs are claims against an item's count. They never create item records.
- **Partial move** splits the record: source `qty -= n`, destination gets a new record, or **merges** into an existing same-name item (case-insensitive) in that location. Checked-out units cannot be moved (max = available).
- **Return** deletes the checkout.
- **Put away elsewhere** (from a checkout): if the checkout covers the whole item, move the item; otherwise split `checkout.qty` to the destination (merge rules apply). Then delete the checkout.
- **Used up**: `qty -= n` (floored at 0). From a checkout, also delete the checkout.
- **Restock** (Bought → Put away): linked item `qty += shop.qty`, delete the shop entry. Unlinked entries open a location picker and create/merge an item there.
- Suggested buy qty: `max(1, min_qty − available + 1)`; for used-up items `max(1, min_qty || 1)`.

**Notes** is the single free-text field (Quick add + item detail) and is searchable. There is no separate description field.

## Suggested API
> Superseded by the HTML/HTMX route table in `IMPLEMENTATION.md §7`. Kept here for the operations each screen needs.
```
POST   /api/auth/login {userId,password}       POST /api/auth/logout
GET    /api/me                                 POST /api/me/password {current,next}
GET    /api/users  (public list for login)     POST/PATCH/DELETE /api/users[/id]  (admin)
POST   /api/users/:id/reset-password → {tempPassword}   (admin)
GET    /api/locations (flat list; build tree on client)  POST/PATCH/DELETE /api/locations[/id]
PATCH  /api/locations/:id {parentId}  — reject cycles (target is self or descendant) with 409
GET    /api/locations/:id/items?recursive=1
POST   /api/items   PATCH /api/items/:id   DELETE /api/items/:id
POST   /api/items/:id/move {locationId, qty}
POST   /api/items/:id/consume {qty, addToList}
POST   /api/items/:id/photo  (multipart; resize to 1600px long edge, strip EXIF GPS)
POST   /api/checkouts {itemId,qty,userId}      DELETE /api/checkouts/:id (return)
PATCH  /api/checkouts/:id {userId}             POST /api/checkouts/:id/put-away {locationId}
POST   /api/checkouts/:id/consume {addToList}
GET/POST/PATCH/DELETE /api/shopping[/id]       POST /api/shopping/:id/restock {locationId?}
GET    /api/search?q=&tags=&status=
GET    /export.csv[?location_id={id}]  (all inventory, or a location and its nested boxes)
```
CSV columns: `id,name,quantity,unit,available,in_use,checked_out_by,location,tags`. `checked_out_by` is `"Sam (1); Mia (2)"`, `location` is the full path joined with ` › `, and `tags` are joined with `; `. Values are RFC 4180 quoted.
The All locations page exports the full inventory; each room/box page exports its subtree and keeps the full box path in every row.

---

## Layout & responsive rules
Switch on **container width**, not user agent.

| | Mobile < 640 | Tablet 640–1023 | Desktop ≥ 1024 |
|---|---|---|---|
| Nav | Top bar 60px (menu · title · account avatar · theme · CSV) + bottom tab bar 68px: Places · In use · **+** · List · Search | Top bar; sidebar becomes a 256px off-canvas drawer over a scrim; floating "+ Quick add" pill bottom-right (56px tall, radius 20) on location screens | Persistent 256px sidebar |
| Content padding | 16 / 16 / 28 | 24 / 24 / 100 | 28 / 36 / 48 |
| Item grid | `repeat(auto-fill,minmax(min(100%,156px),1fr))`, gap 12 → 2 cols | 3–4 cols | 4–6 cols, max-width 1280 |
| Item detail | full-screen sheet | right panel `min(440px,100%)` over scrim | same |
| Dialogs | bottom sheet, radius 24 24 0 0, grabber 40×5, max-height 92% | centred, width `min(480px, 100% − 32px)`, radius 24 | same |
| Quick add A | sheet from top:24 to bottom | right drawer, inset 12, width `min(440px,100% − 24px)`, radius 24 | same |
| Drag & drop | off (use Move…) | on | on |

Sidebar (top → bottom): brand row (26px accent square r9 + "Homestock" 16/700); Quick add button (44px, accent, r14); Search field-button (40px, r12, shows `/` key hint); nav (40px rows, r12; active = surface-2 bg, 700 weight; trailing count, tabular nums); "LOCATIONS" label + "drag to move" hint; location tree (34px rows, indent 14px per level, ▶/▼ caret 22×28, recursive item count right); account row (52px, avatar 32 + name + "Admin · Account"); footer row: Export CSV + theme toggle (40px each).

## Screens

### 1. Location & container view (variant A — Sectioned)
- **Breadcrumb:** pills 34px high, r999, padding 0 10, 13px. Current = surface-2 bg, 700 weight. Separators `›` text-3. Scrolls horizontally on mobile. Every segment (including "All locations") is a drop target.
- **Header:** kind label (11/600 uppercase, .08em, text-3), title 26/700/-0.02em, meta 13 text-2 (e.g. "3 boxes · 9 items in total"; at root: "4 rooms · 14 places · 15 items"). Actions on the right (they wrap onto their own line on mobile): **+ Add item** (primary, 44px, r14, padding 0 18), **Add box inside** / at root "Add room" (secondary), **Move box** (secondary, hidden at root). All buttons are nowrap.
- **Boxes inside · N:** grid `minmax(min(100%,220px),1fr)`, gap 10. Card: padding 10 12 10 10, r16, 1px border, 52px thumb r12 (striped placeholder or photo), name 14/600 ellipsis, meta 12 text-3 ("4 items · 1 box" / "Empty"), trailing `›`. Hover: border-strong + shadow.
- **Show nested items:** an on-by-default on/off switch on both All locations and individual location pages. On All locations, room items are always shown and enabling the switch also includes all nested boxes. On a location page, enabling it includes all boxes inside that location. Items are grouped by their storage location; group headings show the box path and link to that location.
- **Item card:** r18, 1px border, overflow hidden. Photo area 4:3 with striped placeholder; qty pill top-left (12.5/700 tabular, padding 4 10, surface bg). Body padding 10 12 12, gap 6: name 14/600; badges row (status badges first, then max 2 tags). Used-up cards render at opacity .7 and sort last.
- **Empty box:** dashed 1.5px border-strong, r22, padding 44 24, centred. Headline "{name} is empty" (17/700), body "Add things one after another with Quick add, create a box inside it, or drag a box here from the sidebar.", buttons "Quick add items" (primary) and "Add a box inside". The whole block is a drop target.

### 2. Item detail
Header bar "ITEM" label + 44px close. Body padding 16 20 24, gap 16:
- Photo 16:10, r18, tap = camera input; pill bottom-right "Add photo" / "Retake photo" (36px, nowrap).
- Name 22/700; location path as accent link pills (each one navigates).
- 3-cell stat grid (r18 border): **Total** with −/+ (32px buttons r10; min = in_use), **Available**, **In use** (cell on use-soft bg, use colour). Numbers 22/700 tabular.
- **Low-stock alert** row: label + hint ("Off — never flagged low" in text-3, or "Flag as low at 2 box or fewer available" in `--low`) + compact stepper (44px, surface-2, r12, min 0).
- Unit line, status badges, tags; **Notes** block (surface-2, r16, pre-wrap) only if there are notes.
- 2×2 action grid, 48px r14: **Check out** (primary; disabled at opacity .45 when available = 0), **Used up…** (same disabled rule), **Move…**, **Add to list** / "On shopping list".
- **Checked out** list: rows on use-soft, r16: avatar 30, "Sam · 1 box", "since 2 days ago", buttons Return and Elsewhere.

### 3. Quick-add (variant A — Sheet form)
- Header: "Quick add" 18/700 + subline ("Enter adds the next one" / "3 added"), **Done** (44px, surface-2, r14).
- **Locked bar:** margin 0 16, padding 10 8 10 14, r16, accent-soft. Padlock built from two simple shapes, then "ADDING TO · LOCKED" (10.5/700 uppercase accent) and the full path (14/600 ellipsis), then **Change** (40px, surface bg, accent text). Change swaps the form for a flat indented location list (44px rows, indent 16/level, selected = accent-soft). Picking one re-locks and clears the session list.
- **Form** (padding 16, gap 14):
  - Photo button 96×96, r18, dashed 1.5px border-strong, camera glyph + "Photo"; the preview fills it after capture.
  - Item name input 52px, r14, **16px font** (prevents iOS zoom), autofocus, `enterkeyhint="next"`, placeholder "e.g. Ear plugs".
  - Quantity stepper (52px buttons, 56px value, r14) + Unit input (default "pcs").
  - **Low-stock alert** row (same as detail; per item, resets to 0 after each add).
  - Tags: chip input (chips are accent-soft pills with ×; Enter or comma adds; Backspace on empty removes the last); checkbox "Keep tags for the next item" (default on).
  - **Notes** textarea (min-height 88, r14, 16px, optional, placeholder "e.g. Expires 03/2027, spare set for the car"). Enter inserts a newline; it does not submit.
  - "ADDED THIS SESSION" list: 36px thumb, name, qty, **Undo** (deletes that item).
- **Footer** (border-top, padding 12 16 16): **Cancel** (flex 1, secondary, 56px) | **Add & next ↵** (flex 2, primary, 56px, opacity .5 while name is empty).
- Behaviour: Enter in name/unit = add. On add: save optimistically; reset name, qty→1, photo, min→0, notes; keep location; keep tags if ticked; refocus name so the keyboard stays up. Cancel/Done/scrim close: discard the unsaved draft and show the toast "N items added to {box}" if N > 0. Default lock = the location being viewed, otherwise the last used one.

### 4. Check-out dialog
Subject card (surface-2 r16: 48 thumb, name 15/600, path). "How many?" stepper (56px buttons, 64px value, 22/700) + "of 3 box available"; "All N" link when max > 2. "Who has it?": user pills grid `minmax(104px,1fr)`, 52px, r16, 2px border (selected = accent border + accent-soft). Defaults to the signed-in user, labelled "Alex (you)". Footer: Cancel (flex 1) | "Check out 1 box" (flex 2), 52px r16.

### 5. In use
Title + meta "2 checkouts across 2 people. Return puts things back where they came from." Groups per person: 30px avatar (use-soft), name 15/700, count. Row card r18: 52 thumb, name + qty pill (use-soft), "from Garage › Shelf A › Bin 3 · 2 days ago"; buttons (40px r12, nowrap, wrap as a group): **Return to {origin}** (primary), **Elsewhere…**, **Reassign**, **Used up**. Empty: "Everything is in its place".

### 6. Move / Put-away dialog (shared)
Title varies: "Move Storage Box 12" (sub: "Everything inside moves with it · 4 items, 1 boxes"), "Move {item}" (stepper when splitting; "1 in use stay behind"), "Put away somewhere else" (from a checkout), "Put away" (unlinked shopping entry). Destination list: bordered r16 box, max-height 320, scrolls; 44px rows indented 16/level; right-aligned note ("current", "this box", "inside", "origin"). Invalid rows are disabled at opacity .4: for boxes that is self, descendants and the current parent; "Top level" is only offered for boxes. Confirm reads "Move box → Garage".

### 7. Used-up dialog
Stepper "How many were used?" + "leaves 5 pcs in Bin 4 · Batteries" (no stepper when it comes from a checkout). A checkbox card "Add to shopping list" with hint "Suggested: you keep at least 8 pcs" is pre-ticked when the result is ≤ min_qty. Confirm "Mark 1 used".

### 8. Shopping list
- Header: title + meta "2 to buy · tick when bought, then put away to restock", and **Share list** (primary 44px, share glyph; disabled when nothing is in To buy).
- Tabs (segmented, 36px, horizontal scroll, no scrollbar): **To buy · N**, **Bought · N**, **Low stock · N**, **Used up · N**.
- To buy: an add row (48px input "Add something to buy" + Add). Rows min-height 60, r18: 48px checkbox hit area around a 24px box (r8; ticked = accent fill + ✓), name 14.5/600, sub "Restocks Garage › Shelf A › Bin 4" or "Not linked to an item", pill **"Buy 2 pcs"** (nowrap), **Remove** button. Removing a linked entry leaves its item in Used up / Low stock.
- Bought: name struck through in text-2, then **Put away in {box}** (primary; restocks) or **Put away…** for unlinked entries (opens the location picker). **Move back to To buy** unticks the entry; **Remove** deletes it without restocking.
- Low stock: items where `low` is true. Row: name + "Low stock" badge, "6 pcs left · alert at 8 · Bin 4 · Batteries", **Add to list** or **Remove from list**. Removing a listing leaves the item in Low stock; both actions update the row and list count immediately.
- Used up: same row with the "Used up" badge and full path.
- Empty states (dashed box): To buy "Nothing to buy. Mark an item “Used up” and tick “Add to shopping list” to fill this."; Bought "Ticked items wait here until you put them away."; Low "Nothing is at or below its alert level. Set an alert level on an item to track it here."; Used "No used-up items."

**Share list:** the text format is
```
Shopping list · 17 Oct

☐ 9V batteries — 2 pcs
☐ Olive oil — 1 bottle

Sent from Homestock
```
Call `navigator.share({title:'Shopping list', text})`. Ignore `AbortError`; on any other error, or if the API is missing, open the fallback sheet: hint "N items as plain text — email it, or copy and paste anywhere", a read-only preview textarea, the checkbox "Include where each item goes" (appends " (→ Bin 4)"), a full-width **Email list** link (`mailto:?subject=Shopping%20list&body=…`), and the primary footer **Copy text** (Clipboard API with an `execCommand('copy')` fallback, toast "List copied — paste it anywhere"). No SMS option.

### 9. Search
Input 52px r16, placeholder "Search items, tags, places — typos are fine", clear ×. Chip row (36px pills, horizontal scroll): status chips In use / Low stock / Used up, then every tag sorted by frequency (selected = accent-soft + accent border). Debounce 300ms, then show 4 skeleton rows (48 thumb + 2 bars, `hsPulse` opacity .45↔1, 1.1s ease-in-out infinite). Results: "PLACES" rows (accent-soft, kind label, path) then "ITEMS · N" rows (64px: thumb, name, full path, badges, qty pill). Idle state: an explainer line plus example chips (n95, batery, binder, cables). No results: "Nothing matches “{q}”" + "Try a shorter word, a tag, or clear the filters."
**Fuzzy matching:** tokenise the query on whitespace and strip a leading `#`. Index each item's name + tags + location-path words. Every token must match some word. Score: prefix 4, substring 3, Levenshtein against the word or its first `len+1` chars ≤ 1 (token ≥ 4 chars) or ≤ 2 (token ≥ 6 chars) scores 2. Sort by score, then name. Server-side, SQLite FTS5 with trigram tokenizer + this re-rank is enough.

### 10. Sign in
Full-screen, centred column max 400: 56px accent tile, "Sign in to Homestock" 24/700, "Our home". "Who’s signing in?" grid of 2 user tiles (56px, r16, avatar 30). Password input 52px (`autocomplete=current-password`). The error state turns the input border `--gone` and shows the inline message "That password doesn’t match. Try again or ask an admin to reset it." Primary **Sign in as {name}** 56px. Footnote: "Forgot your password? Ask an admin to reset it under Household." After sign-in, go to the last screen, or to /account if `must_change_pw`. **First run** (no users): same layout, creates the first admin.

### 11. Account
Banner (low-soft) when `must_change_pw`: "Choose a new password to continue." Profile card (52 avatar, name, "Admin · signed in on this device"). Change password: current / new ("At least 8 characters") / confirm. Errors, shown inline in `--gone`: "Current password is incorrect.", "New password needs at least 8 characters.", "The two new passwords don’t match.", "Choose a password different from the current one." Success: "Password updated." in `--ok`. **Sign out**, **Sign in as another user**, and **Log me out everywhere** (revokes all sessions for that user).

### 12. Household & users
Title "Household & users" + explainer. Member rows (min 64, r18): avatar 38, name ("Alex (you)"), role badge (Admin = accent-soft/accent, Member = surface-2/text-2), "Temporary password" badge (low) when must_change, sub "1 checked out" / "Nothing checked out". Admin actions on others: **Reset password** (immediately generates and shows a temporary password in a modal), **Make admin / Make member**, **Remove** (gone text). Resetting signs out the user and requires them to choose a new password at first sign-in. Admins can remove their own account only when another admin remains; checked-out items must be returned first. **+ Add user** (admin only); members see "Only admins can add, remove or reset members." Then the Data card (Export inventory → **Download CSV**) and Appearance (Light | Dark segmented).
- Add member dialog: name, role cards (Member "Browse, add, check out" / Admin "Also manages users"), temporary password shown in monospace 21px (format `word-word-NN`).
- Reset password: immediately displays a generated temporary password in a modal; the current password stops working and open sessions are signed out. The user must choose their own password at first sign-in.
- Any signed-in user can change their own password from Account using their current password. If forgotten while signed out, ask an admin for a temporary password.
- Remove: blocked if the user has checked-out items or is the last active admin; an admin can remove their own account only when another active admin remains. Removal signs out sessions and retains the account record; deleted names remain reserved and cannot be reused.

## Drag & drop (pointer devices only)
Sources: box cards (moves the box + all descendants) and item cards (moves the whole record). Targets: box cards, breadcrumb segments, sidebar tree rows, the empty-box zone. The source fades to opacity .4 while dragging. A valid target gets `inset 0 0 0 2px var(--accent)` + accent-soft fill; box cards also show the overlay "Drop into {name}". Invalid targets (self, descendants, current parent) get no highlight and do nothing. On drop: optimistic move and the toast "Moved Storage Box 12 and 4 items to Garage". Touch never relies on drag; use Move….

## Global behaviour
- **Toast:** bottom-centre pill (text colour bg, bg colour text, 13.5/600), bottom 84px on mobile and 24px otherwise, auto-hide after 2.8s, single instance.
- **Optimistic writes** for create/move/checkout/consume. On error: revert + toast with Retry.
- **Photos:** show a local `URL.createObjectURL` preview immediately; upload in the background. `<input type="file" accept="image/*" capture="environment">` is visually hidden inside a `<label>` (never `display:none`).
- **Keyboard:** `/` focuses search (outside inputs); `Esc` closes dialog / detail / drawer.
- **Theme:** `data-theme` on `<html>`, rendered by the server from `user.theme` (persisted **per user**, toggled with `POST /me/theme`). `system` and the sign-in screen follow `prefers-color-scheme`.
- **Disabled:** opacity .45 + the `disabled` attribute.
- **Auth:** HttpOnly SameSite=Lax session cookie, 30-day sliding expiry, argon2id, 5 login attempts/min rate limit. Members cannot call admin endpoints. The last admin cannot be removed; an admin can remove their own account only if another admin remains.

## Touch targets
44×44 minimum everywhere (icon buttons get the full 44). Dialog steppers use 56px buttons; long-press repeats after 400ms at 10/s. Primary sheet actions are 52–56px tall and full width above the safe-area inset. Checkbox rows have a 48px hit area. Keep ≥ 8px between adjacent targets. Inputs use a 16px font.

---

## Design tokens

Font: `"Helvetica Neue", Helvetica, Arial, system-ui, sans-serif`. Use `font-variant-numeric: tabular-nums` for all quantities. Use monospace only for temp passwords and code.

Type scale: display 26/700 (−0.02em, lh 1.15) · h2 22/700 · title 18/700 · body 14/1.45 · input 16 · meta 12.5 · label 11/600 uppercase .08em · badge 11/600 · numeric stat 22/700.

Spacing: 4 · 8 · 12 · 16 · 20 · 24 · 32 · 48. Radius: 8 checkbox · 10–12 thumbs/small buttons · 14 buttons/inputs · 16 rows/sheet sub-cards · 18 cards · 22 empty states · 24 sheets/dialogs · 999 pills/badges/chips.

Shadows: light `0 1px 2px rgba(40,30,15,.06), 0 8px 24px rgba(40,30,15,.08)`; dark `0 1px 2px rgba(0,0,0,.4), 0 10px 28px rgba(0,0,0,.4)`. Scrim: light `rgba(29,26,22,.36)`, dark `rgba(0,0,0,.58)`.

| Token | Light | Dark | Use |
|---|---|---|---|
| --bg | #f7f5f1 | #15130f | app background |
| --surface | #fffdfa | #1d1a16 | cards, sidebar, sheets, inputs |
| --surface-2 | #efebe4 | #27231e | segmented track, steppers, pills |
| --border | #e6e1d8 | #332e27 | card outlines |
| --border-strong | #cfc8bc | #4a4339 | inputs, secondary buttons |
| --text | #1d1a16 | #ede8e0 | primary text |
| --text-2 | #514a41 | #bab1a5 | secondary text |
| --text-3 | #6b6358 | #998f82 | meta / labels |
| --accent | oklch(0.53 0.19 268) ≈ #3d5bd9 | oklch(0.75 0.12 268) | primary, current location, focus |
| --accent-fg | #ffffff | #13110e | text on accent |
| --accent-soft | oklch(0.95 0.03 268) | oklch(0.31 0.06 268) | locked bar, selection, drop target |
| --use / -soft | oklch(0.50 0.12 200) / oklch(0.95 0.03 200) | oklch(0.78 0.10 200) / oklch(0.30 0.045 200) | In use |
| --low / -soft | oklch(0.50 0.12 65) / oklch(0.95 0.04 75) | oklch(0.80 0.10 75) / oklch(0.31 0.045 75) | Low stock |
| --gone / -soft | oklch(0.50 0.12 28) / oklch(0.95 0.03 28) | oklch(0.76 0.10 28) / oklch(0.30 0.045 28) | Used up, destructive |
| --ok / -soft | oklch(0.50 0.12 150) / oklch(0.95 0.03 150) | oklch(0.78 0.10 150) / oklch(0.30 0.045 150) | Bought, on list, success |

Focus ring: `border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-soft)`.
Image placeholder: `repeating-linear-gradient(135deg, var(--surface-2) 0 7px, var(--bg) 7px 14px)`.

The Tailwind v4 `@theme` block, plus Tailwind markup for the item card, badges, stepper, camera input and locked bar, are in the concept doc (sections 2 and 4). Copy them from there.

## Assets
No bitmap or icon assets. All glyphs are text characters (+ − × › ▶ ▼ ✓ ↵) or simple CSS shapes (padlock, camera, share, hamburger). In production, swap them for an icon set (e.g. Lucide: plus, minus, x, chevron-right, chevron-down, check, corner-down-left, lock, camera, share, menu). Item and box photos are user content.

## Files
- `design/Home Inventory UX Concept.dc.html` — concept doc: IA map, flows, tokens, live screen frames, handover notes, Tailwind snippets.
- `design/Home Inventory Prototype.dc.html` — full clickable prototype. Logic (state transitions, quantity rules, fuzzy search, CSV, share text) is in the `<script data-dc-script>` class at the bottom. Read it as the behavioural spec.
- `design/support.js` — runtime needed only to open the two files above. It loads React from the internet, so the design references need a connection to open. Not part of the app, and not a pattern to copy.
- `IMPLEMENTATION.md` — Rust/Axum/HTMX architecture, routes, backups, LXC/CIFS deployment, build order.
- `starter/` — `Cargo.toml`, `config.example.toml`, `migrations/0001_init.sql`, `src/` (db, auth, repo, search, backup, photos, routes), `templates/` (base, location, contents, item card, quick add, session row), `static/app.js`, `styles/input.css`.
- `.github/copilot-instructions.md` — condensed rules for GitHub Copilot; copy it into the repo root.
