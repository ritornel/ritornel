# Contributing to Ritornel

Ritornel is early and has one maintainer, so please keep that in mind when you wait for an answer.

## Before you start

- **Bugs and small fixes:** open an issue or a pull request directly.
- **Anything larger:** open an issue first and say what you have in mind. Much of the design is settled in notes that are not public yet, and a short conversation is cheaper than a pull request that goes against them.

## Making a change

[`AGENTS.md`](AGENTS.md) has the commands and the few rules. The short version:

1. Install [mise](https://mise.jdx.dev), then run `mise trust` and `mise install` in your checkout.
2. Make your change, with a test that fails without it.
3. Run `mise run check`. It runs everything CI runs.
4. Write the commit subject as `type(scope): what changed`, as the history does.

A change to the database schema is a new numbered migration plus `mise run schema`. A change to a call or a wire type is that change plus `mise run contract`.

**Land nothing a revert cannot undo until it is complete and proven.** Every change lands complete—known-incomplete work never lands, and partial features wait on their branch. Care follows what can go wrong after merge:

- **Contract changes and migrations** land one complete unit at a time, with one `CONTRACT_VERSION` bump and the fewest migrations it needs (collapse unmerged migrations before merging), after a migration dry run on a copy of a real backup. A release carrying a contract or migration change ships alone, and the next contract or migration unit waits to merge until that release is deployed.
- **Changes whose mistakes outlive a revert**—writing the library or listening history, sending outward (scrobbles, Now Playing), or writing or restoring backups—need two independent reviews (from two different model vendors for agent work) and a dry run on a copy of a real backup whenever they rewrite existing data. They land when ready unless they also carry a contract change or migration.
- **Everything else** (tests, CI, desktop, website, docs) lands after review (one review from another model vendor for agent work) as soon as it is ready.

Keep pull request descriptions factual about what changed and how it was checked; settle open design questions in an issue before opening the pull request.

In a stack of pull requests, CI runs only its Linux jobs on a layer that another open pull request builds on, and every job on the top layer, which holds the layers under it. Land a stack through its top layer.

## Using AI

Ritornel was written with coding agents, and contributions made the same way are welcome. End the pull request with the model and harness that did the work. What matters is that you can say what the change does for someone using the app, that you ran it, and how you checked it.

## Two rules

- **Test data is invented.** Nothing from a real music library or a real listening history is committed, and nothing invented is copied from a real catalogue.
- **A dependency carries a licence that `deny.toml` lists.** `mise run licenses` fails on any other. Ask before adding a licence to the list.

## Licence

Ritornel is MIT-licensed. By contributing you agree that your contribution is under the same licence.
