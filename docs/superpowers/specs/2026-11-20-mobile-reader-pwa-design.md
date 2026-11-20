# Mobile reader PWA: design

Date: 2026-11-20. Status: approved in brainstorming, awaiting implementation plan.

## Goal

A read-only, installable web app for reading a catalog on a phone (iPhone
through Safari's Add to Home Screen, Android through Chrome's install prompt),
with no AI in the loop. It runs on the existing server, on the same origin as
the web console, and reuses the console's session sign-in.

## Decisions

| Question | Decision |
|---|---|
| Native (Flutter + Rust) or PWA | PWA: one codebase, no app-store distribution, no new auth. |
| Offline | Recently viewed concepts are readable offline (bounded cache). No whole-catalog sync. |
| Where it lives | A dedicated reader at `/read/`, separate from the console at `/ui/`, with its own manifest and service worker scoped to `/read/`. |
| Session end | A `401` wipes the reader's caches and goes to sign-in; a lost catalog grant evicts only that catalog; offline, cached content stays readable. |
| Screens | Catalog picker, home (search and recent changes), search results, concept view with backlinks, tree browse, account. |
| Offline search | None. Offline, search says it needs a connection; home lists what is available offline. |
| Styling | Independent of the console: Pico CSS plus the reader's own `read.css` and tokens. The console's `styles.css` is not loaded. System fonts. |
| Cache cap | 200 concepts, least recently viewed evicted first. |

## Out of scope (MVP)

Editing; history and diffs; open questions and provenance detail; offline
search; whole-catalog sync; push notifications; background sync; catalog
unlock inside the reader (the reader links to the console for it); API-key
sign-in (the reader is cookie-only).

## Architecture

### Serving

- The reader's files live under `src/http/ui/read/`, so `build.rs` already
  embeds them in the `ASSETS` table that `src/http/ui.rs` serves.
- `src/http/ui.rs` adds routes `/read` (permanent redirect to `/read/`),
  `/read/` (serves `read/index.html`) and `/read/{*path}` (serves
  `read/<path>`), behind the same `security_headers` layer and CSP. The
  console's `/ui/{*path}` route must not serve `read/...` files (they are
  reachable only under `/read/`), and the reader's route serves only the
  `read/` prefix.
- `/read/sw.js` is served with `Cache-Control: no-cache`, so a deploy reaches
  installed apps on their next launch. Its location makes its scope exactly
  `/read/`; it never controls a console page.
- `content_type()` gains `png` (`image/png`) and `webmanifest`
  (`application/manifest+json`).
- `is_safe_return_to` in `src/http/pages.rs` also accepts paths starting
  `/read/`, so sign-in can return to the reader. The existing visible-ASCII
  and no-backslash guards still apply.
- The existing CSP (`default-src 'self'`, `script-src 'self'`,
  `connect-src 'self'`) already permits a same-origin service worker and
  manifest; it is not loosened.

### Files under `src/http/ui/read/`

- `index.html`: viewport with `viewport-fit=cover`, `<link rel="manifest">`,
  `apple-touch-icon`, `apple-mobile-web-app-capable` and status-bar meta,
  `theme-color` per colour scheme; loads `/ui/vendor/pico.min.css`,
  `read.css` and `js/main.js` (module). No web fonts.
- `manifest.webmanifest`: `name`, `short_name`, `start_url` and `scope`
  `/read/`, `display: standalone`, `background_color` and `theme_color`
  from the reader palette, icons 192 and 512 px (PNG, one `maskable`).
- `icons/`: `icon-192.png`, `icon-512.png`, `icon-maskable-512.png`,
  `apple-touch-icon.png` (180 px).
- `read.css`: the reader's own tokens on `:root` (overriding `--pico-*`
  variables for type scale, spacing, colours, radius, plus reader tokens for
  measure, touch-target size and safe-area padding), light and dark sets by
  `prefers-color-scheme`, phone-first layout, and rules for the classes
  `src/render.rs` emits in `body_html`. That class list is the one contract
  shared with the console's stylesheet: a new class in `render.rs` may need a
  rule in both.
- `sw.js`: the service worker (caching, below).
- `js/main.js`: hash router and screen dispatch; registers `sw.js`.
- `js/api.js`: read-only client (below).
- `js/cache.js`: page-side messages to the worker (wipe all, evict catalog,
  evict concept, list offline concepts).
- `js/screens/*.js`: one module per screen.

The reader imports `../../js/core/dom.js` (`h()`, `clear()`) and
`../../js/core/format.js` from the console. It does not import the console's
`api.js` (bearer keys, toasts, unlock dialog), `router.js` or
`components/tree.js` (which depends on the console router).

### Screens and routes

| Route | Screen | Data |
|---|---|---|
| `#/` | Home: search box and recent changes for the current catalog; offline, the "Available offline" list instead | `GET /api/v1/catalogs/{c}/recent-changes` |
| `#/c` | Catalog picker; skipped when the user reaches one catalog. The last choice is remembered in `localStorage` (wrapped in try/catch). | `GET /api/v1/catalogs` |
| `#/t/{catalog}/{folder}` | Tree: drill-down list of one folder's subfolders and documents, index first | `GET /api/v1/catalogs/{c}/tree` |
| `#/s/{catalog}?q=` | Search results: title, path, snippet | `GET /api/v1/search?q=&catalogs={c}&limit=50` |
| `#/v/{catalog}/{path}` | Concept: title, breadcrumbs (tree folders), collapsible outline, `body_html`, a "Show the rest" action when the body is shortened (refetches with `?full=true`), and a "Linked from" list | `GET /api/v1/catalogs/{c}/concepts/{path}`, `GET .../links/{path}` |
| `#/me` | Signed-in user and sign-out | `GET /api/v1/session` (user, `csrf`, `expires_at`; the `csrf` token is kept in memory only), `DELETE /api/v1/session` with the `X-OKF-CSRF` header |

Concept links in `body_html` are written by `src/render.rs` as
`#/browse/{catalog}/{path}[#{slug}]`. The concept screen rewrites that prefix
to `#/v/`. External links get `target="_blank" rel="noopener"`. `body_html`
is inserted with `innerHTML` exactly as the console does: it is produced and
sanitised server-side by `render.rs`.

## Caching (service worker)

Three caches:

1. `read-shell-<version>`: precached on `install` (`index.html`, `read.css`,
   JS modules, icons, manifest, `/ui/vendor/pico.min.css`, the imported
   console core modules). Cache-first. `<version>` is the crate version,
   written into `sw.js` when the server serves it (a placeholder substituted
   by `src/http/ui.rs`), so each release replaces the shell. `activate`
   deletes every `read-shell-*` cache except the current one and calls
   `clients.claim()`.
2. `read-content`: concept GETs (`/api/v1/catalogs/{c}/concepts/{path}`,
   keyed on the full URL including `?full=true`). Stale-while-revalidate:
   online, the cached copy is answered at once and a fresh fetch updates the
   cache, and the page re-renders if the fresh body differs (the worker posts
   a message); offline, the cached copy is answered. Capped at 200 entries,
   evicting the least recently viewed (the worker keeps a small IndexedDB
   index of URL to last-viewed time).
3. `read-meta`: the catalog list, trees, recent changes and backlinks.
   Network-first, cache fallback.

Never cached: search, `/api/v1/session`, `/api/v1/me`, every non-GET request, everything outside
`GET /api/v1` and the shell.

Every answer the worker serves from `read-content` or `read-meta` carries
`X-Read-Cache: hit; at=<RFC 3339 time cached>`. The worker never builds an
error page; it passes the network's answer through, answers from cache, or
lets the fetch fail.

Caches are tied to the signed-in user: the page tells the worker the user's
name after `GET /api/v1/session` succeeds, and the worker wipes `read-content` and
`read-meta` when it changes. Sign-out tells the worker to wipe both before
calling `DELETE /api/v1/session`.

## Error handling

`js/api.js` classifies every answer; only the page decides what to do:

| Answer | Action |
|---|---|
| `2xx` | Render. If `X-Read-Cache` is present and the network was unreachable, show "Offline, synced <ago>". |
| `401` | Wipe `read-content` and `read-meta`; navigate to `/signin?return_to=/read/` plus the current hash. |
| `403` with body `error` `Forbidden`, or `404` on a catalog route | Evict that catalog's entries in both caches and the remembered catalog; go to the picker with a short note. |
| `404` on a concept | Evict that concept's entries; show "This concept no longer exists" and a link to its parent folder. |
| `403` with body `error` `UnlockRequired` or `Suspended` | Keep the caches; show the server's message; for `UnlockRequired`, link to the console (`/ui/`) to unlock. |
| Network failure with no cached copy | "Not available offline" with a link to the Available Offline list. |

## Testing

Rust unit tests (`src/http/ui.rs`, `src/http/pages.rs`):

- `/read/`, `/read/sw.js`, `/read/manifest.webmanifest` and an icon are
  served with the right content types and the security headers.
- `/read/sw.js` carries `Cache-Control: no-cache` and the crate version.
- `/ui/read/...` does not serve reader files; `/read/` does not serve console
  files.
- `safe_return_to` accepts `/read/...` and still rejects `//host`,
  backslashes and other prefixes.

Browser journeys (`tests/ui_e2e/read.rs`, rustwright headless Chromium):

1. Signed out, `/read/` goes to sign-in and returns to the same route.
2. Catalog picker, tree drill-down, concept, an internal link, "Linked from".
3. Search renders results; offline, search says it needs a connection.
4. View three concepts, go offline, reload: all three render with the synced
   note; an unviewed concept shows "Not available offline".
5. With a lowered cap (test hook), the least recently viewed concept is
   evicted.
6. Revoke the session server-side: the next online request wipes the caches
   and goes to sign-in; offline afterwards shows nothing cached.
7. Remove the grant to catalog X: X's entries are evicted, catalog Y's
   survive.
8. `UnlockRequired`: caches survive; the unlock link points into the console.
9. Sign-out wipes the caches.
10. A new shell version replaces the old shell cache on activate.

Manual, before release: install on iPhone (Safari, Share, Add to Home Screen)
and on Android (Chrome install prompt); check standalone launch, safe areas
on a notched screen, light and dark, and offline reading in airplane mode.

## Known limits

- iOS may evict an installed web app's storage after weeks without use; the
  offline cache is best effort.
- An installed iOS web app has its own cookie store, so the user signs in
  once inside it (passkeys make that quick).
- A lost, unlocked phone shows cached concepts until it next reaches the
  server.
