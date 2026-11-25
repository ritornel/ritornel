---
name: ritornel-issues
description: >-
  Rules for deciding when to file a GitHub issue on Ritornel, how to structure
  and label issues, and how to separate durable public work from ephemeral
  sprint fixes or architecture docs.
---

# Ritornel issue standard

GitHub Issues and the [Ritornel Roadmap](https://github.com/orgs/ritornel/projects/1) GitHub Project are the public backlog for Ritornel across `ritornel/ritornel`, `ritornel/console`, `ritornel/website`, and `ritornel/.github`. Keep the tracker low-noise so it stays readable as a roadmap over time.

## Two kinds of issues

### 1. High-level product epics (`label: epic`)

An epic tracks a major product capability over time (for example, network playback, transcoding, portable device sync, playlists, headless hub, or library corrections).

- **Keep epic descriptions high-level.** State the goal, a concise MVP vs. beyond-MVP boundary, and relevant `docs/internals/` pointers.
- **Progressive detail via sub-issues.** An epic may be filed while still coarse (`Status: Needs Design` on the project board) without premature task checklists. Flesh out specifics in linked GitHub sub-issues as designs settle and slices become ready to build.
- **Epics live beyond MVP.** Once an epic's MVP ships, update its shipped summary and move it to `Next` or `Later` until the capability is complete.

### 2. Concrete issues (bugs, platform gaps, and shaped slices)

File a concrete public GitHub issue **only if all three are true**:

1. **It will outlive the current week or PR batch.** Do not file issues for five-minute fixes you can land directly, or for rapid-fix audit findings being burned down immediately in PR batches.
2. **It is concrete and settled.** Do not file speculative micro-tasks or unverified observations before they are reproduced or shaped.
3. **It contains no private data.** Never include personal machine names, local paths, or real music-library or listening-history data.

Link concrete feature slices to their parent epic using GitHub's native sub-issue relationship. Standalone bugs and independent enhancements sit directly on the project board without a forced parent epic.

**Agent rule:** Never auto-file batches of micro-issues during an audit or active sprint. Only open a public issue when explicitly asked, or when a pull request deliberately defers a concrete, user-visible bug or platform gap so it is not forgotten.

## Project board tracking

Add durable issues and epics to [Ritornel Roadmap](https://github.com/orgs/ritornel/projects/1) using two fields:

- **`Status`:** `Needs Design` (coarse / unshaped), `Needs Call` (waiting on a concrete decision), `Ready` (shaped and ready to pick up), `In Flight` (active branch/PR), or `Done`.
- **`Horizon`:** `Now` (active priority), `Next` (queued), or `Later` (parked out of daily view).

When work lands, update `docs/internals/<area>.md` in the same PR, close the linked issue, and set its board status to `Done`.

## Issue format

Keep issues short, direct, and matter-of-fact. Use literal technical titles and descriptions—avoid literary, folksy, or stylized phrasing (reserve editorial voice for marketing copy), and avoid hardcoding a single hardware brand name into an epic title when the capability is broader.

```markdown
## Problem (or Goal)
One short paragraph stating what is broken or what this adds for someone using Ritornel.

## Current vs. Expected (or Scope)
Concrete behavior today and what done looks like. For an epic, give a brief MVP vs. Beyond MVP outline and leave task-level detail to linked sub-issues.

## Pointers
Relevant crates, files, or `docs/internals/<area>.md` sections.
```

## Labels

Use a small, consistent set of labels when available on the repository:

- **Kind:** `bug`, `enhancement`, `documentation`, `epic`, `accessibility`, `flaky-test`
- **Area:** `area:server`, `area:contract`, `area:identity`, `area:history`, `area:playback`, `area:desktop`, `area:plugins`
- **Platform** *(when OS-specific)*: `platform:macos`, `platform:linux`, `platform:windows`

## Pre-flight check before posting

- Every example uses invented artists, albums, and tracks.
- No personal paths (`/Users/...`, `C:\Users\...`), real collection counts, or private internal references appear in the title or body.
- No duplicate issue already tracks the same problem.
