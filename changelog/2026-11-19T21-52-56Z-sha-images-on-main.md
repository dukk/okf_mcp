## Added
- `.github/workflows/image-cleanup.yml`: a daily job (04:17 UTC) using `dataaxiom/ghcr-cleanup-action` v1.2.2 (pinned by commit) that deletes `sha-*` tags older than 30 days from `ghcr.io/hypotheosis/okf-mcp` and `ghcr.io/hypotheosis/okf-maintenance`, along with untagged images of the same age. Images with a semver tag (`*.*.*`) are kept indefinitely, with their sha tag, platform manifests and attestations. A manual run defaults to a dry run.

## Changed
- `image.yml` and `maintenance-image.yml` now also run on every push to `main`, so both images exist for each commit on main as `sha-<full 40-character commit sha>`. `image.yml` switched from the short sha to the long one. `v*` tag pushes still add the version tag, and there is still no `:latest` (Decision 13).
- `maintenance-image.yml` now writes its sha tag on every run (before, only on `workflow_dispatch`). Its `org.opencontainers.image.version` label is the tag name only on a `v*` tag push, and the commit sha otherwise (before, a branch push would have labelled it `main`).

## Notes
- A release tag push rebuilds the commit main already built. The `sha-` tag moves to the versioned image, and the earlier image becomes untagged and ages out.
- Deleting with `GITHUB_TOKEN` requires the repository to have Admin access to both packages (package settings, "Manage Actions access").
