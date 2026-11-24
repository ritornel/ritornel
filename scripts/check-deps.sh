#!/usr/bin/env bash
# Fails if a dependency points the wrong way: contract <- domain <- engine <-
# server, and contract <- client <- each client program. See "Workspace and
# tooling" in docs/internals/server.md.
#
# A rule covers what a package is built with: its normal dependencies, direct
# and transitive, on every platform and with every feature on. Dev and build
# dependencies are not counted.
#
# The desktop app, in GPUI, is a workspace of its own (apps/desktop), and
# its rule is asked of its own manifest. Asking builds nothing, so it needs
# no desktop library and runs on Linux as it does on a Mac.
set -euo pipefail
cd "$(dirname "$0")/.."

# The manifest of the workspace a rule is about.
manifest=Cargo.toml
tree() {
    cargo tree --manifest-path "$manifest" --edges normal --target all --all-features "$@"
}
failed=0

# forbid <package> <crate>...: the package is never built with these crates.
forbid() {
    local package=$1 crate built_with
    shift
    built_with=$(tree --package "$package" --prefix none --format '{p}' | awk 'NR > 1 { print $1 }')
    for crate in "$@"; do
        if grep --quiet --line-regexp --fixed-strings "$crate" <<<"$built_with"; then
            echo "error: $package must not depend on $crate. The path, from $crate back to $package:" >&2
            tree --package "$package" --invert "$crate" >&2 || true
            failed=1
        fi
    done
}

# only <package> [<crate>...]: of the workspace's crates, the package is built
# with these and no others.
only() {
    local package=$1 member members
    members=$(cargo tree --workspace --depth 0 --prefix none --format '{p}' | awk 'NF { print $1 }')
    for member in $members; do
        case " $* " in
            *" $member "*) ;;
            *) forbid "$package" "$member" ;;
        esac
    done
}

# No client links the engine or SQLite. ritornel-device gets a line here
# when it arrives.
forbid ritornelctl ritornel-engine rusqlite
forbid ritornel-client ritornel-engine rusqlite
manifest=apps/desktop/Cargo.toml forbid ritornel-desktop-gpui ritornel-engine rusqlite

only ritornel-contract
only ritornel-domain ritornel-contract
forbid ritornel-domain rusqlite

# A plugin builds on the traits and on nothing of the core's, and the core
# builds on no plugin.
only ritornel-source
only ritornel-lastfm ritornel-source
forbid ritornel-lastfm rusqlite
forbid ritornel-engine ritornel-lastfm scrobl
only ritornel-musicbrainz ritornel-source
forbid ritornel-musicbrainz rusqlite
forbid ritornel-engine ritornel-musicbrainz reqwest

# What Windows asks for that takes unsafe code stands beside the core: it
# builds on no crate of the workspace, and the engine, the server and the
# tests build on it.
only ritornel-windows

# A server built without its default features but MusicBrainz, as the hub
# is, has no Last.fm code, no media session and no audio output in it, so
# on Linux it builds with no ALSA or D-Bus library installed.
hub=$(cargo tree --manifest-path Cargo.toml --package ritornel-server --no-default-features \
    --features musicbrainz --edges normal --target all --prefix none --format '{p}' | awk '{ print $1 }')
for crate in ritornel-lastfm scrobl souvlaki zbus windows cpal alsa alsa-sys; do
    if grep --quiet --line-regexp --fixed-strings "$crate" <<<"$hub"; then
        echo "error: ritornel-server, built as the hub is, is built with $crate" >&2
        failed=1
    fi
done

# Built without any feature, it has no plugin, and no reqwest, which only the
# MusicBrainz plugin brings.
bare=$(cargo tree --manifest-path Cargo.toml --package ritornel-server --no-default-features \
    --edges normal --target all --prefix none --format '{p}' | awk '{ print $1 }')
for crate in ritornel-lastfm ritornel-musicbrainz reqwest; do
    if grep --quiet --line-regexp --fixed-strings "$crate" <<<"$bare"; then
        echo "error: ritornel-server without any feature is built with $crate" >&2
        failed=1
    fi
done

if [ "$failed" -ne 0 ]; then
    exit 1
fi
echo "Dependency direction: ok."
