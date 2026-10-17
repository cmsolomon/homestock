# Homestock — implementation architecture (Rust)

This file adds the owner's implementation plan to the design spec in `README.md`. If the two disagree on **behaviour or UI**, `README.md` wins. This file decides **stack, schema, structure and operations**.

Runnable-shaped starter code is in `starter/`. It has not been compiled. It shows the intended structure and the trickiest logic, and Copilot should build out from it.

---

## 1. Stack

| Concern | Choice | Notes |
|---|---|---|
| Runtime | Rust (stable), `tokio` **current-thread** runtime | Lowest idle footprint. CPU-heavy work (image resize, backup, CSV) goes to `spawn_blocking`. |
| HTTP | `axum` 0.8, `tower-http` (static files, compression, body limit, trace) | Path syntax `/{id}`. |
| DB | SQLite via `rusqlite` (`bundled`, `backup` features), **WAL** | One writer connection behind a `Mutex`, used through `spawn_blocking`. Plenty for 2–6 users, and SQLite serialises writes anyway. |
| Templates | `askama` (compile-time) | Templates are checked at build time and shipped inside the binary. |
| Interactivity | **HTMX** (vendored `static/htmx.min.js`) + ~100 lines of vanilla JS (steppers, drag-and-drop, share) | No SPA. |
| CSS | Tailwind v4 via the **standalone Tailwind CLI** at build time → `static/app.css` | No CDN, ever. See “Offline” below. |
| Auth | `argon2` hashes, random session token in an HttpOnly cookie, `sessions` table | |
| Search | SQLite **FTS5 `trigram`** table → candidate set → Rust re-rank with `strsim` | Handles typos like “batery”. See §5. |
| Images | `image` crate (jpeg/png/webp/heic-less) → resize to 1600 px, re-encode JPEG q82 | Re-encoding also strips EXIF/GPS. Files are content-addressed: `photos/<sha256>.jpg`. |
| CSV | `csv` crate | |
| Config | `config.toml` (serde + `toml`) | Seeds initial users and sets backup policy. |
| Backup | SQLite online backup API, triggered when the app is idle, copied to the CIFS-mounted NAS | See §6. |

Expected footprint: a single ~8–12 MB static binary, about 10–20 MB RSS when idle, effectively 0% CPU when idle. The only wakeup is a 60 s backup tick.

## 2. Directory structure

```
homestock/
├── Cargo.toml
├── build.rs                    # optional: run tailwind CLI if present
├── config.example.toml
├── migrations/
│   └── 0001_init.sql           # executed at startup (user_version gate)
├── styles/
│   └── input.css               # Tailwind v4 @theme tokens (from README tokens)
├── static/                     # embedded or served from disk
│   ├── app.css                 # built by tailwind CLI
│   ├── htmx.min.js             # vendored, no CDN at runtime
│   └── app.js                  # steppers, drag & drop, share, theme
├── templates/
│   ├── base.html               # shell: sidebar / top bar / tab bar, data-theme
│   ├── login.html
│   ├── location.html           # breadcrumb, header, scope toggle, #contents
│   ├── _contents.html          # boxes + items grid + empty state (HTMX target)
│   ├── _item_card.html
│   ├── _box_card.html
│   ├── quick_add.html          # sheet partial
│   ├── _qa_row.html            # one “added this session” row
│   ├── item_detail.html
│   ├── in_use.html  shopping.html  search.html  _search_results.html
│   ├── household.html  account.html
│   └── _toast.html             # OOB swap target
└── src/
    ├── main.rs                 # config, db, migrations, seed, router, backup task
    ├── config.rs
    ├── db.rs                   # Db wrapper (spawn_blocking), pragmas, migrate
    ├── error.rs                # AppError → Response (HTMX-aware)
    ├── auth.rs                 # hashing, sessions, CurrentUser extractor, AdminUser
    ├── model.rs                # row structs + derived status
    ├── repo/
    │   ├── mod.rs
    │   ├── locations.rs        # tree, subtree CTE, path, move (cycle check)
    │   ├── items.rs            # create, split/merge move, checkout, consume, reindex
    │   └── shopping.rs
    ├── search.rs               # FTS5 trigram + re-rank
    ├── photos.rs               # multipart → resize → content-addressed save
    ├── backup.rs               # idle-aware online backup → NAS
    ├── activity.rs             # last-request timestamp middleware
    └── routes/
        ├── mod.rs              # Router
        ├── locations.rs
        ├── quick_add.rs
        ├── items.rs  checkouts.rs  shopping.rs  search.rs
        ├── users.rs  account.rs  export.rs
```

## 3. Schema

The full SQL is in `starter/migrations/0001_init.sql`. Decisions:

- **Adjacency list + recursive CTE** rather than a closure table. A household has hundreds to low thousands of locations, and a recursive CTE over an indexed `parent_id` returns in microseconds. Moving a box (with everything inside) is then a **single-row UPDATE**. With a closure table it would mean rewriting every ancestor/descendant pair. Cycles are rejected in the move transaction.
- Time is stored as unix seconds (`INTEGER`, `unixepoch()`), so no date crate is needed in queries.
- `item.notes` is the only free-text field (Quick add + item detail). There is no separate description field.
- `checkout` rows are deleted on return. No history table, per the owner's requirement.
- `shop` is a real table. The design needs To buy / Bought / Put away and free-text entries. The **Low stock** and **Used up** tabs are dynamic views over `item`.
- `user.theme` (`light|dark|system`) is persisted **per user**. `user.last_location_id` is the remembered Quick-add box.
- `item_fts` is a trigram FTS5 table maintained by **app code** (SQLite triggers cannot run the recursive CTE needed for `path`). Call `reindex(items)` after every item write and after moving a box (reindex its subtree).

## 4. Core logic (see `starter/src/repo/*.rs`)

- `subtree_ids(loc)` / `path(loc)`: recursive CTEs.
- `child_box_stats(parent)`: one CTE returns, for each child box, its recursive item count and box count (feeds “4 items · 1 box” and variant-B tiles).
- `items_in(loc, nested)`: items + `in_use` + tags in one query; consumed items sort last.
- `move_location(id, new_parent)`: cycle check → UPDATE → reindex subtree.
- `move_item(id, to, qty)`: whole-record move, or **split** + **merge** into a same-name, same-unit item at the destination. Splits are limited to `available`.
- `checkout(item, qty, user)`: transaction checks `available` (and a trigger guards it too).
- `put_away(checkout, to)`: moves the record if the checkout covers all of it, otherwise splits. Then deletes the checkout.
- `consume(item | checkout, qty, add_to_list)`: `qty = max(0, qty − n)`; `INSERT OR IGNORE` into `shop` (partial unique index = one open entry per item).
- `restock(shop_id, to?)`: linked → `qty += n`; unlinked → create/merge at `to`.

## 5. Fuzzy search (see `starter/src/search.rs`)

1. Tokenise the query (lowercase, split on whitespace, strip `#`).
2. **Candidates:** for tokens ≥ 3 chars, OR together their trigrams in an FTS5 MATCH (`"bat" OR "ate" OR "ter" …`), limit 400. For shorter tokens, use `LIKE '%x%'` on `name`/`tags` (fast at this size). For an empty query with filters, all items.
3. **Re-rank in Rust:** split name, notes, tags and path into words. Every token must hit a word: prefix 4 / substring 3 / Levenshtein ≤1 (len ≥ 4) or ≤2 (len ≥ 6) → 2. Sort by score, then name.
4. Apply tag and status filters (`in_use`, `low`, `used`) in SQL on the candidate join.
5. HTMX: `hx-get="/search/results" hx-trigger="input changed delay:300ms, search" hx-indicator="#search-skeleton"`.

## 6. Backups (see `starter/src/backup.rs`)

- `activity.rs` middleware stores the timestamp of the last request in an `AtomicU64`.
- The backup task keeps its **own read connection** and polls `PRAGMA data_version`. That value changes whenever *another* connection commits, so it is a free “dirty” flag with no app bookkeeping.
- Every 60 s a backup runs when **dirty AND idle ≥ `idle_secs` (default 300) AND since-last ≥ `min_interval_secs` (1800)**. It also runs when **dirty AND since-last ≥ `max_interval_secs` (86400)**, even if the app is busy, and once on graceful shutdown.
- **NAS safety:** the backup only runs if `<nas_dir>/<sentinel>` exists. If the CIFS share is not mounted, the mount point is an empty local folder, and writing there would silently fill the container disk.
- **Procedure:** `Backup::new(src, dst).run_to_completion(256 pages, 20 ms pause)` into a **local staging file**, then `PRAGMA quick_check`, then copy to `<nas>/homestock-<unix>.sqlite3.part`, fsync, and rename to the final name. Prune to the newest `keep` files.
  - *Adjustment to the owner's note (“straight to the NAS”):* SQLite's own file locking over CIFS is unreliable, especially with `nobrl`/caching options. Staging locally and copying a finished file keeps the NAS write a plain sequential copy. Set `stage_locally = false` to write directly if the share is known-good.
- **Photos:** they are content-addressed and immutable, so the same tick copies any `photos/*.jpg` missing on the NAS (`<nas>/photos/`). No diffing is needed.
- Restore: stop the service, copy a backup to `data_dir/homestock.sqlite3`, delete `-wal`/`-shm`, start.

## 7. Routes (server-rendered + HTMX partials)

```
GET  /login  POST /login  POST /logout  GET/POST /setup (only when no users)
GET  /                          → 302 /loc
GET  /loc                       root (rooms)
GET  /loc/{id}?scope=level|nested
GET  /loc/{id}/contents?scope=  partial: #contents (scope toggle uses hx-get + hx-push-url)
GET  /loc/{id}/quick-add        partial: sheet (locks {id}; also sets user.last_location_id)
POST /loc/{id}/quick-add        multipart → _qa_row.html (hx-swap afterbegin) + OOB toast
GET  /quick-add                 → 302 /loc/{last_location_id}?qa=1 (FAB / tab-bar +)
POST /loc                       new box/room {name, parent_id?}
POST /loc/{id}/move             {parent_id | "root"}   (drag & drop + Move… sheet)
POST /loc/{id}/photo
GET  /items/{id}                partial: detail panel
PATCH /items/{id}               qty, min_qty, notes, tags
DELETE /items/{id}              (Quick-add Undo)
POST /items/{id}/move           {location_id, qty?}
POST /items/{id}/checkout       {qty, user_id}
POST /items/{id}/consume        {qty, add_to_list}
POST /items/{id}/photo
GET  /in-use
POST /checkouts/{id}/return     PATCH /checkouts/{id} {user_id}
POST /checkouts/{id}/put-away   {location_id}   POST /checkouts/{id}/consume {add_to_list}
GET  /shopping?tab=buy|bought|low|used
POST /shopping {name}  POST /shopping/{id}/toggle  POST /shopping/{id}/restock {location_id?}
DELETE /shopping/{id}  (Not buying)   POST /shopping/from-item/{item_id}   POST /shopping/low/all
GET  /shopping/share.txt        plain text for Web Share / Copy / mailto
GET  /search  GET /search/results?q=&tag=&status=
GET  /export.csv
GET  /household  POST /users  POST /users/{id}/reset  POST /users/{id}/role  DELETE /users/{id}   (admin)
GET  /account  POST /account/password  POST /me/theme {light|dark|system}
GET  /photos/*  (ServeDir, Cache-Control: immutable)   GET /static/*
```
HTMX conventions:
- Full page if `HX-Request` is absent, otherwise the partial.
- Errors return `HX-Retarget: #toast` + `HX-Reswap: innerHTML` with `_toast.html`.
- An auth failure on an HTMX request returns `HX-Redirect: /login`.
- Mutations return the updated fragment plus `hx-swap-oob` updates for counts (sidebar badges, tab counts).

## 8. Front-end behaviour notes

- **Theme:** `<html data-theme="{{ user.theme }}">` is rendered by the server. The toggle does `hx-post="/me/theme"` and flips the attribute immediately in JS. Before login, use `prefers-color-scheme`. *(This replaces the README's localStorage note.)*
- **Quick add (variant A):** the form posts multipart, so photo, name, qty, unit, min_qty, tags, keep_tags and notes all go in one request. On success the client:
  - resets name, qty, photo, min and notes;
  - keeps the tags if keep_tags is ticked;
  - refocuses the name field.
  Enter in name or unit submits; Enter in notes adds a newline. Cancel/Done just clears `#sheet`, and the server has already saved each row.
- **Remembered breadcrumb:** the lock is the `{id}` in the URL. Every quick-add POST also stores `user.last_location_id`, so the global + opens on the last box used.
- **Drag & drop** (`static/app.js`, ≥ 640 px only): native HTML5 DnD on `[data-drag-loc]` / `[data-drag-item]`. Drop targets are `[data-drop-loc]` (cards, breadcrumb, sidebar). Drop → `htmx.ajax('POST', …/move, {values, target:'#contents'})`. The server re-renders contents and returns the toast via OOB.
- **Share list:** `fetch('/shopping/share.txt')` → `navigator.share({title, text})`. On a non-abort failure, open the fallback sheet (Email `mailto:` + Copy text).

## 9. Deployment on Proxmox LXC

- **CIFS in an unprivileged LXC:** unprivileged containers cannot mount CIFS themselves. Mount the share on the **Proxmox host** and bind it in:
  ```
  # host /etc/fstab
  //nas/backups/homestock /mnt/nas-homestock cifs credentials=/root/.smb-homestock,vers=3.1.1,uid=101000,gid=101000,file_mode=0640,dir_mode=0750,_netdev,nofail,x-systemd.automount 0 0
  # host
  pct set <CTID> -mp0 /mnt/nas-homestock,mp=/mnt/nas
  touch /mnt/nas-homestock/.homestock-nas      # sentinel
  ```
  (uid 101000 = container uid 1000 under the default unprivileged mapping. Match it to the `homestock` service user.)
- **systemd unit** (inside the container): `User=homestock`, `WorkingDirectory=/var/lib/homestock`, `ExecStart=/usr/local/bin/homestock --config /etc/homestock/config.toml`, `Restart=on-failure`, `MemoryMax=128M`, `NoNewPrivileges=yes`, `ProtectSystem=strict`, `ReadWritePaths=/var/lib/homestock /mnt/nas`. Send `SIGTERM` for a graceful shutdown, which runs a final backup.
- **HTTPS on the LAN:** put Caddy (`tls internal`) or the existing reverse proxy in front. A secure context is required for `navigator.share` on phones. Install Caddy's root CA on household devices once.
- **Offline:** the app makes **no external requests at runtime** and must work with the internet down. Tailwind is a build-time tool: the standalone CLI (single binary, no Node) turns `styles/input.css` + the class names in `templates/` into one small `static/app.css`. HTMX is vendored as `static/htmx.min.js`. There are no web fonts (system font stack), no CDNs and no analytics. Optionally embed `static/` in the binary (`rust-embed` or `include_bytes!`) for a single-file deploy.
- **Release profile:** `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`, `opt-level = 3`.
- **SQLite pragmas** (set on every connection): `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY`, `cache_size=-4000` (≈4 MB). Run `PRAGMA optimize` after each backup.

## Local testing

For local HTTP testing, run `cargo run -- --config config.local.example.toml` and open `http://127.0.0.1:8080`. Create the initial household admin on the setup screen. This config stores test data under `/tmp/homestock-local-data`, binds only to loopback, and disables the Secure cookie flag because the connection is plain HTTP. Use `cookie_secure = true` behind HTTPS; do not expose the local config on a network.

## 10. Build order for Copilot

1. `db.rs`, migrations, `config.rs`, seed users, `auth.rs`, login/setup pages, `base.html` shell with responsive nav.
2. Locations: tree, `/loc/{id}`, scope toggle, box/room create, empty state.
3. Quick add (variant A) with photo upload.
4. Item detail: qty / min / notes edit, move with split/merge.
5. Checkout / In use / return / put away / reassign.
6. Consume + shopping list tabs + share.
7. Search (FTS5 + re-rank) with skeleton.
8. Drag & drop, CSV export, household admin, account/password, theme persistence.
9. Backup task + NAS sentinel + photo sync; systemd + Caddy notes.
