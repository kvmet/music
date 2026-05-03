# Report Template

The orchestrator renders the final report using the structure below. Placeholders in `{braces}` are substituted at render time. Iteration comments in `{for each ...}` are directives for the orchestrator, not literal text.

This file has no outer code fence so its sample `` ``` `` evidence fences and JSON snippets render as they would in a produced report.

---

# Fleet Review Report — {date}

- **Mode:** {mode}
- **Run ID:** {run_id}
- **Scope:** {scope_summary}
- **Base / HEAD:** {base_ref} ... {head_ref}   (branch mode only)
- **Files reviewed:** {file_count}
- **Fleet:** {n_lens_agents} lens agents across {n_lenses} lenses
- **Validation:** {n_confirmed} confirmed, {n_refuted} refuted, {n_unclear} unclear

## Executive summary

| Severity | Count |
|----------|-------|
| Critical | {n_critical} |
| High | {n_high} |
| Medium | {n_medium} |
| Low | {n_low} |
| Info | {n_info} |

{top_themes_paragraph}

---

## Findings

{for each file, ordered by max severity within file:}

### {file_path}

{for each finding in file, sorted by severity then consensus_count × confidence_weight:}

#### [{severity}] {title}

- **Lens:** {lens}
- **Confidence:** {confidence}
- **Consensus:** flagged by {consensus_count} / {total_agents} agents
- **Location:** `{file}:{line_start}-{line_end}`

**Claim**

{claim}

**Evidence**

~~~{language}
{evidence_quote}
~~~

**Suggested fix**

{suggested_fix or "Not provided."}

**Validator**

{validator.reasoning}

{if validator.mitigations_found: include "**Mitigations observed:** {validator.mitigations_found}"}

{if verdict == "unclear": include "**⚠ Human review recommended** — validators disagreed or evidence was ambiguous."}

---

{end findings section}

## Meta-findings

{for each category with ≥3 findings whose files share a parent directory:}

- **{parent_directory}** — {count} findings of category `{category}`. Consider whether this is a systemic pattern rather than isolated bugs.

{if no meta-findings: omit section}

---

## Patterns worth exploring

*(Branch mode only. Surface-level observations outside the reviewed diff. Not findings in this review. Recommendations for further exploration.)*

{for each nearby_observation:}

- **{pattern}**
  - Seen at: {examples joined by ", "}
  - Why it matters: {why_it_matters}

{if feature mode or no observations: omit section}

---

## Refuted findings

*(Findings that were raised but could not survive adversarial validation. Kept here for audit and tuning.)*

{for each refuted finding:}

- **[{original_severity}] {title}** — `{file}:{line_start}`
  - Original claim: {claim}
  - Validator reasoning: {validator.reasoning}
  - Failed assumptions: {failed_assumptions joined by "; "}

{if none refuted: write "No findings refuted."}

---

## Lens coverage

| Lens | Persona | Raised | Surviving |
|------|---------|--------|-----------|
{for each lens agent: | {lens} | {persona} | {raised_count} | {surviving_count} |}

---

*Generated {iso_timestamp} by fleet-review. Run directory: `{run_dir}`. JSON sidecar: `{run_dir}/report.json`.*

---

## Rendering notes

- Evidence code blocks use tilde fences (`~~~`) to avoid conflicts with outer markdown code fences that readers may wrap the report in.
- If a section has no content, omit its header entirely rather than showing "None" (except for Refuted findings, which always gets a line for audit).
- Detect `{language}` for evidence code blocks from file extension: `.py` → `python`, `.js` / `.jsx` → `javascript`, `.ts` / `.tsx` → `typescript`, `.go` → `go`, `.md` → `markdown`, else blank.
- Keep `top_themes_paragraph` to 2–4 sentences. Concrete patterns, not platitudes. Hint: dominant categories, most-affected directories, lenses that overlapped on the same findings.
- JSON sidecar (`report.json`) mirrors the full run: `{mode, run_id, timestamps, lenses_run[], personas_assigned[], confirmed_findings[], unclear_findings[], refuted_findings[], nearby_observations[], lens_coverage{}}`.
