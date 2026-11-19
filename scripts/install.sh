#!/usr/bin/env bash
# Installs this checkout's release build for everyday use: ritornel-server and
# ritornelctl in ~/.local/bin, and on a Mac Ritornel.app in /Applications.
# The app carries its own ritornel-server and starts it when none is running;
# the one in ~/.local/bin is for the background service (`ritornel-server
# service install`) and a headless hub. `mise run install` builds them first.
# RITORNEL_BIN_DIR and RITORNEL_APP_DIR choose other places.
#
# A server that is running keeps the build it started with: this says so and
# leaves it running.
set -euo pipefail
cd "$(dirname "$0")/.."

bin_dir="${RITORNEL_BIN_DIR:-$HOME/.local/bin}"
app_dir="${RITORNEL_APP_DIR:-/Applications}"

mkdir -p "$bin_dir"
install -m 755 target/release/ritornel-server target/release/ritornelctl "$bin_dir/"
echo "Installed ritornel-server and ritornelctl in $bin_dir"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "Note: $bin_dir is not on PATH" ;;
esac

if [ "$(uname -s)" = Darwin ]; then
    # Staged beside the old copy and swapped in, so no file of an older
    # build is left inside the new one.
    built=apps/desktop/target/release/bundle/Ritornel.app
    app="$app_dir/Ritornel.app"
    rm -rf "$app.new" "$app.old"
    ditto "$built" "$app.new"
    [ ! -e "$app" ] || mv "$app" "$app.old"
    mv "$app.new" "$app"
    rm -rf "$app.old"
    echo "Installed Ritornel.app in $app_dir"
fi

if "$bin_dir/ritornelctl" status >/dev/null 2>&1; then
    echo "A server is running on the old build. To use this one: quit Ritornel if it started the server, or run ritornel-server service install again if it is the background service, or stop one started by hand"
fi
