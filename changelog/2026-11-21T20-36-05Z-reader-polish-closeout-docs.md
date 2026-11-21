# Reader polish: close-out doc fixes

## Docs
- `docs/superpowers/specs/2026-11-21-reader-polish-design.md`: Jump back in
  shows "Synced N ago", most recently viewed first, as built (issue #867).
  The header points to the plan's "Changes from the spec", its Corrected
  notes and the milestone changelog's "Changed from the plan".
- `docs/operations.md`: the console CSP line has `font-src 'self'
  https://fonts.gstatic.com`, as `src/http/ui.rs` sends since #863, and says
  why (the reader serves its own fonts).
