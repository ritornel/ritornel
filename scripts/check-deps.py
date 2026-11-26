# Fails if a dependency points the wrong way: contract <- domain <- engine <-
# server, and contract <- client <- each client program. See "Workspace and
# tooling" in docs/internals/server.md.
#
# A rule covers what a package is built with: its normal dependencies, direct
# and transitive, on every platform and with every feature on. Dev and build
# dependencies are not counted.
#
# The desktop app, in GPUI, is a workspace of its own (apps/desktop), and its
# rule is asked of its own manifest. Asking builds nothing, so it needs no
# desktop library and runs on Linux and Windows as it does on a Mac.

import os
import pathlib
import subprocess
import sys

os.chdir(pathlib.Path(__file__).resolve().parent.parent)

failed = False


def cargo(*args: str) -> str:
    """Cargo's output. If Cargo fails, the script ends with its exit code."""
    done = subprocess.run(["cargo", *args], stdout=subprocess.PIPE, encoding="utf-8")
    if done.returncode != 0:
        sys.exit(done.returncode)
    return done.stdout


def names(listing: str) -> list[str]:
    """The package names a `--prefix none --format {p}` listing holds."""
    return [line.split()[0] for line in listing.splitlines() if line.strip()]


def tree_args(manifest: str, *args: str) -> list[str]:
    """The arguments of `cargo tree` over what a package is built with."""
    return ["tree", "--manifest-path", manifest, "--edges", "normal", "--target", "all", "--all-features", *args]


def forbid(package: str, *crates: str, manifest: str = "Cargo.toml") -> None:
    """The package is never built with these crates."""
    global failed
    # The first line is the package itself.
    built_with = names(cargo(*tree_args(manifest, "--package", package, "--prefix", "none", "--format", "{p}")))[1:]
    for crate in crates:
        if crate in built_with:
            print(f"error: {package} must not depend on {crate}. The path, from {crate} back to {package}:", file=sys.stderr)
            sys.stderr.flush()
            subprocess.run(["cargo", *tree_args(manifest, "--package", package, "--invert", crate)], stdout=sys.stderr)
            failed = True


# The main workspace's crates.
members = names(cargo("tree", "--workspace", "--depth", "0", "--prefix", "none", "--format", "{p}"))


def only(package: str, *allowed: str) -> None:
    """Of the workspace's crates, the package is built with these and no others."""
    forbid(package, *(member for member in members if member != package and member not in allowed))


# No client links the engine or SQLite. ritornel-device gets a line here
# when it arrives.
forbid("ritornelctl", "ritornel-engine", "rusqlite")
forbid("ritornel-client", "ritornel-engine", "rusqlite")
forbid("ritornel-desktop-gpui", "ritornel-engine", "rusqlite", manifest="apps/desktop/Cargo.toml")

only("ritornel-contract")
only("ritornel-domain", "ritornel-contract")
forbid("ritornel-domain", "rusqlite")

# A plugin builds on the traits and on nothing of the core's, and the core
# builds on no plugin.
only("ritornel-source")
only("ritornel-lastfm", "ritornel-source")
forbid("ritornel-lastfm", "rusqlite")
forbid("ritornel-engine", "ritornel-lastfm", "scrobl")
only("ritornel-musicbrainz", "ritornel-source")
forbid("ritornel-musicbrainz", "rusqlite")
forbid("ritornel-engine", "ritornel-musicbrainz", "reqwest")

# What Windows asks for that takes unsafe code stands beside the core: it
# builds on no crate of the workspace, and the engine, the server and the
# tests build on it.
only("ritornel-windows")


def server_built_with(*features: str) -> list[str]:
    return names(
        cargo(
            "tree", "--manifest-path", "Cargo.toml", "--package", "ritornel-server", "--no-default-features",
            *features, "--edges", "normal", "--target", "all", "--prefix", "none", "--format", "{p}",
        )
    )


# A server built without its default features but MusicBrainz, as the hub
# is, has no Last.fm code, no media session and no audio output in it, so
# on Linux it builds with no ALSA or D-Bus library installed.
hub = server_built_with("--features", "musicbrainz")
for crate in ("ritornel-lastfm", "scrobl", "souvlaki", "zbus", "windows", "cpal", "alsa", "alsa-sys"):
    if crate in hub:
        print(f"error: ritornel-server, built as the hub is, is built with {crate}", file=sys.stderr)
        failed = True

# Built without any feature, it has no plugin, and no reqwest, which only the
# MusicBrainz plugin brings.
bare = server_built_with()
for crate in ("ritornel-lastfm", "ritornel-musicbrainz", "reqwest"):
    if crate in bare:
        print(f"error: ritornel-server without any feature is built with {crate}", file=sys.stderr)
        failed = True

if failed:
    sys.exit(1)
print("Dependency direction: ok.")
