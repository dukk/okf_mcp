# okf-mcp display names: design

Status: delivered in milestone 17 (epic #597), 2026-11-17. The provisioner follow-up is tracked in
https://github.com/hypotheosis/telekin-platform/issues/111 (Part 5, Rollout step 2).

## Goal

Users, teams and catalogs each have a slug `name` (`^[a-z0-9-]+$`,
unique). An upstream signup provisioner now generates these slugs at
random (for example `u-3f9a0c1d2e4b5a69` for a user and its personal
team, `m-7b1e…` for a catalog), so that signups never collide and the
slug says nothing about the person. That is the right choice for an
identifier, but today the server shows the slug almost everywhere as if
it were a name a person would recognise: emails ("Hello u-3f9a…"),
consent and sign-in pages, the passkey picker, the console header and
tables, a new catalog's `index.md` heading, export filenames, and
attribution.

This design keeps the slug as a permanent, opaque id and adds a human
**label** that people can set and edit. Everything a person reads shows
the label; the slug remains wherever an exact value is needed (API
paths, tool parameters, client configuration) and appears to people only
as a small secondary "ID" line.

## Decisions

1. **Slugs are permanent and opaque.** No rename of `name` is added for
   users, teams or catalogs. Nothing that references a slug (the
   provisioner's own records, billing metadata, client configs, hooks,
   audit) can break.
2. **Labels fall back to something human, never the slug.**
3. **Who edits:** users edit their own display name; team owners edit
   their team's and their team's catalogs' display names; admins and the
   provisioner edit any. A personal team has no label of its own.
4. **Stored attribution stays ids; labels are resolved at display time.**
5. **MCP inputs take the slug only; outputs carry the label beside it.**
6. **Scope is this repository.** The provisioner's changes are a
   follow-up issue in its own repository, depending on the release that
   ships this design.

## Part 1: Data model and the label rule

### Schema (migration `0019_display_names.sql`)

- `ALTER TABLE catalogs ADD COLUMN display_name text NOT NULL DEFAULT ''`.
  Users and teams already have `display_name`.
- Normalise existing `users.display_name` and `teams.display_name`:
  trim, replace control characters and line breaks with a space, cut to
  80 characters.
- Add `CHECK (char_length(display_name) <= 80)` to `users`, `teams` and
  `catalogs`.
- No other data is written. The migration is additive, so it is safe
  under multi-replica rolling deploys (an old replica ignores the new
  column).

Slug columns, constraints and uniqueness are unchanged. A personal
team's `display_name` column stays but is never read for its label.

### `DisplayName` newtype

One newtype in a shared module, used for every write path (create,
update, CLI, provisioner):

- input is trimmed;
- at most 80 characters (`char_length`, i.e. Unicode scalar values);
- no control characters and no line breaks (labels are rendered into
  email subjects and HTML);
- the empty string means "unset";
- not unique: two catalogs or users may share a label.

Invalid input is a 400 (`OkfError` validation variant) naming the rule
broken.

### The label rule

One Rust function per kind computes the label. No surface reimplements
the fallback, including the console JS.

| Kind | Label |
|---|---|
| User | `display_name`, else the local part of the email, else "Unnamed user" |
| Personal team | "<user label>'s team" |
| Shared team | `display_name`, else "Unnamed team" |
| Catalog | `display_name`, else `description`, else "Untitled catalog" |

Every API view of a user, team or catalog (`UserView`, `TeamView`,
`CatalogSummary`, `/me`, `/whoami`, and the nested references inside
them such as a team's members or a user's teams) gains a computed
`label` field beside `name` and `display_name`. `name` stays the slug.

When two catalogs reachable by the same person share a label, surfaces
that list them together (the console catalog selector, the consent
page) disambiguate with the owner team's label: "Personal memory (Acme
team)". Any text that still occurs twice in the final list (two catalogs
of one team, owner teams that share a label, or a display name that
happens to look like a generated suffix) switches to its label plus its id,
"Notes (id: m-7b1e…)", until every choice shown is distinct. The
disambiguation is computed server-side alongside the listing.

## Part 2: Editing endpoints and permissions

| Route | Body | Allowed callers |
|---|---|---|
| `PATCH /api/v1/me` (new) | `{display_name}` | the signed-in user, for themselves |
| `PATCH /api/v1/users/{name}` (new) | `{display_name}` | admin, provisioner |
| `PATCH /api/v1/teams/{name}` (new) | `{display_name}` | an owner of that team, admin, provisioner; refused (409) for a personal team |
| `PATCH /api/v1/catalogs/{catalog}` (exists) | `{display_name?, description?}`, at least one | admin on the catalog (as today), an owner of the catalog's owner team, provisioner |
| `POST /api/v1/catalogs` (exists) | gains optional `display_name` | unchanged |

Every route returns the updated view (with `label`) and writes an audit
entry through the existing audit machinery, recording the new value's
length in characters (`{"length": n}`), never the text, as
`catalog.description_changed` does: a display name is personal data
("People in entries").

> **Corrected (issue #600).** This read "recording the old and new
> value"; plan Decision 4 records only the length.

**Team-owner check.** Session ceilings stop at `write`, so a team owner
never holds `Access::Admin` on a catalog. The catalog PATCH therefore
accepts either `principal.require(Access::Admin, catalog)` or a new store
check, "the principal's user is an `owner` in `team_members` of the
catalog's `owner_team_id`" (one indexed query). Owners are not raised to
admin; the check unlocks only this route. A caller who cannot reach the
catalog at all gets 404 as today, so the check reveals nothing about
catalogs outside the caller's reach.

**Provisioner.** The three PATCH routes on users, teams and catalogs
join `PROVISIONER_ROUTES`. The provisioner can then set or correct
labels after signup.

**CLI.** `user update <name> --display-name <text>`, `team update <name>
--display-name <text>`, and `catalog update <name> --display-name <text>`
(beside the existing `--description`).

**Propagation.** A label change bumps the same generation the
description update bumps today, so `list_catalogs` and the console show
it at once on every replica. Labels never affect keys, grants,
selection headers or tool parameters.

## Part 3: What people see

Wherever a slug was shown as a name, the label replaces it and the slug
moves to a muted secondary "ID: m-7b1e…" line or tooltip.

### Web console

- **Header:** `#whoami` shows the user label. `#catalog-select` option
  text is the (disambiguated) catalog label; the option value stays the
  slug.
- **Account page:** an editable "Display name" field saving through
  `PATCH /me`; the slug becomes an "Account ID" line; the teams list and
  ceiling table show labels. A "Teams you own" section lets a team owner
  rename each shared team they own (the Teams page stays admin-only).
- **Users, teams, catalogs pages:** the first column is the label with
  the slug as a secondary line in the same cell; drawer titles use the
  label; edit drawers gain a display-name field shown only to callers
  allowed to edit (Part 2). The catalog drawer's save is available to
  team owners, not only admins.
- **Delete confirmation** still requires typing the slug (it is shown in
  the dialog for copying); the friction is deliberate.
- **Other pages** (browse, suspended view, health, runs, changesets,
  transfer, unlocks, keys, my-keys, my-grants, audit) show labels for
  catalogs, teams, actors and people.
- **Audit page:** people are shown through the label rule applied to the
  `audit_people.display_name` snapshot (stored today but never read).
  Deleting a user blanks that snapshot (existing anonymisation), so a
  deleted user's entries show "Unnamed user", never the slug or their old
  name.
- **URLs** (`#/browse/<slug>/…`, REST paths) keep the slug.

### Emails

- Setup: "Hello <user label>,".
- Unlocked: subject and body name the catalog label.

### Sign-in, consent, passkeys

- "Signed in as <user label>", "Welcome, <user label>".
- Consent checkboxes, level selects and the default-catalog select are
  labelled with the catalog label; element ids and form values keep the
  slug.
- The sign-in-code summary names catalogs and the user by label.
- WebAuthn `user.displayName` becomes the user label; `user.name` stays
  email, else slug.

### Catalog content and exports

- A new catalog's root `index.md` heading is `# <display name>`, falling
  back to `# Catalog` when the catalog has no display name. Existing
  catalogs keep their heading: content is never rewritten after the fact.

  > **Corrected (issue #606).** The first draft used the catalog label,
  > which falls back to the description. The description is already the
  > paragraph beneath the heading, so the heading would have repeated it.
- The export filename becomes `<slugified label>-<slug>.tar.gz`, still
  unique and now recognisable.

### Deliberately still slugs

CLI list output (operators), logs and traces, and the `X-OKF-Catalogs`
header / `?catalogs=` value in client configuration, which people copy
from setup instructions rather than read.

## Part 4: MCP surface and attribution

### Tool outputs

Wherever a tool returns a catalog, `catalog` stays the slug and
`catalog_label` is added beside it: `get_concept`, `history`,
`open_changeset`, `list_changesets`, search hits (including skipped
catalogs) and the report outputs. `list_catalogs` entries gain
`display_name` and `label`; `suspended.team` gains `team_label`.

### Tool inputs

Unchanged: `catalog`, `catalogs` and the required `catalog` on
`maintenance_*` take the slug only. Parameter descriptions change from
"catalog name" to "catalog id (the `name` from `list_catalogs`)".

### Server instructions

A paragraph is added: catalogs, users and teams have an opaque id and a
human label; use the label when talking to people, pass the id in tool
calls, and never ask a person to type an id.

### Error messages

Errors about a catalog the caller can reach name both, for example
`catalog "Work notes" (m-7b1e…) is unavailable: its team "Acme" is
suspended`. Errors for a catalog that does not exist or is not
reachable echo only the value the caller sent, so they confirm nothing.

### Attribution

- Stored values are unchanged: `documents.updated_by`, revision and
  changeset `actor`, and `server_settings.updated_by` keep what they
  store today.
- At display time, `history`, `list_changesets` and the console resolve
  an actor to a label and add an `actor_label` field. A session actor is
  a user slug and resolves to that user's label; key actors (a key name
  or free text) and OAuth actors (a client name) are already human text
  and pass through.
- Free-text stamps inside markdown (`by: …`, `human:<owner>`) are left
  alone. Skills that write `human:<owner>` keep using the owner's slug,
  which is stable.

### Skills and docs

`skills/*/SKILL.md` gain the "say the label, pass the id" guidance. The
README, `docs/operations.md` and the generated `docs/reference/tools.md`
are reworded and regenerated.

## Part 5: Rollout, testing, follow-ups

### Rollout

1. Release and deploy this server change.
2. The provisioner follow-up (its own repository's issue):
   - send `display_name` when it creates a catalog: "<checkout name>'s
     memory", or "Personal memory" when no name was collected;
   - show team and user labels on its billing-link page and in its
     shared-team emails, read from this server's views;
   - its records stay keyed by slug; nothing in its storage changes.

### Testing

- **Unit:** `DisplayName` validation (trim, 80-character limit counted
  in scalar values, control characters, line breaks, empty); a table per
  label function covering every fallback step and the personal-team
  case; catalog-label disambiguation.
- **Store and routes:** each PATCH allowed for the right callers (self,
  team owner, admin, provisioner) and refused for everyone else (403 /
  404 as specified); personal-team PATCH refused with 409; audit entry
  written; the catalog PATCH works for a team owner holding only write
  ceilings; `PROVISIONER_ROUTES` includes the new routes and nothing
  more.
- **Migration:** existing over-long or control-character display names
  are normalised; the `CHECK` holds.
- **MCP:** `list_catalogs` and the other outputs carry labels; error
  messages name label and id where allowed and are unchanged where
  existence must not leak.
- **Rendering:** email templates and the consent page show labels and
  still escape HTML; passkey options carry the label; a new catalog's
  `index.md` heading is the label.
- **Console:** header, account edit and owner-visible catalog edit, in
  the repository's existing UI test pattern.
- **Property tests:** the label is never empty; whenever a display name,
  email or description is set, the label is not the slug.

### Out of scope / follow-ups

- The provisioner follow-up above.
- A user-chosen, renamable alias for catalogs, considered and deferred:
  revisit only if typing ids proves painful.
- Rewriting headings of existing catalogs' `index.md`.
