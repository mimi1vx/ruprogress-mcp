# Fixtures

Every fixture below is a synthetic JSON document built from the official
Redmine REST API documentation's example shapes for the stated version, not a
capture from a live instance (none was available while building this crate).
All identifiers, names, and dates are placeholders (`Example Project`,
`alice`, `2026-01-01`, ...); none of it is real data. If/when a fixture is
later replaced with a genuine capture, update this table with the real
capture date and re-run the scrub test below.

| Fixture | Redmine version modeled | Endpoint | Notes |
|---|---|---|---|
| `issue_6_1.json` | 6.1 | `GET /issues/1.json` | `created_on`/`updated_on` with no UTC suffix (naive form); single-value custom field |
| `issue_7_0.json` | 7.0 | `GET /issues/1.json` | `created_on`/`updated_on` with `Z` suffix; multi-value custom field |
| `project_6_1.json` | 6.1 | `GET /projects/1.json` | no `parent` |
| `project_7_0.json` | 7.0 | `GET /projects/1.json` | includes `parent` |
| `time_entry_6_1.json` | 6.1 | `GET /time_entries/1.json` | naive timestamps |
| `time_entry_7_0.json` | 7.0 | `GET /time_entries/1.json` | `Z`-suffixed timestamps |
| `user_6_1.json` | 6.1 | `GET /my/account.json` | naive timestamps; `mail` omitted (permission-gated field, and email-shaped strings are exactly what the scrub test below rejects) |
| `user_7_0.json` | 7.0 | `GET /my/account.json` | `Z`-suffixed timestamps |
| `tracker_6_1.json` | 6.1 | `GET /trackers.json` | second tracker omits `default_status` |
| `tracker_7_0.json` | 7.0 | `GET /trackers.json` | every tracker carries `default_status` |
| `issue_status_6_1.json` | 6.1 | `GET /issue_statuses.json` | carries `is_default` |
| `issue_status_7_0.json` | 7.0 | `GET /issue_statuses.json` | omits `is_default` (dropped from this endpoint) |
| `issue_priority_6_1.json` | 6.1 | `GET /enumerations/issue_priorities.json` | — |
| `issue_priority_7_0.json` | 7.0 | `GET /enumerations/issue_priorities.json` | — |
| `users_list_6_1.json` | 6.1 | `GET /users.json` | naive timestamp; no `admin` field |
| `users_list_7_0.json` | 7.0 | `GET /users.json` | `Z`-suffixed timestamp; `admin: true` |
| `saved_queries_6_1.json` | 6.1 | `GET /queries.json` | — |
| `saved_queries_7_0.json` | 7.0 | `GET /queries.json` | — |
| `project_with_trackers_7_0.json` | 7.0 | `GET /projects/1.json?include=trackers` | `project.trackers` populated |
| `attachment_6_1.json` | 6.1 | `GET /attachments/6243.json` | naive timestamp; no `thumbnail_url` (non-image) |
| `attachment_7_0.json` | 7.0 | `GET /attachments/6244.json` | `Z`-suffixed timestamp; includes `thumbnail_url` (image), which the client ignores (not modeled) |
| `project_files_6_1.json` | 6.1 | `GET /projects/1/files.json` | naive timestamp; file attached to a `Version` (`version` present) |
| `project_files_7_0.json` | 7.0 | `GET /projects/1/files.json` | `Z`-suffixed timestamp; file attached directly to the `Project` (`version` absent) |
| `upload_token_6_1.json` | 6.1 | `POST /uploads.json` | `upload.api.rsb` is identical across both versions (verified against `redmine/redmine` `6.1-stable` and `master`); paired per convention, values differ only for test readability |
| `upload_token_7_0.json` | 7.0 | `POST /uploads.json` | see above |

## Plugin fixtures

Third-party Redmine plugin endpoints (Checklists, and the other families as
they land) are modelled differently from the pairs above:

- **Provenance**: synthetic, derived from the reference implementation's
  handling of the plugin's endpoints rather than a live capture. Three of
  the four plugin families are commercial and the fourth needs a server-side
  install neither this repository nor CI has; none of these fixtures were
  captured from a running instance.
- **No `_6_1`/`_7_0` version pair**: the version axis that matters for a
  plugin endpoint is the *plugin's* version, not Redmine core's, and the
  plugin version is not something this project can enumerate. Naming two
  identical files per endpoint would imply a cross-version check that never
  happened, so plugin fixtures are named `{family}_{operation}.json` with no
  pair.

| Fixture | Endpoint | Notes |
|---|---|---|
| `checklist_items.json` | `GET /issues/{id}/checklists.json` | envelope shape, `{"checklists": [...]}` |
| `checklist_items_bare.json` | `GET /issues/{id}/checklists.json` | bare-array shape, `[...]` — the same plugin endpoint has been observed sending either |
| `checklist_item_created.json` | `POST /issues/{id}/checklists.json` | nested `{"checklist": {"id": N}}` shape |
| `agile_data_full.json` | `GET /issues/{id}/agile_data.json` | a populated row, all fields set |
| `agile_data_empty.json` | `GET /issues/{id}/agile_data.json` | `{"agile_data": null}` — the issue has no agile row |
| `issue_with_tags.json` | `GET /issues/{id}.json` | `additional_tags` plugin's injected `tags` array: one tag with `id`, one without (the plugin frequently omits it — see `model::plugins::tags::IssueTag`'s doc comment). Unlike the other plugin fixtures, `additional_tags` is open-source, but the wire shape is still unverified against a live instance. |

## Scrubbing

Every fixture must be free of real secrets, emails, and IP addresses. This is
enforced by `tests/scrub.rs`, which runs the same pattern locally that would
otherwise only be caught in CI:

```
(api[_-]?key|bearer |[a-z0-9._-]+@[a-z0-9.-]+\.[a-z]{2,}|\b(?:\d{1,3}\.){3}\d{1,3}\b)
```

Note this also rejects placeholder emails like `alice@example.com` — use a
bare username (`alice`) instead of an email shape anywhere a fixture doesn't
strictly need one.
