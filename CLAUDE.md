# CLAUDE.md

Canonical project guide is [AGENTS.md](AGENTS.md). Read it.

## Claude-specific notes

- Skills live at `.agents/skills/` (vendor-agnostic). `.claude/skills/` is intended to symlink there; see `.agents/skills/README.md`.
- Reviews: prefer `/fleet-review feature <signal>` for architecture or module reviews; `/fleet-review branch` for diff review against main.
