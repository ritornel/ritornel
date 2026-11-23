# Code under other licences

Ritornel is under the MIT licence (`../LICENSE`). A few files hold code adapted from other projects, and those parts stay under the licence they came with. Each file says so at its top, with the original's copyright lines.

| File | Adapted from | Licence |
|---|---|---|
| `apps/desktop/src/elastic.rs` | WebKit's `ScrollingEffectsController` and `ScrollAnimationRubberBand` | BSD 2-Clause, in the file |
| `apps/desktop/src/pretty.rs` | WebKit's `InlineContentConstrainer::prettifyRange` | BSD 2-Clause, in the file |
| `apps/desktop/src/lean.rs`, `balanced` | WebKit's `InlineContentBalancer` | BSD 2-Clause, in the file |
| `apps/desktop/src/input.rs` | Zed's GPUI example `input.rs` | Apache 2.0, `Apache-2.0.txt` |

The desktop app's GPUI Linux backend, `gpui-pre-linux`, comes from [ritornel/gpui](https://github.com/ritornel/gpui), Ritornel's patched copy of the published crate, under Apache 2.0 like the rest of GPUI. It is a dependency, not code in this repository, and `deny.toml` checks its licence as any other's.

A file that takes code from elsewhere joins this table, and only code under MIT, Apache 2.0 or BSD may be taken (`../AGENTS.md`, "Rules").
