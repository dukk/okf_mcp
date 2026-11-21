# Mobile reader polish: design

Date: 2026-11-21. Status: approved in brainstorming; it follows the mobile
reader milestone (`2026-11-20-mobile-reader-pwa-design.md`, epic #807).

Visual reference: [`assets/2026-11-21-reader-polish-mockups.html`](assets/2026-11-21-reader-polish-mockups.html).
It mocks three directions on Home, Folder and Concept, with placeholder content.
Open it in a phone-sized viewport and use **Views** to switch between them.

## Goal

The reader works, but it looks spartan and reads like raw data. It shows
paths instead of titles, slugs instead of labels, one row per revision, and
default type and spacing. This milestone gives it a clean, Notion-like design.
It also fixes six functional bugs found while using it on a phone as a heavy
user would.

The three uses it must serve are all real:
- **A. Look things up:** arrive with a question, search, read one concept.
- **B. Browse and read:** wander folders, follow links, read long pieces.
- **C. Check what changed:** recent activity in the catalog.

## Decisions

| Question | Decision |
|---|---|
| Direction | The mockups' **Pages** direction (Notion-like) everywhere. Home uses the **Activity** direction's timeline. Concept bodies use the **Reader** direction's serif type and summary lede. |
| Reading progress | None. No "minutes left", no progress bars, no scroll tracking. |
| "Jump back in" | Kept on Home: up to 3 recently read concepts from the offline cache, each marked when it's readable offline. |
| Type | Inter for the interface, Literata for concept bodies. Both are self-hosted as Latin-subset `woff2`, SIL Open Font License, and precached so they work offline. |
| Icons | One inline SVG sprite, used with `<use>`. No icon font, no emoji. |
| Base CSS | Pico CSS remains the base layer (project rule). The look comes from the reader's own tokens and component classes in `read.css`. |
| Heavy work | Stays in Rust. The server adds titles, descriptions and plain snippets. The page only does presentational grouping. |
| Console | Unchanged. |

## Screens

### Chrome (every screen)

- **Header:** a small catalog mark (the catalog label's first letter on the accent colour), then the catalog **label** (not the slug), then a chevron. Tapping it opens the catalog picker. A search icon sits on the right. The header is sticky and gets a hairline border once the page scrolls.
- **Bottom tabs:** an icon and a label each (Home, Browse, Search, Account). The active tab is shown by stronger text and icon colour.
- **Scroll position:** every screen change starts at the top.
- **System bars:** the manifest and `theme-color` meta tags give a light and a dark colour, so Android's navigation bar matches the theme.

### Home

1. **Search box,** with the catalog's page count ("Search 1,284 pages"). Tapping it opens Search.
2. **Jump back in:** up to 3 rows (icon, title, folder and "read 2 h ago", an offline mark). It is taken from the reader's offline cache, newest first, and hidden when the cache is empty.
3. **Activity:**
   - **Filter chips:** All, Created, Edited, Deleted, By me.
   - **Day headings:** Today, Yesterday, then dates.
   - **Groups:** under each day, changes are grouped by actor, then by batch. A batch is changes with the same actor and the same message less than 2 minutes apart. Each group shows an avatar (initials, with a colour derived from a hash of the actor), "**Name** imported 6 pages" or "**Name** edited Kubernetes cluster", and a time.
   - **Group contents:** a group of more than one document lists up to 3 entries (title, folder path, a pill for new, edited or deleted), then "+N more", which expands the group.
   - **Single changes:** a group of one is a single row.
   - **Deletions:** struck through and not links.
   - **Paging:** "Show older" at the end fetches the next page (`after`).

The previous "Recent changes" list and "Available offline" list are replaced. The offline list stays reachable when offline: Home shows "Jump back in" expanded to every offline copy, under an "Available offline" heading.

### Folder (Browse)

- **Top:** breadcrumbs (catalog label, then folder titles), the folder title (its index's title, or the folder name), and the index's description as a lede.
- **About:** a row that opens the folder's `index.md` body. It no longer sits above the lists, so the folder's own content isn't repeated.
- **Folders:** a folder icon, the title, a one-line description, the page count and a chevron.
- **Pages · N:** a document icon, the title, and a one-line description (ellipsised). `log.md` appears as a final "Log" row.

### Concept

- **Top:** breadcrumbs, then the title. Below it, a meta row: the `type` as a pill, up to 3 `tags` as pills, "Edited 1 h ago by Name", and the reading time.
- **Lede:** the frontmatter `description` in larger muted serif text, when present.
- **On this page:** collapsible, listing every heading except the page title, whatever its level, and linking to `?s=<slug>`. It shows the section count.
- **Body:** Literata at about 17.5 px with a line height of 1.7, and headings in Inter. Inline code is a small mono chip that doesn't change the line height. Blockquotes become a soft callout. Links are underlined in the text colour, not default blue.
- **Long concepts:** the reader always asks for the whole body (`?full=true`), so there is no "Show the whole concept" step. The 20k-character outline limit is meant for MCP agents.
- **Linked from · N:** one row per linking document, with "N links" when it links more than once. Logs show as "Log · N entries".

### Search

- **Results:** a document icon, the title, the folder path in small print, and a plain-text snippet with no markdown syntax.
- **More results:** when a page fills the limit, "Show more" fetches the next page. The count reads "50+ results" instead of a hard "50 hits".

### Account

The same functions as today, restyled: user, email, session end, Sign out, Delete offline copies.

## Server changes

All are additive, so existing clients are unaffected.

- **`GET /catalogs/{c}/recent-changes`:** each change gains `title` (string or null) and `description` (string or null). Both are read from the catalog snapshot at request time, and are null for a deleted or missing document.
- **`GET /catalogs/{c}/tree`:** each document gains `description` (string or null). The reader derives folder page counts from the paths.
- **`GET /search`:** each hit gains `snippet_text`, the snippet with markdown syntax removed, by the same renderer the console uses, reduced to text. `snippet` is unchanged.
- **`GET /catalogs`:** already has `concepts` (the page count) and `label`. No change.
- **Reader CSP:** `font-src` adds `'self'`. The console's CSP is unchanged unless it is shared, in which case `'self'` is harmless.
- **Shell:** the fonts and the icon sprite are reader files, so they are hashed in `shell.json` and precached.

## Bugs fixed

1. **Scroll position:** it isn't reset between screens. `main.js`'s `show()` scrolls to the top.
2. **Duplicate backlinks:** the list repeats a document once per link. It is now deduplicated by path, with counts.
3. **Empty outline:** `concept.js` keeps only headings with `level >= 2`, so concepts made of level-1 sections show nothing. It now drops only the page title.
4. **Green navigation bar:** the manifest has one theme colour. It now gets per-scheme `theme-color` meta tags and a neutral manifest colour.
5. **Search:** raw markdown in snippets, and a silent cap at 50. Fixed by `snippet_text` and "Show more".
6. **Long concepts:** they stop behind a button. The reader now always reads the whole body.

## Element ids

Ids the browser journeys use keep their meaning: `#recent`, `#folders`, `#concepts`, `#backlinks`, `#outline`, `#body`, `#search-form`, `#search-input`, `#hits`, `#offline-list`, `#concept-title`, `#document-title`, `#catalog-list`, `#account-user`, `#sign-out`, `#delete-offline` and the notice ids.

`#recent` becomes the activity timeline's container. Journeys that read its links still find the document links in it. Where a journey depended on markup that changes, such as "Show the whole concept", the journey is updated in the same task that changes the markup.

## Testing

- **Rust unit tests:**
  - titles and descriptions in recent changes, and null for a deleted document;
  - descriptions in the tree;
  - `snippet_text` has no markdown syntax;
  - the reader CSP's `font-src` includes `'self'`;
  - `shell.json` lists the fonts and the sprite with their hashes.
- **Node tests** (`tests/read_worker/`):
  - timeline grouping: a 6-document import becomes one group; different actors or messages split; a gap of over 2 minutes splits; day boundaries split; each filter chip works;
  - outline: only the page title is dropped, whatever the levels;
  - backlink deduplication and counts;
  - avatar initials and colour are stable for the same actor.
- **Browser journeys:**
  - the existing ones still pass;
  - new ones check:
    - scroll resets on tab change;
    - a concept with only level-1 sections has an outline;
    - a document that links three times shows once, with a count;
    - an import shows as one group that expands;
    - search's "Show more" goes past 50;
    - snippets contain no `**` or backticks;
    - a long concept renders whole without a button;
    - the header shows the catalog label.
- **Visual check:** for each screen, phone screenshots in both themes are compared with the mockup. A before/after page is reviewed before the milestone merges.

## Out of scope

Editing, notifications, reading progress, the console's look, and offline search.
