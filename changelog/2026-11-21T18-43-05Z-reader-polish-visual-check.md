# Reader: the visual check

## Added
- `tests/ui_e2e/read.rs` `serve_a_sample_catalog_for_the_visual_check`, an
  ignored test that serves a sample catalog (placeholder content shaped like
  the mockup's) so the reader can be opened on a phone through
  `adb reverse`. It names `owner` as the actor of the setup's own changes
  (they were recorded under the operating-system login) and asserts the
  actors it shows.
- Journeys `a_concept_body_is_plain_text_not_a_card`,
  `the_outline_opens_without_picos_gaps_and_stays_muted_when_focused`,
  `the_breadcrumbs_have_no_pico_nav_spacing`,
  `the_home_search_box_has_no_margin` and
  `the_meta_separator_trails_a_fact_and_never_leads_one` (the dot trails each
  visible fact, none leads one, the last has none, hidden and wrapped): computed-style checks that fail with
  Pico's defaults.

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
