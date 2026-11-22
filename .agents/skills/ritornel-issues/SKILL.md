---
name: ritornel-issues
description: >-
  Rules for deciding when to file a GitHub issue on Ritornel, how to structure
  and label issues, and how to separate durable public work from ephemeral
  sprint fixes or architecture docs.
---

# Ritornel issue standard

GitHub Issues are the public backlog for durable bugs, platform gaps, and grounded upcoming features. Keep the tracker low-noise so it stays readable as a roadmap over time.

## When to file an issue (the 3-point test)

File a public GitHub issue **only if all three are true**:

1. **It will outlive the current week or PR batch.** Do not file issues for five-minute fixes you can land directly, or for rapid-fix audit findings being burned down immediately in PR batches.
2. **It is concrete and settled.** Do not file rough ideation, open-ended brainstorming, or unsettled UX questions that have not been decided yet.
3. **It contains no private data.** Never include personal machine names, local paths, or real music-library or listening-history data.

**Agent rule:** Never auto-file batches of micro-issues during an audit or active sprint. Only open a public issue when explicitly asked, or when a pull request deliberately defers a concrete, user-visible bug or platform gap so it is not forgotten.

## Two kinds of issues

- **Bug or platform gap:** A concrete defect, flaky test, or missing platform capability that will sit open for a while (for example, audio output staying open while idle, or missing Windows media controls).
- **Feature tracking issue (epic):** A grounded, settled upcoming feature ready to build publicly. Put the concise design scope and acceptance criteria in the issue body, and attach a checklist or sub-issues if it spans multiple pull requests. When the work lands, update `docs/internals/<area>.md` in the same PR and close the issue.

## Issue format

Keep issues short, direct, and matter-of-fact. Use literal technical titles and descriptions—avoid literary, folksy, or stylized phrasing (reserve editorial voice for marketing copy).

```markdown
## Problem (or Goal)
One short paragraph stating what is broken or what this adds for someone using Ritornel.

## Current vs. Expected (or Scope)
Concrete behavior today and what done looks like. For a multi-PR feature, list the slices or acceptance checks.

## Pointers
Relevant crates, files, or `docs/internals/<area>.md` sections.
```

## Labels

Use a small, consistent set of labels when available on the repository:

- **Kind:** `bug`, `enhancement`, `documentation`
- **Area:** `area:server`, `area:contract`, `area:identity`, `area:history`, `area:playback`, `area:desktop`, `area:plugins`
- **Platform** *(when OS-specific)*: `platform:macos`, `platform:linux`, `platform:windows`

## Pre-flight check before posting

- Every example uses invented artists, albums, and tracks.
- No personal paths (`/Users/...`, `C:\Users\...`), real collection counts, or private internal references appear in the title or body.
- No duplicate issue already tracks the same problem.
