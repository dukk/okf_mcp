# Reader: installable before sign-in, named after the product (issue #900)

Time: see the file name (UTC).

## Why

- Signed out, `/read/` redirected in JavaScript to `/signin`, a page with no
  manifest, so Chrome reported the reader as not installable.
- The app was always "Catalog Reader" / "Reader", whatever the deployment.

## What changed

- A 401 (at boot or later) still deletes the offline copies and the
  remembered catalog, then shows the reader's own signed-out screen (`#screen-signed-out`)
  with a "Sign in" link to `/signin?return_to=/read/<hash>`. The page keeps its
  manifest and worker. Account sign-out still goes to `/signin`.
- `index.html` and the manifest carry a `__PRODUCT__` placeholder that
  `src/http/read.rs` fills per request from the settings' `product_name`
  (HTML-escaped for the page, JSON-escaped for the manifest): manifest name
  "<product> Reader", short name "<product>", tab title "<product> Reader",
  iOS title "<product>".
- The shell version is now the build's hash combined with the product name,
  and `shell.json` and `sw.js` are built per request, so a rename (settings
  can change at run time) changes the worker and installed copies update.
- `routes()` now takes the app state; `ui::content_type` is `pub(crate)`.

## Tests

- Unit: rendered manifest (default and custom name, JSON escaping), page
  (title, iOS title, HTML escaping, manifest link, no inline script), a
  rename changing the version and the listing hashes, and a run-time rename
  through the settings.
- Browser: signed out stays on `/read/` with the manifest linked and the
  worker active; "Sign in" returns to the same route; the journeys that
  asserted the redirect now assert the signed-out screen, including the 401 at
  boot deleting the copies.
