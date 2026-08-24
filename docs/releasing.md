# Releasing

Versions, the changelog, tags, GitHub Releases, the `ghcr.io` image, and both
crates.io publishes are driven by [release-plz](https://release-plz.dev/)
from conventional-commit history. There is no manual version bump after the
one-time bootstrap below.

## One-time bootstrap

Trusted Publishing cannot create a crate on crates.io, and its OIDC exchange
fails with an opaque 400 if no publisher is registered yet. These steps are
load-bearing in this order:

1. Land the release-automation changes on `main`; confirm CI (including the
   `package` job) is green.
2. **Manually publish `0.9.0` from a clean checkout of that commit**, using a
   personal crates.io API token:
   ```sh
   cargo publish -p redmine-client --locked
   cargo publish -p ruprogress-mcp --locked
   ```
   This is the only step that ever needs a long-lived token — it is not
   stored anywhere in this repo.
3. On crates.io, add **four** trusted-publisher configs (2 crates × 2
   workflow filenames), each with owner `mimi1vx`, repo `ruprogress-mcp`,
   environment `release`:
   - `release.yml` (the hand-pushed-tag / `workflow_dispatch` path)
   - `release-plz.yml` (the automated path — crates.io matches the *calling*
     workflow's `workflow_ref`, not `job_workflow_ref`, so a `workflow_call`
     from `release-plz.yml` presents that filename, not `release.yml`)

   Registering only `release.yml` leaves the automated path broken in a way
   that only shows up on the *second* release.
4. In the GitHub repo settings: create a `release` environment, and enable
   *Settings → Actions → General → Allow GitHub Actions to create and approve
   pull requests* (release-plz's PR job needs it).
5. Push the `v0.9.0` tag by hand. `release.yml` fires on `push: tags`, builds
   the Linux x86_64/aarch64 tarballs, creates the GitHub Release, pushes the
   multi-arch `ghcr.io` image, and its `publish` job no-ops on
   `already uploaded` — exercising that retry tolerance for real on day one.
6. Set the `ghcr.io/mimi1vx/ruprogress-mcp` package visibility to public and
   link it to the repository.
7. From the next merge to `main`, release-plz owns versions, changelog, tag,
   and release — see the steady-state flow below.

## Steady state

1. Merge conventional-commit PRs (`fix:`, `feat:`, `feat!:`/`BREAKING CHANGE:`,
   …) to `main` as usual.
2. `release-plz.yml`'s `release-plz-pr` job opens or updates a release PR
   proposing the next version and its `CHANGELOG.md` entry.
3. Merging that PR runs `release-plz-release`, which tags `v<x.y.z>` and
   creates the GitHub Release (`git_tag_enable`/`git_release_enable` are
   pinned to the `ruprogress-mcp` package in `release-plz.toml`, so exactly
   one tag is created even though this is a two-crate workspace).
4. That job's `publish` step calls `release.yml` with the new tag, which
   builds the tarballs, uploads them to the release, builds and pushes the
   multi-arch image, and publishes both crates to crates.io via OIDC — no
   token, no manual step.

## Retrying a failed release

- **A `release.yml` job failed (build, assets, container, or publish):**
  re-run it with `workflow_dispatch`, passing the existing tag
  (`ref: v<x.y.z>`). Every job is idempotent: `assets` reuses an existing
  GitHub Release, `container`/`container-manifest` overwrite the same tag,
  and `publish` treats crates.io's `already uploaded`/`already exists` as
  success.
- **The tag or GitHub Release never got created:** push it by hand
  (`git tag v<x.y.z> && git push origin v<x.y.z>`); the `push: tags` trigger
  picks it up.
- **crates.io accepted `redmine-client` but the job died before
  `ruprogress-mcp`:** just re-run `publish` (via `workflow_dispatch` on
  `release.yml` or a re-run of the failed run) — the per-crate `-p` publish
  calls are independently retry-safe.
