---
name: ritornel-docs
description: >-
  How documentation is organized, written, cited, and maintained in Ritornel.
  Read before creating or editing ARCHITECTURE.md, docs/internals/, future
  docs/guide/, or doc comments that cite them.
---

# Ritornel documentation standard

Every kind of documentation in this repository has one clear home. Do not mix shipped architecture, bugs/tasks, and speculative future specs in the same document.

## Documentation map

| Home | Audience | What belongs there |
|---|---|---|
| `README.md` | Anyone landing on the repo | What Ritornel is, what works today, how to build and run it, and where things live. |
| `ARCHITECTURE.md` | Contributors and agents | Bird's-eye workspace map, crate boundaries, one-way dependency rules, core invariants, and shared vocabulary. |
| `docs/internals/<area>.md` | Contributors and agents | How a shipped subsystem works today, why its key choices were made, and any deliberate architectural hooks kept in code. |
| `rustdoc` (`//!`, `///`) | Contributors and crate users | Local invariants for a crate, module, type, enum, or trait. Checked with `mise run docs`. |
| `schema/*.sql`, `contract/contract.ts` | Contributors and client authors | Generated reference snapshots, never edited by hand (`mise run schema`, `mise run contract`). |
| `docs/guide/` *(future)* | Listeners and self-hosters | User-facing documentation organized by Diátaxis (tutorials, how-to guides, concepts, reference). |
| GitHub Issues | Maintainer and contributors | Bugs, platform gaps, flaky tests, and grounded upcoming features (epics with sub-issues). |

## Writing `docs/internals/<area>.md`

`docs/internals/` describes **only what is built and running in this repository today**.

Required shape:

1. **Opening paragraph** — What this subsystem owns, where it lives, and what adjacent guides cover. Start directly with `# Title` (no YAML frontmatter).
2. **How it works** — Present tense, describing actual flows, data structures, and boundaries. Use small tables or ASCII diagrams where they clarify structure.
3. **Decisions** — Named headings or bold lead-in bullets stating each rule and why it exists in one to three sentences. Never use numbered decision lists (numbered lists rot when choices change).
4. **Later** *(optional, brief)* — Only deliberate architectural hooks already present in the code or schema for future work. Keep to a few concise bullets; never write speculative table schemas or multi-step future walkthroughs.

Never put any of these in `docs/internals/`:

- `## To do` sections, bug trackers, or flaky-test lists (file durable items as GitHub Issues per `ritornel-issues`).
- Milestone tables, delivery schedules, or date-stamped status diaries.
- Review tags (`[plan]`, `[new]`, `[review §…]`) or "Flags for review" sections.
- Real personal library statistics, real collection examples, or local machine paths (`/Users/...`).

## Code-to-doc citations

- **Code → doc:** Cite a subsystem guide by path and exact heading title, never by a list number:
  - `docs/internals/platforms.md, "Windows"`
  - `"Code organization" in docs/internals/platforms.md`
- **Doc → code:** Name key crates, modules, types, and constants in backticks (for example `` `RESOLVER_VERSION` ``, `` `methods.rs` ``, `` `HistorySource` ``) so they stay searchable with `rg`. Avoid linking to line numbers.
- **Local vs. cross-cutting:** Put single-type or single-trait invariants in `///` rustdoc right beside the code using intra-doc links (`[`Type`]`). Reserve `docs/internals/` for cross-cutting architecture and rationale.

## Voice and style

- **Direct technical English, not literary phrasing.** Use literal, scannable engineering nouns for headings and concepts (for example, `Record health`, `Export`, `Process lifecycle`, `Storage split`, `Code organization`—never coy or folksy headings like `"Keeping the record"`, `"Taking the record out"`, or `"Which file holds what"`).
- Plain words, present tense for shipped behavior, one idea at a time. Avoid overly stylized, poetic, or precious cadence.
- No exclamation marks, no superlatives, no marketing hype, and no promise of anything that is not built.
- Do not hard-wrap Markdown prose.
- Every example uses invented data, never a real music library or listening history.

## Authoring and maintenance workflow

1. **Verify against code first.** Before writing or editing any doc, check actual crate names, version constants, enums, CLI flags, RPC methods, and schema files in the codebase. Never rely on memory or older notes.
2. **Update in the same PR.** Any pull request that changes behavior, invariants, platform support, crate boundaries, or wire contracts updates the matching `docs/internals/<area>.md` (and `ARCHITECTURE.md` if crate layout or core invariants changed) in the same PR.
3. **Run `mise run docs`** (part of `mise run check`). It verifies `docs/internals/` citations, heading matches, structural rules, and rustdoc intra-doc links.

## Future user documentation (`docs/guide/`)

When user-facing documentation is added, organize every page strictly by Diátaxis—never mix quadrants on one page:

- **Tutorials** (`getting-started/`) — Learning by doing (install, scan a folder, play an album).
- **How-to guides** (`how-to/`) — Step-by-step recipes for a concrete task (import Last.fm history, run a headless hub).
- **Concepts** (`concepts/`) — Plain-English mental models (how Ritornel treats files, recordings, and listening history).
- **Reference** (`reference/`) — Exact lookup facts (data paths, supported formats, shortcuts, `ritornelctl` CLI reference generated from `clap` and checked in CI).

Write user guides in calm, second-person plain English for everyday listeners. Keep internal crate, module, and table names out of user guides unless a reference page explicitly requires them.
