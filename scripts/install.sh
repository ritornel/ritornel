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

# install_app BUILT NAME: copies the app BUILT to $app_dir/NAME, staged
# beside the old copy and swapped in, so that no file of an older build is
# left inside the new one. A running app keeps the files it started with.
install_app() {
    local app="$app_dir/$2"
    mkdir -p "$app_dir"
    rm -rf "$app.new" "$app.old"
    if [ "$system" = Darwin ]; then ditto "$1" "$app.new"; else cp -R "$1" "$app.new"; fi
    [ ! -e "$app" ] || mv "$app" "$app.old"
    mv "$app.new" "$app"
    rm -rf "$app.old"
    echo "Installed $2 in $app_dir"
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

# Checked before anything is installed. The Linux app is installed only if
# `scripts/desktop.sh app` just built it: the server in its bundle is the
# one built now.
bundle=apps/desktop/target/release/bundle/ritornel
linux_app=false
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
    id=dev.ritornel.desktop
    share=$HOME/.local/share
    [[ ${XDG_DATA_HOME:-} != /* ]] || share=$XDG_DATA_HOME
    icons=$share/icons/hicolor
    mkdir -p "$share/applications" "$icons/512x512/apps"
    install -m 644 "$app_dir/ritornel/icon.png" "$icons/512x512/apps/$id.png"
    # A cache that is there and stale would hide the icon.
    if [ -f "$icons/icon-theme.cache" ] && command -v gtk-update-icon-cache >/dev/null; then
        gtk-update-icon-cache --force --quiet "$icons" || true
    fi
    entry=$(<"apps/desktop/bundle/$id.desktop")
    printf '%s\n' "${entry/EXEC/"$launch"}" >"$share/applications/$id.desktop.new"
    mv "$share/applications/$id.desktop.new" "$share/applications/$id.desktop"
    echo "Installed the launcher entry $id.desktop in $share/applications"
fi

if "$bin_dir/ritornelctl" status >/dev/null 2>&1; then
    echo "A server is running on the old build. To use this one: quit Ritornel if it started the server, or run ritornel-server service install again if it is the background service, or stop one started by hand"
fi
