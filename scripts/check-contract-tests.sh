#!/usr/bin/env bash
# Tests scripts/check-contract.sh against made-up versions of main's
# contract/contract.version and this one, in a scratch repository.

set -euo pipefail

script="$(cd "$(dirname "$0")" && pwd)/check-contract.sh"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

git -C "$scratch" init --quiet
mkdir -p "$scratch/scripts" "$scratch/contract"
cp "$script" "$scratch/scripts/"
version_file() {
    printf '# A comment.\nversion %s\ndigest %s\n' "$1" "$2" >"$scratch/contract/contract.version"
}
version_file 2 aaaa
git -C "$scratch" add contract
git -C "$scratch" -c user.name=test -c user.email=test@example.invalid commit --quiet -m main
git -C "$scratch" tag base

failed=0
expect() {
    local want=$1 version=$2 digest=$3 what=$4
    version_file "$version" "$digest"
    if RITORNEL_CONTRACT_BASE=base "$scratch/scripts/check-contract.sh" >/dev/null 2>&1; then
        got=pass
    else
        got=fail
    fi
    if [[ $got != "$want" ]]; then
        echo "error: check-contract.sh should $want $what, and did not" >&2
        failed=1
    fi
}

expect pass 2 aaaa "the same wire at main's version"
expect fail 2 bbbb "another wire at main's version"
expect pass 3 bbbb "another wire one version above main's"
expect pass 3 cccc "a wire changed again one version above main's"
expect fail 4 bbbb "a second raise"
expect fail 1 aaaa "a version behind main's"

RITORNEL_CONTRACT_BASE=nowhere "$scratch/scripts/check-contract.sh" >/dev/null 2>&1 && {
    echo "error: check-contract.sh should fail with no base to read" >&2
    failed=1
}

exit "$failed"
