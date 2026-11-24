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

Land complete changes without leaving known gaps in core behavior for follow-up pull requests. When a feature adds migrations or changes `CONTRACT_VERSION` (or the wire contract), keep it to one contract bump and the fewest migrations its purpose needs—collapse migrations not yet on `main` before merging rather than adding one per pull request in a stack. Keep pull request descriptions factual about what changed and how it was checked; settle open design questions in an issue before opening the pull request.

In a stack of pull requests, CI runs only its Linux jobs on a layer that another open pull request builds on, and every job on the top layer, which holds the layers under it. Land a stack through its top layer.

## Using AI

Ritornel was written with coding agents, and contributions made the same way are welcome. End the pull request with the model and harness that did the work. What matters is that you can say what the change does for someone using the app, that you ran it, and how you checked it.

## Two rules

- **Test data is invented.** Nothing from a real music library or a real listening history is committed, and nothing invented is copied from a real catalogue.
- **A dependency carries a licence that `deny.toml` lists.** `mise run licenses` fails on any other. Ask before adding a licence to the list.

## Licence

Ritornel is MIT-licensed. By contributing you agree that your contribution is under the same licence.
