# FOUNDRY Governance — token-fairness

Consumed by the delivery step (`ds-step-9-commit-push`) and the merge-governance protocol
(`knowledge/protocols/merge-governance.md`). Defines who merges and how this repo is governed
through GitHub.

**Merge mode:** pr-approval

> Agent commits to a feature branch, pushes, opens a PR carrying the adversarial-review verdict,
> then STOPS. A human reviews and merges. The agent never self-merges. The always-on adversarial
> review gate runs regardless of mode.

## Repo-specific conventions

- **Trunk branch:** `master` (not `main`). PRs target `master`; the stacked-PR retargeting guard
  in merge-governance applies against `master`.
- **Org allowlist:** origin is `agentic-underground/token-fairness` → matches the default
  `agentic-underground/*`, so full Commit→Issue→PR governance is active:
  - one GitHub issue per completed work item;
  - each work-item commit carries a `GITHUB_ISSUE: #N` footer trailer
    (see `knowledge/protocols/commit-message.md` §2);
  - the **PR body** carries `Closes #N` (the only thing that closes the issue on merge);
  - the `ROADMAP:` footer uses the **non-closing** `item #N` form on this GitHub origin, to avoid
    a roadmap number wrongly closing a same-numbered GitHub issue (the two-number-space trap).
- **Handler annotation:** each value-handler appends one `gh issue comment` per completed
  contribution (Activity / Value added / Cost) per `knowledge/protocols/handler-annotation.md`;
  fallback sink when no issue/`gh` is `doc/HANDLER_LOG.jsonl`.

> Switch with "give FOUNDRY merge autonomy" (→ `direct-merge`) / "require PR approvals"
> (→ `pr-approval`). Absent/unreadable ⇒ defaults to `pr-approval`.
