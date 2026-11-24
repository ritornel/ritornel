#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# shellcheck source=../install.sh
source "$script_dir/../install.sh"

system=$(uname -s)
name=Ritornel.app
[[ $system == Darwin ]] || name=ritornel

make_bundle() {
    mkdir -p "$1"
    printf '%s\n' "$2" >"$1/version"
}

built_v1=$tmp/built-v1/$name
built_v2=$tmp/built-v2/$name
make_bundle "$built_v1" v1
printf 'stale\n' >"$built_v1/stale"
make_bundle "$built_v2" v2

# 1. Pre-existing .new and .old siblings survive an install, with no temp folders left behind.
(
    app_dir=$tmp/siblings
    mkdir -p "$app_dir/$name.new" "$app_dir/$name.old"
    printf 'keep-new\n' >"$app_dir/$name.new/sentinel"
    printf 'keep-old\n' >"$app_dir/$name.old/sentinel"
    cp -R "$built_v1" "$app_dir/$name"

    install_app "$built_v2" "$name" >/dev/null

    [[ $(<"$app_dir/$name/version") == v2 ]]
    [[ ! -e "$app_dir/$name/stale" ]]
    [[ $(<"$app_dir/$name.new/sentinel") == keep-new ]]
    [[ $(<"$app_dir/$name.old/sentinel") == keep-old ]]
    [[ -z $(find "$app_dir" -maxdepth 1 -mindepth 1 -name '.*') ]]
)

# 2. A failed publish restores the previous app and leaves siblings and temp folders clean.
(
    app_dir=$tmp/fail-publish
    hit=$tmp/publish.hit
    mkdir -p "$app_dir/$name.new" "$app_dir/$name.old"
    printf 'keep-new\n' >"$app_dir/$name.new/sentinel"
    printf 'keep-old\n' >"$app_dir/$name.old/sentinel"
    cp -R "$built_v1" "$app_dir/$name"

    # shellcheck disable=SC2329 # invoked indirectly by install_app
    mv() {
        if [[ $1 == */new && $2 == "$app_dir/$name" ]]; then
            echo hit >"$hit"
            return 42
        fi
        command mv "$@"
    }

    rc=0
    install_app "$built_v2" "$name" >/dev/null 2>&1 || rc=$?
    [[ $rc -eq 42 && -f "$hit" ]]
    [[ $(<"$app_dir/$name/version") == v1 ]]
    [[ $(<"$app_dir/$name/stale") == stale ]]
    [[ $(<"$app_dir/$name.new/sentinel") == keep-new ]]
    [[ $(<"$app_dir/$name.old/sentinel") == keep-old ]]
    [[ -z $(find "$app_dir" -maxdepth 1 -mindepth 1 -name '.*') ]]
)
