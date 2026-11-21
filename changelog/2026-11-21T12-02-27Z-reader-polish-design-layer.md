# Reader: fonts, icons and design tokens

## Added
- Self-hosted Latin-subset woff2 fonts under the SIL Open Font License,
  with their license files: Inter (interface, weights 400 to 700) and
  Literata (concept bodies, roman and italic, 400 to 600), built by
  `scripts/subset-reader-fonts.sh` from the upstream release files
  (Inter 4.1 and Literata 3.103). Input SHA-256:
  - `InterVariable.ttf` 4989b125924991b90d05b2d16e0e388c48f7d5bb8b30539bbf9c755278d0ccaf
  - `Literata[opsz,wght].ttf` b41138c9373112f32abb589cc22e8674b06ed4048b0c513be922bdd26f274440
  - `Literata-Italic[opsz,wght].ttf` d483dfaeba9cbf4ce71d32a52ee65df82f7e35b15fff8d1011cdb242d1fcd465

  Outputs are recorded in `scripts/reader-fonts.sha256`: 32720, 33384 and
  35104 bytes, about 98 KB in all, precached.
- `src/http/read/icons.svg`, the reader's icon sprite.
- `read.css` rewritten: Notion-like tokens for light, dark and an explicit
  `data-theme`, Pico's variables mapped onto them, and the component classes
  the screens use (rows, sections, pills, avatars, chips, timeline groups,
  the outline, the serif body).

## Fixed
- The system navigation bar was the old green accent: the manifest's colours
  are neutral now, and the page's `theme-color` metas give `#ffffff` in light
  and `#191919` in dark.

## Changed
- `.txt` files are served as `text/plain`.
- The font script reads Literata's own release layout
  (`fonts/variable/Literata[opsz,wght].ttf`), not a Google Fonts download.

## Tests
- The shell listing precaches the fonts, the sprite and the licenses; the
  theme-color metas and the manifest colours.
