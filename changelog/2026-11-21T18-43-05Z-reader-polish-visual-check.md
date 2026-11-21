# Reader: the visual check

## Added
- `tests/ui_e2e/read.rs` `serve_a_sample_catalog_for_the_visual_check`, an
  ignored test that serves a sample catalog (placeholder content shaped like
  the mockup's) so the reader can be opened on a phone through
  `adb reverse`.

## Changed
- Home: the search box sits directly under the header, as in the mockup
  (it had an extra 8px margin).
- Folder and Concept: the breadcrumbs are plain text. Pico's `nav` rules gave
  the list items padding and the lists negative margins, so the crumbs were
  spaced wide and sat low under the header.
- Concept: the body (an `article`) is no longer drawn as a Pico card, the
  "On this page" list opens without the gap Pico puts under an open summary
  and without the list item margins, a focused summary no longer turns blue,
  and the meta row's separator trails its fact instead of leading the next
  line when the row wraps.
- The sample's Kubernetes description is quoted: unquoted, its colon made the
  front matter invalid YAML (the plan's sample had the same flaw and is
  corrected).

## Docs
- Each screen was compared with the mockup on a phone in light and dark (Home
  with the Activity direction, the concept with the Reader direction), and a
  before/after page went to review. Differences left as design questions are
  listed in the pull request.
