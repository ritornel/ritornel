#!/usr/bin/env bash
# Checks the contract version against main's: a wire that differs from
# main's is a new version, one above main's, however many commits or pull
# requests it takes to land. So builds with one number speak one wire, and
# a change made in several steps still lands as one raise.
#
# contract/contract.version holds the version and a digest of the wire, and
# a test checks that it is this build's (`mise run contract` writes it).
# This compares it with main's as this work builds on it: the commit where
# it leaves origin/main, which CI fetches first, so a change landing on main
# since does not fail it. $RITORNEL_CONTRACT_BASE names another base.

set -euo pipefail

cd "$(dirname "$0")/.."

file=contract/contract.version
if [[ -n ${RITORNEL_CONTRACT_BASE:-} ]]; then
    base=$RITORNEL_CONTRACT_BASE
elif ! base=$(git merge-base HEAD origin/main 2>/dev/null); then
    echo "error: cannot find where this leaves origin/main: fetch it (git fetch origin main), or set RITORNEL_CONTRACT_BASE" >&2
    exit 1
fi

field() {
    # The value of a line `<name> <value>`, or nothing.
    awk -v name="$1" '$1 == name { print $2 }'
}

if ! theirs=$(git show "$base:$file" 2>/dev/null); then
    echo "error: cannot read $file at $base: fetch it (git fetch origin main), or set RITORNEL_CONTRACT_BASE" >&2
    exit 1
fi
ours=$(cat "$file")

base_version=$(field version <<<"$theirs")
base_digest=$(field digest <<<"$theirs")
version=$(field version <<<"$ours")
digest=$(field digest <<<"$ours")

if ! [[ $base_version =~ ^[0-9]+$ && $version =~ ^[0-9]+$ && -n $base_digest && -n $digest ]]; then
    echo "error: $file here or at $base does not hold a version and a digest" >&2
    exit 1
fi

if ((version < base_version)); then
    echo "error: the contract is at version $version, behind $base's $base_version: rebase" >&2
    exit 1
fi
if ((version == base_version)) && [[ $digest != "$base_digest" ]]; then
    echo "error: the wire differs from $base's and CONTRACT_VERSION is still $base_version: raise it to $((base_version + 1)) in crates/ritornel-contract/src/lib.rs, then run \`mise run contract\`" >&2
    exit 1
fi
if ((version > base_version + 1)); then
    echo "error: the contract is at version $version, and $base's is $base_version: a change lands as one raise, to $((base_version + 1))" >&2
    exit 1
fi
echo "Contract version: ok ($version, $base at $base_version)."
