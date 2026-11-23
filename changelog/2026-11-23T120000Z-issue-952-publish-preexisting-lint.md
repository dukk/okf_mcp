# Issue 952: publish no longer refuses lint errors the published file already has

Date: 2026-11-23

## Problem

Since 0.28.0 angle-bracket links such as `[x](<a b.md>)` are links. A `log.md`
written before that, holding such text, now has a `broken-link` error. Every
`log` call changes `log.md`, so every logged publish was refused with
`LintFailed`, and `log.md` cannot be edited through the tools.

## Change

`split_lint` (`src/store/publish.rs`) now compares each error in a changed file
with the errors of the published catalog (the snapshot the changeset is
applied onto). `demote_preexisting` matches on file, rule and message, never
line number, and by count: each published finding excuses at most one finding of
the result. So one old broken link plus one new identical one is one new error,
which still refuses. An excused finding is returned as a warning whose message
ends with "(already in the published file)".

The published catalog is linted only when a changed file has an error, so the
common publish costs nothing extra. The rule applies to every path through
`publish::check`: the MCP `publish` tool and the HTTP changesets API.

## Not done

- A repair operation for `log.md` (issue proposal 3).
- Having `log` escape link-looking text (issue proposal 4).
- `find_links` treating link text inside code spans as links (separate bug).
