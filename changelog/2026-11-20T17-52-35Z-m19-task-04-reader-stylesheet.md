# M19 Task 04: the reader's stylesheet

## Added
- `src/http/read/read.css`: the reader's own look on Pico CSS 2, with tokens on
  `:root`, light by default and dark from the system setting. It styles the
  top bar, offline banner, reading column, bottom tab bar, folder and concept
  lists, breadcrumbs, notices, hints, the outline, and the rendered markdown.

## Changed
- Nothing else. The shell listing now also names and hashes `/read/read.css`.

## Notes
- The class and id names were checked against the markup that tasks 5 to 9 of
  the plan create (`bar`, `banner`, `reader`, `tabs`, `list`, `row`, `meta`,
  `folder`, `crumbs`, `notice`, `hint`, `outline`, `level-N`, `markdown`,
  `data-slug`); none needed changing.
- `.screen` is created by the router but needs no rules of its own.
