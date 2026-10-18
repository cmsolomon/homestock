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
| Config | `config.toml` (serde + `toml`) | Configures the database path, data directory, backup destination, and backup policy. |
| Backup | SQLite online backup API, triggered when the app is idle, copied to a configured backup directory | See §6. |

Expected footprint: a single ~8–12 MB static binary, about 10–20 MB RSS when idle, effectively 0% CPU when idle. The only wakeup is a 60 s backup tick.

## 2. Directory structure

```
homestock/
├── Cargo.toml
├── Dockerfile                  # offline runtime image
├── release.sh                  # builds a portable Docker release archive
├── deploy/                     # Compose, config template, install/upgrade scripts
├── build.rs                    # optional: run tailwind CLI if present
├── config.example.toml
├── migrations/
│   ├── 0001_init.sql           # executed at startup (user_version gate)
│   └── 0002_soft_delete_users.sql # retains removed accounts and reserves names
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
- **Schema version:** SQLite's `PRAGMA user_version` is the schema version, currently `2`. Each migration sets it to its new version. On startup, if an existing database is behind the application schema, the app creates and integrity-checks an SQLite online backup next to the database (named `*.pre-migration-v<old>-to-v<new>-<timestamp>.sqlite3`) before applying any migration. If the backup cannot be completed and verified, startup stops before migration. New databases are initialized without a pre-migration backup.
- **NAS safety:** the backup only runs if `<backup.directory>/<sentinel>` exists. If the CIFS share is not mounted, the mount point is an empty local folder, and writing there would silently fill the container disk.
- **Procedure:** `Backup::new(src, dst).run_to_completion(256 pages, 20 ms pause)` into a **local staging file**, then `PRAGMA quick_check`, then copy to `<backup.directory>/homestock-<unix>.sqlite3.part`, fsync, and rename to the final name. Prune to the newest `keep` files.
  - *Adjustment to the owner's note (“straight to the NAS”):* SQLite's own file locking over CIFS is unreliable, especially with `nobrl`/caching options. Staging locally and copying a finished file keeps the NAS write a plain sequential copy. Set `stage_locally = false` to write directly if the share is known-good.
- **Photos:** they are content-addressed and immutable, so the same tick copies any `photos/*.jpg` missing on the backup destination (`<backup.directory>/photos/`). No diffing is needed.
- Restore: stop the service, copy a backup to the configured `database_path`, delete its `-wal`/`-shm` sidecars, start.

## 7. Routes (server-rendered + HTMX partials)

```
GET  /                          sign-in/setup when signed out; redirect to /loc or /account when signed in
GET  /login  POST /login  POST /logout  POST /setup (only when no active users)
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
GET  /export.csv[?location_id={id}]  (all inventory or selected location subtree)
GET  /household  POST /users  POST /users/{id}/reset  POST /users/{id}/role  DELETE /users/{id}   (admin)
GET  /account  POST /account/password  POST /account/logout-everywhere  POST /me/theme {light|dark|system}
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

## 9. Docker deployment in a Proxmox LXC

- **Build an offline release bundle** on a machine with Docker for the target architecture: `./release.sh 0.2.0`. It builds the multi-stage `Dockerfile`, saves the image, and creates `dist/homestock-0.2.0.tar.gz` with the image, Compose file, example config, deployment guide, and install/upgrade scripts. Transfer and extract the archive inside the LXC. The app image itself makes no external requests at runtime.
- **First install:** review `config.toml` and `.env`, then run `./install.sh`. The config has explicit `database_path` and `[backup].directory` paths inside the container. Compose bind-mounts `/var/lib/homestock` to the persistent data directory (SQLite database, photos, staging) and `/var/backups/homestock` to the backup destination. Keep the database path under `/var/lib/homestock`, or update the Compose data mount as well. The initial admin is created through the setup page.
- **Access and HTTPS:** Compose publishes only `127.0.0.1:8080`; configure Caddy or another reverse proxy in the LXC to forward to it. Use HTTPS (`tls internal` for Caddy on a LAN) because production config enables Secure cookies and `navigator.share` needs a secure context.
- **Backup destination:** set `HOMESTOCK_BACKUP_DIR` in `.env` to the mounted host path, or leave it at `./backups`. Backup files and photos are only written when the configured `.homestock-nas` sentinel exists in that directory. For a CIFS share, mount it on the Proxmox host and bind it into the LXC before pointing Compose at it; create the sentinel on the mounted share, not on an unmounted mount-point directory. Make the destination writable by the UID/GID in `.env` (defaults to `10001:10001`).
- **Upgrade:** unpack the new release over the existing deployment directory and run `./upgrade.sh`. The script verifies the configured data directory and database file exist before it replaces the container. The release archive does not contain `config.toml`, `.env`, or the data directories. It never runs `docker compose down -v`; do not change the data-directory mapping or `database_path` during an upgrade unless intentionally migrating the database. Before applying schema changes, startup writes a verified SQLite backup beside the database. Keep independent backups before upgrades as well.
- **Permissions:** the container runs as UID/GID 10001 by default. Update the two IDs in `.env` to match the owner of the data and backup directories; for a fresh local deployment run `chown 10001:10001 data backups` if the directories were created by root. The CIFS mount must also grant that identity write access.
- **SQLite pragmas** (set on every connection): `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY`, `cache_size=-4000` (≈4 MB). Run `PRAGMA optimize` after each backup.

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
