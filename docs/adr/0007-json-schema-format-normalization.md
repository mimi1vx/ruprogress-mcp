# ADR 0007: JSON Schema `format` normalization for tool schemas

## Context

`schemars` 1.2 emits `{"type":"integer","format":"uint64","minimum":0}` for
every `u64` field (`uint32` for `u32`), and rmcp hands that schema to clients
verbatim as `inputSchema`/`outputSchema`. `uint*` (and `int128`) are
schemars/Rust-specific — they are not in the JSON Schema `format`
vocabulary, nor in the OpenAPI-derived subset `ajv-formats` recognizes.

opencode compiles every tool's schema with Ajv in strict mode plus
`ajv-formats`. Ajv knows `int32`/`int64`/`date-time`/etc. but not `uint*`, so
it logs one `strict mode: unknown format "uint64" ignored in schema at path
"#/$defs/TrackerOut/properties/id"` warning per affected field, once per
`tools/list`. These are warnings, not errors — every tool still works — but
they drown the log on every session start. Some other MCP clients are less
forgiving: Gemini-family providers reject a schema with an unrecognized
integer `format` outright rather than warning and ignoring it.

Fourteen structs across `tools/discovery.rs`, `tools/meta.rs`,
`tools/projects.rs`, and `tools/output.rs` carry a `u64`/`u32` field, plus
`ProjectRef`'s untagged `u64 | String` `anyOf`.

## Decision

Add `tools::schema`, a single module both `input`/`output` schema
construction routes through. It calls rmcp's own
`schema_for_input`/`schema_for_output`, then walks the resulting JSON value
recursively and removes every `"format"` entry whose value is in a small
non-standard list (`uint`, `uint8`, `uint16`, `uint32`, `uint64`, `uint128`,
`int128`). The walk is blind — every object/array node, not just the ones a
human expects — so it reaches `$defs`, `properties`, `items`, and
`anyOf`/`oneOf`/`allOf` (including `ProjectRef`'s untagged enum) without
needing to special-case any of them.

`format` is annotation-only for JSON Schema integers: it does not affect
validation the way `minimum`/`maximum` do. schemars already emits
`"minimum": 0` alongside `"format": "uint64"` for every unsigned field, so
removing the format string loses no constraint information. Rust field types
stay `u64`/`u32`; nothing downstream of the schema (deserialization,
serialization, the actual field types) changes.

Every `#[tool(...)]` attribute's `output_schema` (and, for the two tools that
take parameters, `input_schema`) now reads
`crate::tools::schema::output::<T>()` / `crate::tools::schema::input::<T>()`
instead of calling rmcp's `schema_for_output`/`schema_for_input` directly.
`tests/tools_basic.rs` asserts every `format` string served anywhere in
`tools/list` is in an explicit allowlist (the `ajv-formats` set), which fails
if a future tool's author forgets to route a new struct through this module.

## Alternatives considered

- **Change `u64`/`u32` fields to `i64`/`i32`.** Rejected: `redmine-client`'s
  id types and `Page` counts are genuinely unsigned. This would spray
  `try_from`/casts through every tool that touches an id, and drops the
  `minimum: 0` constraint the schema currently gets for free from the Rust
  type.
- **Per-field `#[schemars(with = "i64")]` overrides (~18 sites).** Rejected:
  no central enforcement — trivially forgotten on a new field — and it
  misdescribes an unsigned id as signed in the schema even though the Rust
  type stays `u64`.
- **Do nothing.** Rejected: the warnings are per-field, not per-session, so
  the noise scales with the number of numeric fields across all tools, and a
  provider that hard-rejects unknown integer formats would make the affected
  tools uncallable rather than merely noisy.

## Consequences

- Every new tool must build its schemas through `tools::schema::output`/
  `input`, not rmcp's functions directly — enforced by the allowlist test
  rather than left to code review.
- The normalizer clones and walks the schema once per `#[tool]` attribute
  evaluation (i.e. once per router construction / per service instance, not
  per request), so its cost is negligible and it must not be moved onto a
  per-request path.
- A blind `"format"`-key walk would also strip a `uint64` string that
  appeared as a JSON *value* somewhere legitimate (e.g. an enum variant
  literally named `"uint64"`). No such value exists in this server's schemas
  today; `tools/schema.rs`'s unit tests pin the behavior — only the `format`
  *key* is touched, never a string value — so this stays intentional rather
  than accidental.

## 2026-08 amendment: portable dialect

Google Vertex's `FunctionDeclaration.parameters` is an OpenAPI-3.0-subset
`Schema`: a single-valued `type` enum, no `$ref`/`$defs`, and no `type`
arrays. It rejects this server's `tools/list` outright (`parameters.project_id
schema didn't specify the schema type field`) rather than warning like Ajv
does for the `uint*` `format` case above.

`tools::schema::to_portable` extends this module's central-normalizer
pattern with two more transforms, gated behind
`REDMINE_MCP_SCHEMA_DIALECT=portable` (`crate::config::SchemaDialect`,
default `strict`):

- Inline every `$ref`/`$defs` pair (all 16 of this server's input-schema
  `$defs` are small and non-recursive, so inlining is safe and — since a
  `$ref` node repeated in `properties` no longer shares a single `$defs`
  entry — even shrinks `tools/list` slightly).
- Collapse `{"type":["T","null"]}` (every `Option<T>` for a scalar `T`) to
  `{"type":"T"}`.

This is opt-in, not the default, because the portable form is **lossy**: a
model can no longer see that an optional field may be explicitly `null`, nor
— for the untagged `integer | string` unions (`ProjectRef`, `AssignedToRef`,
`UserRef`) still expressed as a bare `anyOf` node with no `type` of its own
— that Vertex might still reject it (deferred to a stage 2 scalar-union
collapse, landed only if that turns out to be necessary). Runtime
deserialization is unaffected either way; the worst case for a client that
handles the rich form correctly (Claude, GPT) is that switching to `portable`
would degrade its view of the schema for no reason, hence the flag defaults
to `strict`.

`outputSchema` is untouched by either dialect: Gemini never sees it, and
opencode validates `structuredContent` against it with Ajv, where the richer
form is strictly better.
