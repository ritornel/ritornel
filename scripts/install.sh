#!/usr/bin/env bash
# Installs this checkout's release build for everyday use: ritornel-server and
# ritornelctl in ~/.local/bin, and the desktop app with its own server: on a
# Mac Ritornel.app in /Applications, and on Linux the folder `ritornel` in
# ~/.local/lib, with a launcher entry and an icon in $XDG_DATA_HOME. The app
# starts its own ritornel-server when none is running; the one in
# ~/.local/bin is for the background service (`ritornel-server service
# install`) and a headless hub. `mise run install` builds them first.
# RITORNEL_BIN_DIR and RITORNEL_APP_DIR choose other places.
#
# On Linux without the desktop app's libraries, `scripts/desktop.sh app`
# skips the app, and this installs the programs alone.
#
# A server that is running keeps the build it started with: this says so and
# leaves it running.
set -euo pipefail

# refuse_unless_app NAME PROGRAM: only an app is ever replaced. A folder at
# $app_dir/NAME without the app's PROGRAM in it, such as a data directory,
# stops the install before anything is written.
refuse_unless_app() {
    local app="$app_dir/$1"
    if [ -e "$app" ] && [ ! -x "$app/$2" ]; then
        echo "$app is there and is not Ritornel's app: nothing installed, and it left as it is" >&2
        exit 1
    fi
}

# install_app BUILT NAME: copies the app BUILT into a unique staging folder
# beside $app_dir/NAME, moves any previous copy aside into it, and swaps the
# new copy in, so that no file of an older build is left inside the new one
# and no pre-existing sibling beside the app is deleted. If publishing fails,
# the previous copy is moved back. A running app keeps the files it started
# with.
install_app() {
    local built=$1 name=$2 app="$app_dir/$2"
    local work status=0
    mkdir -p "$app_dir" || return $?
    work=$(mktemp -d "$app_dir/.$name.XXXXXX") || return $?
    if [ "$system" = Darwin ]; then
        ditto "$built" "$work/new" || status=$?
    else
        cp -R "$built" "$work/new" || status=$?
    fi
    if [ "$status" -eq 0 ] && [ -e "$app" ]; then
        mv "$app" "$work/old" || status=$?
    fi
    if [ "$status" -eq 0 ]; then
        mv "$work/new" "$app" || status=$?
        if [ "$status" -ne 0 ] && [ -e "$work/old" ]; then
            if ! mv "$work/old" "$app"; then
                echo "Could not restore $app; previous copy is at $work/old" >&2
                rm -rf "$work/new"
                return "$status"
            fi
        fi
    fi
    rm -rf "$work"
    [ "$status" -eq 0 ] || return "$status"
    echo "Installed $name in $app_dir"
}

# A path as one argument of a launcher entry's Exec: quoted, with what the
# Desktop Entry Specification reserves inside quotes escaped, then escaped
# again as the entry's string. A path the entry cannot hold everywhere, with
# a control character, `=` or `%`, is refused.
exec_quoted() {
    local path=$1
    if [[ $path == *[[:cntrl:]=%]* ]]; then
        echo "A launcher entry cannot run $path: choose another RITORNEL_APP_DIR" >&2
        return 1
    fi
    path=${path//\\/\\\\}
    path=${path//\"/\\\"}
    path=${path//\`/\\\`}
    path=${path//\$/\\\$}
    path=${path//\\/\\\\}
    printf '"%s"' "$path"
}

main() {
    bin_dir="${RITORNEL_BIN_DIR:-$HOME/.local/bin}"
    system=$(uname -s)
    case $system in
        Darwin) app_dir="${RITORNEL_APP_DIR:-/Applications}" ;;
        *) app_dir="${RITORNEL_APP_DIR:-$HOME/.local/lib}" ;;
    esac
    # Where it is run from, before it moves to the checkout.
    [[ $bin_dir == /* ]] || bin_dir=$PWD/$bin_dir
    [[ $app_dir == /* ]] || app_dir=$PWD/$app_dir
    cd "$(dirname "$0")/.."

    # Checked before anything is installed. The Linux app is installed only if
    # `scripts/desktop.sh app` just built it: the server in its bundle is the
    # one built now.
    local bundle=apps/desktop/target/release/bundle/ritornel
    local linux_app=false
    local launch=""
    if [ "$system" = Darwin ]; then
        refuse_unless_app Ritornel.app Contents/MacOS/ritornel-desktop
    elif [ "$system" = Linux ]; then
        if cmp -s target/release/ritornel-server "$bundle/ritornel-server"; then
            linux_app=true
            refuse_unless_app ritornel ritornel-desktop
            launch=$(exec_quoted "$app_dir/ritornel/ritornel-desktop")
        else
            echo "The desktop app was not built with this build: installing the programs alone"
        fi
    fi

    mkdir -p "$bin_dir"
    install -m 755 target/release/ritornel-server target/release/ritornelctl "$bin_dir/"
    echo "Installed ritornel-server and ritornelctl in $bin_dir"
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) echo "Note: $bin_dir is not on PATH" ;;
    esac

    if [ "$system" = Darwin ]; then
        install_app apps/desktop/target/release/bundle/Ritornel.app Ritornel.app
    elif $linux_app; then
        install_app "$bundle" ritornel
        # The entry and the icon are named for the window's app ID, which is
        # how a launcher, a Wayland compositor and MPRIS match them up. An
        # XDG_DATA_HOME that is not absolute is ignored, as the XDG Base
        # Directory Specification says.
        local id=dev.ritornel.desktop
        local share=$HOME/.local/share
        [[ ${XDG_DATA_HOME:-} != /* ]] || share=$XDG_DATA_HOME
        local icons=$share/icons/hicolor
        mkdir -p "$share/applications" "$icons/512x512/apps"
        install -m 644 "$app_dir/ritornel/icon.png" "$icons/512x512/apps/$id.png"
        # A cache that is there and stale would hide the icon.
        if [ -f "$icons/icon-theme.cache" ] && command -v gtk-update-icon-cache >/dev/null; then
            gtk-update-icon-cache --force --quiet "$icons" || true
        fi
        local entry entry_stage
        entry=$(<"apps/desktop/bundle/$id.desktop")
        entry_stage=$(mktemp "$share/applications/.$id.desktop.XXXXXX")
        if printf '%s\n' "${entry/EXEC/"$launch"}" >"$entry_stage" &&
            chmod 644 "$entry_stage" &&
            mv "$entry_stage" "$share/applications/$id.desktop"; then
            echo "Installed the launcher entry $id.desktop in $share/applications"
        else
            rm -f "$entry_stage"
            exit 1
        fi
    fi

    # The installed client may already speak a newer contract than the server.
    if pgrep -u "$(id -u)" -x ritornel-server >/dev/null 2>&1; then
        echo "Warning: ritornel-server is running. Restart it to use the installed build. Quit and reopen Ritornel if it started the server, reinstall the background service with ritornel-server service install, or restart a server started by hand." >&2
    fi
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
