#!/usr/bin/env bash
# Says which of CI's jobs a pull request's changes can affect, given the
# paths it changes, one a line, on standard input. It prints
# `workspace=true|false` for the jobs that build the main workspace
# (workspace, hub and player-arm64) and `desktop=true|false` for the desktop
# app's, as GitHub Actions takes a step's outputs. Run it from the
# repository's root.
#
#   git diff --name-only --no-renames HEAD^1 HEAD | scripts/ci-changes.sh
#
# The main workspace builds nothing of the desktop app. The desktop app
# builds the crates its lock file lists without a source, which are the
# repository's own, and a change to one of those runs its jobs too, so a
# crate it comes to depend on is never skipped. Documentation is checked by
# the checks job, which always runs. A path not named here, the workflow and
# mise.toml among them, runs both, so a new folder is never skipped by
# mistake.
set -euo pipefail

# The packages the desktop app's lock file lists without a source, and the
# folders under crates/ whose Cargo.toml names one of them.
desktop_packages=$(awk '
    /^\[/ { if (package && name != "" && !source) print name; package = /^\[\[package\]\]/; name = ""; source = 0 }
    package && /^name = / { gsub(/"/, "", $3); name = $3 }
    package && /^source = / { source = 1 }
    END { if (package && name != "" && !source) print name }
' apps/desktop/Cargo.lock)
desktop_crates=$(for manifest in crates/*/Cargo.toml; do
    name=$(awk -F'"' '/^name = / { print $2; exit }' "$manifest")
    if grep -qxF "$name" <<<"$desktop_packages"; then
        basename "$(dirname "$manifest")"
    fi
done)

workspace=false
desktop=false
while IFS= read -r path || [[ -n $path ]]; do
    case $path in
        *.md | docs/* | LICENSE | LICENSES/*) ;;
        apps/desktop/* | scripts/desktop.*) desktop=true ;;
        crates/*/*)
            workspace=true
            crate=${path#crates/}
            if grep -qxF "${crate%%/*}" <<<"$desktop_crates"; then
                desktop=true
            fi
            ;;
        apps/server/* | apps/ritornelctl/* | Cargo.lock | schema/* | fixtures/* | contract/* | \
            scripts/player-footprint.sh)
            workspace=true
            ;;
        *)
            workspace=true
            desktop=true
            ;;
    esac
done
echo "workspace=$workspace"
echo "desktop=$desktop"
