# Reader: the concept screen

## Changed
- Below the title, a meta row: the `type` and up to three `tags` as pills,
  the reading time, and "Edited 1 h ago by Name" (from the history route;
  left out offline). The frontmatter `description` is a serif lede. The
  edit line fills in when the history answers; the concept never waits for it.
- "On this page" lists every heading but the page title, with the section
  count, whatever the levels.
- The body is set in the serif reading type. The title heading is not
  repeated in the body.
- "Linked from · N" lists each linking document once, with "N links"
  when it links more than once; a log shows as "Log" with its entries.

## Fixed
- A concept made of level-1 sections had an empty outline: only a level-1
  first heading that reads as the frontmatter title is dropped now (from the
  outline and the body); an untitled concept keeps every heading.
- A document that linked several times was listed once per link.
- A long concept stopped behind "Show the whole concept": the reader always
  reads the whole body (`?full=true`). The button and its notices are gone.

## Tests
- Journeys: level-1 sections and the meta row; the concept draws while its
  history is held, and the edit line follows; an untitled concept keeps
  `# One` and `# Two` in its body and its outline; three links shown once
  with a count; a long concept whole without a button, offline too, and
  after a lost grant; a late answer leaves a newer screen alone. The
  journeys that held or failed the concept read by URL now name
  `?full=true`.
