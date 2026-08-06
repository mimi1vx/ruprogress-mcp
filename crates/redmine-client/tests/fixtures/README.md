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
