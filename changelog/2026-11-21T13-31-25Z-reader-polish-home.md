# Reader: Home with Jump back in and an activity timeline

## Changed
- Home's search box counts the catalog's pages ("Search 1,284 pages").
- "Jump back in" lists the three concepts read last (from the offline
  copies), each with its folder, when it was read and an offline mark.
- "Recent changes" became an activity timeline: filter chips (All, Created,
  Edited, Deleted, By me), day headings, and groups by actor and batch (the
  same actor and message, less than 2 minutes apart) with an avatar,
  "Name imported 6 pages", the time, and up to three documents with a pill,
  then "+N more". Deletions are struck through and not links. "Show older"
  reads the next page. Titles replace paths.
- Offline, or from the worker's copy, every offline copy is listed under
  "Available offline", as before.
- Home reads 50 changes a page (was 20).
- No `.meta` span sits inside a link on Home any more (it made the link
  unclickable at its centre).

## Tests
- Journeys: an import is one group that expands; a chip filters; Show
  older reads the next page; Jump back in lists the concept read last; the
  search box counts pages. Updated: the journeys that read `#recent`.

## Fixed (review of the PR)
- "Show older" removes its "needs a connection" notice once a later page
  read works.
- Journeys for its failure paths: an unavailable read keeps the rows and
  re-enables the button, and a retry clears the notice; a lost grant evicts
  the catalog; a 401 signs out; a late answer after leaving changes nothing.
