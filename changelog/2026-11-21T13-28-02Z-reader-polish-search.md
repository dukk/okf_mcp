# Reader: search

## Changed
- Each hit shows a document icon, its title, its folder and a plain-text
  snippet (the server's `snippet_text`).
- A page that fills the limit offers "Show more", which reads again with 50
  more (up to the server's 200, then a hint to refine the search) and draws
  that answer in place of the list, keeping the scroll position, so a
  document published between the reads is neither repeated nor skipped. The
  count reads "50+ results" rather than a flat "50 hits".

## Fixed
- Snippets showed raw markdown (`**`, backticks, link syntax).
- Search stopped silently at 50 hits.
- A successful "Show more" after an offline attempt left "More results need a connection." under the full list; the notice is removed on success.

## Tests
- Journeys: 60 matching notes show 50 with "50+ results", then all 60 after
  Show more, each once, with no markdown in the snippets; a new top match
  published between the reads shows first, with no hit repeated or skipped.
- Journeys: an offline Show more keeps the rows and re-enables the button, and the retry after reconnecting loads all 60 and clears the notice; a refused Show more (unlock required) replaces the results with the refusal screen; with 210 matches Show more stops at "200+ results" with the refine hint.
