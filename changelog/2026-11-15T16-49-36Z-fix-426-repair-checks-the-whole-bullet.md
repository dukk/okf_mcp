# Fix #426 review: repair checks the whole bullet, label included

## Fixed

- The previous round checked only a listing's link destination
  (`[](<href>)`). A label can break a link too. A folder named `a]` gets
  the heading `A]`, so its bullet `* [A]](a]/) - ...` doesn't parse, and a
  concept title ending in `\` escapes the closing `]`. Every repair
  appended such a bullet again. The repair now builds the exact bullet it
  would write (`folder_bullet`, `concept_bullet`) and lists the target only
  if that bullet resolves to it. Otherwise the note says `did not list
  /<path> in /<index>: no markdown link to it can be written`.
- `list_concept`'s bullet moved into `concept_bullet`, and
  `insert_folder_listing`'s into `folder_bullet` (replacing `folder_href`).
  The bullets written are unchanged: label escaping still matches `okf.py`.

## Tests

- `repair_indexes_checks_the_whole_bullet_label_included`: a folder `a]`,
  a title `C:\` (not listed) and a title `D [draft]` (escaped and listed);
  the second run writes nothing. It fails with the destination-only check.
