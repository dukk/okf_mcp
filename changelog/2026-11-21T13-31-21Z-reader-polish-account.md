# Reader: Account, the catalog picker and a theme choice

## Added
- Account has an Appearance choice: System (the default), Light or Dark. It
  sets the page's theme and the system bars' colour, applies before the
  first paint (`js/theme.js`), and stays on the device across sign-outs.

## Changed
- Account restyled: the user with an avatar, the email, when the session
  ends, Sign out and Delete offline copies, as before.
- The catalog picker lists each catalog with its mark, label, description
  and page count. A row holds no `.meta` span, so the whole row is clickable.

## Tests
- Journey: a theme choice overrides the system, survives a reload, and
  System gives it back.
