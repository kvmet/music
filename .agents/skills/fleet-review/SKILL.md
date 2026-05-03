---
name: fleet-review
description: Multi-agent code review fleet. Spawns diverse lens-specialized sub-agents, dedupes findings, validates adversarially, produces a grounded report. Two modes: branch (current vs main) and feature (directed at a feature or module).
---

# /fleet-review — Multi-Agent Code Review

Routes by first argument:

- `branch [base]` → [Branch review](#branch-mode) (default if no args; base defaults to `main`)
- `feature <signal>` → [Feature review](#feature-mode) (signal: file list, glob, symbol name, directory, or prose description)

Optional flag: `--lenses a,b,c` to restrict the lens set (default: all lenses from `lenses.md`).

Stages 3–6 are identical across modes. Only scope assembly (stage 1) and prompt framing (stage 2) differ.

**Companion files** (read when referenced):
- `lenses.md` — lens catalog and persona pools
- `prompts.md` — sub-agent prompt templates and finding schema
- `report.md` — final report template

**Operating rules**
- Read-only review of repo files. The orchestrator writes artifacts under `.fleet-review/run-<timestamp>/` and nowhere else. Suggest the user add `.fleet-review/` to `.gitignore` if not already present.
- Each stage persists its output to the run directory (`scope/`, `findings-raw.json`, `findings-deduped.json`, `findings-validated.json`, `findings-refuted.json`, `nearby-observations.json`, `report.md`, `report.json`). Keeps the orchestrator context lean and leaves a debug trail.
- Announce the run directory to the user at the start of the run.
- If any stage fails catastrophically (all lens agents malformed, scope empty, etc.), stop and report the run directory path to the user.
- Never skip the feature-mode confirmation gate.

---

## Stage 1 — Scope

Produce scope artifacts at `.fleet-review/run-<id>/scope/`, shared identically across all lens agents. Determinism matters: every agent gets the same manifest, so finding divergence is attributable to lens, not luck.

Artifacts written at this stage:
- `scope/manifest.md` — ordered list of in-scope files, each with `kind` and a one-line reason. Lens agents must Read files from this list; files outside are off-limits.
- `scope/diff.patch` — branch mode only; the unified diff.
- `scope/intent.md` — PR description, commit messages (branch mode), or the user's feature signal plus discovery agent notes (feature mode).

### Branch mode

1. Resolve base branch. Default `main` unless the user supplied one as the second argument.
2. Write `scope/diff.patch` from `git diff <base>...HEAD`.
3. Write `scope/intent.md` with the PR description (via `gh pr view` if a PR exists) or `git log <base>..HEAD` for commit messages.
4. Build the manifest. Include:
   - Every file appearing in the diff (`kind: changed`)
   - Files containing depth-1 callers of changed symbols (`kind: caller`)
   - Tests referencing any changed symbol (`kind: test`)
5. Exclude from the manifest: generated files, vendored code, lockfiles, binaries. Check `.gitattributes` for `linguist-generated` when unsure.
6. Write `scope/manifest.md` with one entry per file: `path`, `kind`, one-line reason.
7. **Chunking rule:** if total diff lines exceed 2000, partition the manifest into chunks by shared parent directory. Any single file with >500 changed lines becomes its own chunk. Each chunk runs the full fleet independently; dedupe joins them at stage 4.

### Feature mode

The user's signal may be file list, glob, symbol name, directory, or prose description.

1. **Discovery pass.** Spawn one Agent using the `discovery` prompt from `prompts.md`, passing the user's signal. It returns a file list grouped into `core_files`, `test_files`, `integration_files`.
2. **Confirmation gate.** Present the discovered file set to the user with per-file rationale. Wait for explicit approval. The user may add, remove, or accept. Do not proceed without confirmation.
3. Write `scope/intent.md` with the confirmed user signal and the discovery agent's interpretation and notes.
4. Write `scope/manifest.md` from the confirmed file set, with `kind`: `core` | `test` | `integration`. Include one-hop integration points and any data model / schema files the feature touches.
5. **Chunking rule:** if the confirmed manifest exceeds 800 lines of reviewable code, partition by shared parent directory. Any single file >500 lines becomes its own chunk.

---

## Stage 2 — Fan out

Spawn lens agents in parallel using the Agent tool. Each agent is independent. No shared scratchpad.

1. Read `lenses.md` for the lens catalog. Select lenses:
   - Default: all standard lenses plus all music-specific lenses
   - If the user passed `--lenses a,b,c`, use only those
2. Assign personas. Alternate round-robin between the reviewer-persona pool and the player-archetype pool in `lenses.md`. Each lens agent gets one persona.
3. For each (lens, persona) pair, spawn one Agent with:
   - `subagent_type`: `general-purpose`
   - `description`: `Fleet review — <lens>`
   - `prompt`: the `lens_agent` template from `prompts.md` with `{lens}`, `{lens_description}`, `{persona}`, `{mode}`, `{manifest}`, `{diff_or_feature_summary}`, `{intent}` substituted
4. Send all spawn tool calls in a **single message** so they run in parallel.

**Branch mode:** include the `nearby_observations_block` from `prompts.md` in each lens agent prompt. Findings concern the diff only; broader patterns go in `nearby_observations`. `{diff_or_feature_summary}` = embedded diff summary (not the full patch — just changed symbols + hunk count).

**Feature mode:** omit the `nearby_observations_block`. The entire confirmed file set is in-scope. `{diff_or_feature_summary}` = the feature signal and interpretation from discovery.

---

## Stage 3 — Collect

For each returned agent report:

1. Parse the JSON. On parse failure, re-prompt that agent once with the explicit schema. Drop on second failure.
2. Validate each finding against the schema in `prompts.md` (Finding Schema section). Drop findings missing required fields.
3. **Hallucination check.** Collapse runs of whitespace in both the `evidence_quote` and the target file contents, then search for the quote in the file. If the quote is not found anywhere, drop the finding. If found at different lines than cited, adjust `line_start`/`line_end` to the match and tag the finding with `line_adjusted: true`.
4. Assign each surviving finding an `id` (UUID) and tag with `agent_id` and `lens`.
5. Normalize paths (relative to repo root).

Write surviving findings to `findings-raw.json`. Collect `nearby_observations` to `nearby-observations.json` (branch mode only).

---

## Stage 4 — Dedupe

Goal: collapse same-issue findings while preserving cross-agent consensus.

1. **Structural clustering.** Group findings where all match:
   - Same `file`
   - Overlapping `line_start..line_end` ranges (any overlap)
   - Same or related `category` (taxonomy in `prompts.md`)
2. **LLM merge within cluster.** For each cluster with >1 finding, call the `dedupe_merge` prompt from `prompts.md`. It returns either a merged finding (max severity, max confidence, union of `assumes[]`, clearest `claim`, list of distinct `agent_id`s as `consensus`) or a split decision.
3. Compute `consensus_count` per surviving finding = count of distinct agents that flagged it.
4. Write the deduped set to `findings-deduped.json`.

Skip cross-location semantic linking for v1.

---

## Stage 5 — Validate

Each surviving finding gets an adversarial validator. Validators default to refuting, not confirming.

1. For each finding, spawn an Agent with:
   - `subagent_type`: `general-purpose`
   - `description`: `Validate — <title>`
   - `prompt`: the `validator` template from `prompts.md` with the finding JSON embedded
2. Batch up to 10 validator spawns per message for parallelism.
3. Each validator returns `{verdict, reasoning, evidence_quote, failed_assumptions, mitigations_found}`.
4. Apply verdicts:
   - `confirmed` → keep, annotate with validator output
   - `refuted` → move to refuted appendix (not dropped silently)
   - `unclear` → keep, flag for human review in the report
5. For surviving `critical` and `high` severity findings, run a second **independent** validator pass. Independent means a fresh Agent whose prompt contains none of the first validator's output (no verdict, reasoning, or quotes). If the two verdicts disagree, mark the finding `unclear`.
6. Write the validated set (confirmed + unclear, with verdicts attached) to `findings-validated.json`. Write refuted findings to `findings-refuted.json`.

---

## Stage 6 — Report

Render using `report.md` as the template.

**Sort order:**
1. Severity (critical → info)
2. Tiebreak: `consensus_count × confidence_weight` (high=3, medium=2, low=1)

Group by file within each severity band.

**Sections:**
- Executive summary (counts by severity, top themes)
- Findings (main body)
- Meta-findings (any category with ≥3 findings whose files share a parent directory)
- Patterns worth exploring (branch mode only, from `nearby_observations` — surface-level, no severity, framed as recommendations)
- Refuted findings appendix
- Lens coverage table (lens | raised | surviving validation)

Write outputs to the run directory:
- `.fleet-review/run-<id>/report.md` — the human-readable report
- `.fleet-review/run-<id>/report.json` — sidecar: `{mode, run_id, timestamps, lenses_run[], personas_assigned[], confirmed_findings[], unclear_findings[], refuted_findings[], nearby_observations[], lens_coverage{}}`

Tell the user the final report path when done.
