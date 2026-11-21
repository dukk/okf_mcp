# Reader: header, tab icons and scroll position

## Added
- The header shows the current catalog's label (not its name) after a mark
  with its first letter, opens the picker, and has a search icon; it gets a
  hairline once the page scrolls. The bottom tabs have icons.
- `parts.js` gains the shared parts the restyled screens use (`row`,
  `sectionHead`, `pill`, `pageTitle`, `lede`; breadcrumbs with the catalog
  label and folder titles; the serif body), and `icons.js` draws icons from
  the sprite.
- `main.js` reads the catalog list once per page load, for labels and page
  counts, without blocking any screen: when it answers, the header and every
  breadcrumb drawn meanwhile (marked `data-catalog-label`) take the label.

## Fixed
- The scroll position carried over between screens: every screen change now
  starts at the top.

## Tests
- Journeys: the header shows the catalog label and opens the picker; a tab
  change after scrolling starts at the top; with the catalog list held in
  the worker, the tree draws with the catalog's name in its breadcrumb,
  which takes the label once the list answers. A unit test checks that every
  icon the reader names is in the sprite.
