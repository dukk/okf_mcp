# Console tables draw only the newest list read

## Added
- `core/latest.js`: `latest(view)` numbers one list's reads; the returned `current()` is true only while no newer read has started and the page is still shown.
- Browser journeys for the users, teams and catalogs tables: an older list read is held, a newer write's read answers first, the older answer is released, and the table must still show the newer data.
- `Ui::present`, a no-wait check that an element is in the page.

## Changed
- The users and catalogs tables' `load()` draw only the newest answer and nothing once the page was left (they drew in arrival order). The teams table, which already numbered its reads, and the settings page's two lists now use the same helper.

## Notes
- Issue #351. Other pages with an unguarded list read (changesets, my-grants, my-keys) are left for follow-up.
