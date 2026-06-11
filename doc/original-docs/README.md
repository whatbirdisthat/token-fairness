# Original docs (preserved for posterity)

Verbatim copies of the documents that shipped with the **bash** token-aware scheduler inside
idea-to-production's CONCIERGE plugin, before it was ported to `tf` and removed from that repo.
Kept here as historical record — they are not live and are not part of the plugin.

| File | Was | Notes |
|---|---|---|
| `token-aware-scheduling.md` | `concierge/knowledge/` | the scheduler's operating model / canon |
| `SKILL.md` | `concierge/skills/token-scheduler/` | the `/concierge:schedule` orchestration discipline |
| `schedule.md` | `concierge/commands/` | the `/concierge:schedule` command doc |
| `REVIEW_TOKEN_GUARD_FAILURE.md` | i2p repo root | the post-mortem that justified the scheduler |

The living equivalents are this plugin's `tf` binary, `doc/cutover.md`, and the vendored bash
oracle under `tests/oracle/` (the conformance source of truth, pinned at SHA `0b46ff3`).
