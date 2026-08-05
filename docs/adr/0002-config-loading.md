# ADR 0002: Config loading — figment vs. hand-rolled

## Context

The parent plan (`plans/redmine-mcp-rust.md`) assumes `figment` for layered
env + `.env` configuration. The actual env-var namespace is non-uniform
(`REDMINE_*`, `FASTMCP_*`, `ATTACHMENTS_DIR`, `CLEANUP_INTERVAL_MINUTES`), and
most of the complexity is in **cross-field validation** (e.g. rejecting
`legacy-per-user` without `REDMINE_PER_USER_TRUST_PROXY`), which figment does
not help with — that logic has to be hand-written regardless.

## Options

- Figment: layered sources (env, `.env`, defaults), but no built-in
  cross-field validation.
- `dotenvy` + hand-rolled typed getters: load `.env` into the process
  environment, then a validating constructor builds the `AuthMode` enum from
  `std::env::var`.

## Decision

**Drop figment; use `dotenvy` + hand-rolled typed getters.** All the value in
this config surface is in the validating constructor, not in the source
layering. `dotenvy` is already in the workspace dependency table for this
purpose. Decided before phase 2 so the dependency table doesn't carry an
unused `figment` entry.
