# Issue #361: edit a suspended team's notice from the console

## Added
- The Teams drawer's Suspension section, for a suspended team, has a notice textarea prefilled with the current notice and a "Save notice" button. It sends `PUT /api/v1/teams/{name}/suspension` with `{suspended: true, notice}`, so the team stays suspended with its original `suspended_at` and its members stay refused, as with the CLI's `team suspend --notice` on a suspended team (`src/http/ui/js/pages/teams.js`).
- The controls use the same pending-write key and the same `write` path as Suspend and Lift, so they are disabled while any suspension write runs (also in a reopened drawer), a refusal's reason shows, and a stale answer never draws over a newer one.
- Journey `a_suspended_teams_notice_is_edited_without_lifting_the_suspension` (`tests/ui_e2e/teams.rs`): the textarea is prefilled, saving changes the stored notice, `suspended_at` is unchanged, Lift is still offered, and a member's browse page is still refused, showing the new notice.
