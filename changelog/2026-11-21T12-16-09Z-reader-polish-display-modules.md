# Reader: display modules for the timeline, outline, backlinks and avatars

## Added
- `src/http/read/js/text.js` (names from paths, page counts, reading time,
  short ages), `timeline.js` (recent changes by day, then batches by actor
  and message less than 2 minutes apart, and the filter chips),
  `outline.js` (every heading but the page title), `backlinks.js` (one row
  per linking document with its count) and `avatar.js` (initials and a
  colour class from a hash of the actor). Pure modules, used by the screens
  in later tasks.

## Tests
- `tests/read_worker/{text,timeline,outline,backlinks,avatar}.test.mjs`.
