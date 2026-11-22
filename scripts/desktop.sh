#!/usr/bin/env bash
# Checks, builds and runs the desktop app (apps/desktop), in GPUI, on macOS
# and Linux. `mise run check` calls `check`, and CI calls it on its macOS job
# and its desktop-linux job.
#
#   scripts/desktop.sh check          check formatting, run clippy with
#                                          warnings denied, run the tests and
#                                          build the app
#   scripts/desktop.sh app            build Ritornel.app, with the release
#                                          ritornel-server inside it, as
#                                          `mise run build` does, in
#                                          apps/desktop/target/release/bundle;
#                                          on Linux a folder, `ritornel`
#                                          there, of the app, that server
#                                          and the icon
#   scripts/desktop.sh run [args]     run it against an invented library
#                                          of 8,600 albums, never a real one:
#                                          made once in apps/desktop/
#                                          .invented, served silently on a
#                                          port of its own, stopped on quit
#                                          (RITORNEL_GPUI_SERVE_ARGS adds to the
#                                          server's arguments: `--invent-churn 5`
#                                          writes to the library five times a
#                                          second)
#   scripts/desktop.sh tones [args]   run it against a library that plays:
#                                          one invented album of four tones a
#                                          minute and a half long, made once
#                                          with the pinned ffmpeg in
#                                          apps/desktop/.tones, played
#                                          into nothing
#   scripts/desktop.sh offscreen      ensure a shared display no one sees
#                                          is running and print
#                                          `offscreen <id> <x> <y>`;
#                                          `offscreen status` checks it and
#                                          `offscreen stop` removes it
#
# With RITORNEL_OFFSCREEN=1, `run` and `tones` open their windows
# on that shared display (`tools/offscreen.m`), out of the Dock and never
# taking the keyboard, so that someone can go on using the Mac meanwhile:
# an agent's runs always set it. The display stays up between runs so macOS
# does not reconfigure screens on every run; `offscreen stop` removes it.
# A caller that already sets RITORNEL_TEST_DISPLAY keeps its own. A run that
# cannot be hidden does not start, so on Linux, which has no such display
# yet, nothing starts with it.
#
# It is a Cargo workspace of its own, so that GPUI reaches neither the main
# workspace's build nor its lock file. It is checked on macOS and Linux; it
# builds on Windows too (`scripts/desktop.ps1`). On Linux it needs the
# development files of the libraries GPUI links, and without them it says
# which are missing and skips, so the hub's build never needs them; in CI
# (`CI` set) it fails instead, so a job cannot pass by skipping.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
app=$root/apps/desktop
system=$(uname -s)
source "$root/scripts/wait-for-server.sh"

case $system in
    Darwin) ;;
    Linux)
        # By their pkg-config names: Arch's fontconfig, libxcb, libxkbcommon
        # and libxkbcommon-x11; Debian's and Ubuntu's libfontconfig-dev,
        # libxcb1-dev, libxkbcommon-dev and libxkbcommon-x11-dev.
        missing=()
        for library in fontconfig xcb xkbcommon xkbcommon-x11; do
            pkg-config --exists "$library" 2>/dev/null || missing+=("$library")
        done
        if ((${#missing[@]})); then
            needs="The GPUI desktop app needs the development files of ${missing[*]} (by pkg-config)"
            [[ -z ${CI:-} ]] || { echo "$needs, and CI must not skip it." >&2; exit 1; }
            echo "$needs: skipped."
            exit 0
        fi
        ;;
    *)
        echo "The GPUI desktop app is checked on macOS and Linux only: skipped."
        exit 0
        ;;
esac

# What only a Mac can do, said rather than pretended elsewhere.
mac_only() {
    [[ $system == Darwin ]] && return
    echo "$1 works on macOS only: Linux has no display no one sees for it yet." >&2
    exit 1
}

# Cargo, in the app's own workspace.
gpui() {
    cargo "$1" --manifest-path "$app/Cargo.toml" "${@:2}"
}

# The tool that manages the shared display no one sees, built when it changes.
offscreen_tool() {
    mkdir -p "$app/target/tools"
    tool=$app/target/tools/offscreen
    if [[ ! $tool -nt $app/tools/offscreen.m ]]; then
        local tmp="$tool.$$"
        clang -Wall -Wextra -fobjc-arc -framework Foundation -framework CoreGraphics \
            "$app/tools/offscreen.m" -o "$tmp"
        mv -f "$tmp" "$tool"
    fi
}

# hide: with RITORNEL_OFFSCREEN=1, ensures the shared hidden display is running
# and tells the app to open its windows on it. Leaves a caller-supplied
# RITORNEL_TEST_DISPLAY untouched.
hide() {
    [[ ${RITORNEL_OFFSCREEN:-} == 1 ]] || return 0
    mac_only RITORNEL_OFFSCREEN
    [[ -z ${RITORNEL_TEST_DISPLAY:-} ]] || return 0
    offscreen_tool
    local line
    line=$("$tool")
    if [[ ! $line =~ ^offscreen\ ([0-9]+)\ (-?[0-9]+)\ (-?[0-9]+)$ ]]; then
        echo "no display to hide the windows on" >&2
        exit 1
    fi
    export RITORNEL_TEST_DISPLAY=${BASH_REMATCH[1]} \
        RITORNEL_TEST_DISPLAY_AT=${BASH_REMATCH[2]},${BASH_REMATCH[3]}
}

# serve: starts a silent server on the invented library, stopped when this
# script ends, and waits until it can be found.
serve() {
    data=${data:-$app/.invented/data}
    (cd "$root" && cargo build --locked --release --quiet -p ritornel-server)
    server=$root/target/release/ritornel-server
    if [[ $data == "$app/.invented/data" && ! -f $data/.invented ]]; then
        mkdir -p "$data" "$app/.invented/root"
        "$server" invent --data-dir "$data" --root "$app/.invented/root" --albums 8600 --showcase
        touch "$data/.invented"
    fi
    # The server stops when its standard input closes: when this script ends.
    exec 3< <(while kill -0 $$ 2>/dev/null; do sleep 1; done)
    log=$(mktemp)
    RITORNEL_TEST_NO_MEDIA_SESSION=1 "$server" serve --owned --data-dir "$data" \
        --listen 127.0.0.1:0 --output null --log-level warn ${RITORNEL_GPUI_SERVE_ARGS:-} <&3 >>"$log" 2> >(tee -a "$log" >&2) &
    served=$!
    trap 'kill $served 2>/dev/null; rm -f "$log"' EXIT
    wait_for_server "$served" "$log" 100
}

case "${1:-}" in
    check)
        bash "$root/scripts/tests/wait-for-server.sh"
        bash "$root/scripts/tests/offscreen.sh"
        gpui fmt --check
        gpui clippy --locked --all-targets -- -D warnings
        gpui test --locked
        gpui build --locked --release
        ;;
    run)
        gpui build --locked --release --quiet
        hide
        serve
        "$app/target/release/ritornel-gpui" --data-dir "$data" "${@:2}"
        ;;
    tones)
        # The invented library has no audio, so nothing on it plays.
        data=$app/.tones/data
        folder="$app/.tones/root/The Test Tones/Long Tones"
        if [[ ! -d $folder ]]; then
            mkdir -p "$folder"
            for track in 1 2 3 4; do
                hz=$((110 * (track + 1)))
                (cd "$root" && mise exec -- ffmpeg -loglevel error -y \
                    -f lavfi -i "sine=frequency=$hz:duration=90" \
                    -metadata artist="The Test Tones" -metadata album_artist="The Test Tones" \
                    -metadata album="Long Tones" -metadata title="Tone of $hz" \
                    -metadata track=$track -metadata date=2020 \
                    "$folder/0$track Tone of $hz.flac")
            done
        fi
        gpui build --locked --release --quiet
        hide
        serve
        if [[ ! -f $data/.scanned ]]; then
            (cd "$root" && cargo build --locked --release --quiet -p ritornelctl)
            "$root/target/release/ritornelctl" library create --data-dir "$data" >/dev/null
            "$root/target/release/ritornelctl" scan "$app/.tones/root" --data-dir "$data" >/dev/null
            touch "$data/.scanned"
        fi
        "$app/target/release/ritornel-gpui" --data-dir "$data" "${@:2}"
        ;;
    offscreen)
        mac_only "scripts/desktop.sh offscreen"
        offscreen_tool
        shift
        exec "$tool" "$@"
        ;;
    app)
        # The app and the server it starts, side by side in one bundle, as
        # the launcher looks for them (`ritornel_client::launch`).
        (cd "$root" && cargo build --locked --release --quiet -p ritornel-server)
        gpui build --locked --release
        if [[ $system == Linux ]]; then
            # A folder to put anywhere, the app and its server side by side.
            folder=$app/target/release/bundle/ritornel
            rm -rf "$folder"
            mkdir -p "$folder"
            cp "$app/target/release/ritornel-gpui" "$folder/ritornel-desktop"
            cp "$root/target/release/ritornel-server" "$folder/ritornel-server"
            cp "$app/bundle/icon.png" "$folder/icon.png"
            echo "built $folder"
            exit 0
        fi
        bundle=$app/target/release/bundle/Ritornel.app
        version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$app/Cargo.toml" | head -1)
        rm -rf "$bundle"
        mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
        sed "s/VERSION/$version/" "$app/bundle/Info.plist" >"$bundle/Contents/Info.plist"
        cp "$app/bundle/icon.icns" "$bundle/Contents/Resources/icon.icns"
        cp "$app/target/release/ritornel-gpui" "$bundle/Contents/MacOS/ritornel-desktop"
        cp "$root/target/release/ritornel-server" "$bundle/Contents/MacOS/ritornel-server"
        codesign --force --sign - "$bundle/Contents/MacOS/ritornel-server"
        codesign --force --sign - "$bundle"
        echo "built $bundle"
        ;;
    *)
        echo "usage: $0 check | app | run [args] | tones [args] | offscreen [status|stop]" >&2
        exit 2
        ;;
esac
