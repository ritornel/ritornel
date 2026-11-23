#!/usr/bin/env bash
# Says which of CI's jobs a pull request's changes can affect, given the
# paths it changes, one a line, on standard input. It prints
# `workspace=true|false` for the jobs that build the main workspace
# (workspace, hub and player-arm64) and `desktop=true|false` for the desktop
# app's, as GitHub Actions takes a step's outputs.
#
#   git diff --name-only --no-renames HEAD^1 HEAD | scripts/ci-changes.sh
#
# The desktop app builds the client and the contract and nothing else of
# the main workspace, and the main workspace builds nothing of the desktop
# app. Documentation is checked by the checks job, which always runs. A
# path not named here, the workflow and mise.toml among them, runs both, so
# a new folder is never skipped by mistake.
set -euo pipefail

workspace=false
desktop=false
while IFS= read -r path || [[ -n $path ]]; do
    case $path in
        *.md | docs/* | LICENSE | LICENSES/*) ;;
        apps/desktop/* | scripts/desktop.*) desktop=true ;;
        apps/server/* | apps/ritornelctl/* | crates/ritornel-domain/* | crates/ritornel-engine/* | \
            crates/ritornel-source/* | crates/ritornel-lastfm/* | crates/ritornel-windows/* | \
            Cargo.lock | schema/* | fixtures/* | contract/* | scripts/player-footprint.sh)
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
