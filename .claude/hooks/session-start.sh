#!/bin/bash
# Bring the Rust toolchain up to what the workspace actually requires.
#
# This is now a backstop, not the mechanism. `rust-toolchain.toml` pins an exact
# release, and rustup installs a pinned release it does not have on the first
# cargo command, so the toolchain normally fixes itself with nothing running
# here. What this still catches is a pin that has fallen below the workspace's
# own `rust-version`, where cargo fails before compiling a line:
#
#     error: rustc 1.94.1 is not supported by the following packages:
#       mesh@0.0.0 requires rustc 1.95
#
# Nothing builds and no test runs, so an unfixable toolchain is fatal here
# rather than a thinner session: this hook reports and exits non-zero instead
# of letting the failure surface later as a confusing cargo error.
#
# Whether this hook is worth keeping at all is an open question in TODO.md; it
# is also not registered in a session whose project root sits above the repo.
set -euo pipefail

# Overridable so the tests can drive every branch without needing a machine
# whose toolchain happens to be stale. `checkout` stays the real tree either
# way -- it is where this script's siblings live, and a seam pointing the
# manifest at a fixture must not also move where the hook finds its own files.
checkout=$(cd "$(dirname "$0")/../.." && pwd)
repo=${MESH_REPO:-$checkout}

# Deepen the clone before anything reads history -- see scripts/unshallow.sh.
#
# Above the remote-only guard on purpose, unlike everything below it. A shallow
# clone answers `git rev-list --count`, `git log` past the boundary and blame
# with a confident wrong number and no warning, and that is true of the clone
# rather than of the sandbox that made it -- so there is nothing to gain by
# asking where it came from first. On a complete clone it is a no-op that says
# so. Everything below is toolchain provisioning, which a local machine really
# does manage itself.
#
# Run directly rather than through `sh` so it needs nothing on PATH, and
# best-effort: it reports its own failures, and a clone it could not deepen is
# never a reason to refuse to start the session.
(cd "$repo" && "$checkout/scripts/unshallow.sh") || true

# Local checkouts manage their own toolchain; only the ephemeral remote
# container starts from a stale image.
if test "${CLAUDE_CODE_REMOTE:-}" != "true"; then
    exit 0
fi

manifest="$repo/Cargo.toml"

# Read the floor from the manifest rather than repeating it here, the same way
# .github/workflows/ci.yml's msrv job does. A second copy of the number is a
# second thing to forget when the floor moves, and the two disagreeing is
# invisible until a session silently builds against the wrong compiler.
required=$(grep '^rust-version' "$manifest" 2>/dev/null | sed -E 's/.*"(.*)".*/\1/' || true)
if test -z "$required"; then
    echo "session-start: no rust-version in $manifest; leaving the toolchain alone" >&2
    exit 0
fi

# Everything that can reach the network goes through this, because this runs
# before the session is usable and an unbounded download holds up startup with
# nothing on screen to say why.
#
# It wraps the version probe as well as the update, which is not obvious: the
# `rustc` on PATH is normally rustup's shim, and the shim reads
# rust-toolchain.toml and *installs* the channel it names before printing
# anything. With an empty RUSTUP_HOME a bare `rustc --version` answers
# "syncing channel updates for stable" and starts downloading, so leaving the
# probe unwrapped both escapes the bound and lets the install happen on a path
# that was never meant to perform one.
# The budget is for session startup as a whole, so it is one deadline shared by
# every call rather than a fresh timeout each. `timeout` bounds one invocation;
# handing the same number to two calls bounds the pair at twice it, which is
# not what "startup is bounded by ten minutes" means to whoever is waiting.
# Each call gets what is left, and once nothing is, they stop rather than
# starting work that has no time to finish in.
# Overridable so the tests can exercise exhaustion without waiting on it.
TOOLCHAIN_BUDGET=${MESH_TOOLCHAIN_BUDGET:-600}
toolchain_deadline=$(($(date +%s) + TOOLCHAIN_BUDGET))

if command -v timeout >/dev/null 2>&1; then
    bounded() {
        local remaining=$((toolchain_deadline - $(date +%s)))
        if test "$remaining" -le 0; then
            echo "session-start: the ${TOOLCHAIN_BUDGET}s toolchain budget is spent; not starting \`$*\`" >&2
            return 1
        fi
        timeout "$remaining" "$@"
    }
else
    # Say so rather than dropping the bound silently: the whole point of the
    # wrapper is that a stalled download is otherwise invisible.
    echo "session-start: timeout is not installed; toolchain downloads will not be bounded" >&2
    bounded() { "$@"; }
fi

# rustc prints "rustc 1.97.1 (8bab26f4f 2026-07-14)"; a nightly appends a
# pre-release suffix ("1.98.0-nightly") that has to come off before the
# numeric compare below reads the patch field.
rustc_version() {
    local reported
    reported=$(bounded rustc --version) || return 1
    reported=${reported#rustc }
    reported=${reported%% *}
    echo "${reported%%-*}"
}

# Field-by-field numeric compare. String comparison gets this wrong in both
# directions: it sorts 1.9 above 1.95 and 1.100 below 1.95. A missing field
# reads as 0, so a two-component floor of 1.95 is satisfied by 1.95.0.
version_lt() {
    local a1 a2 a3 b1 b2 b3
    IFS=. read -r a1 a2 a3 <<<"$1"
    IFS=. read -r b1 b2 b3 <<<"$2"
    a1=${a1:-0}; a2=${a2:-0}; a3=${a3:-0}
    b1=${b1:-0}; b2=${b2:-0}; b3=${b3:-0}
    if test "$a1" -lt "$b1"; then return 0; fi
    if test "$a1" -gt "$b1"; then return 1; fi
    if test "$a2" -lt "$b2"; then return 0; fi
    if test "$a2" -gt "$b2"; then return 1; fi
    if test "$a3" -lt "$b3"; then return 0; fi
    return 1
}

installed=""
if command -v rustc >/dev/null 2>&1; then
    # A rustc that is on PATH but will not run is a broken toolchain, not an
    # absent one. Leaving `installed` empty routes it down the install path,
    # which is the branch that can actually report something useful.
    installed=$(rustc_version) || installed=""
fi

if test -n "$installed" && ! version_lt "$installed" "$required"; then
    echo "session-start: using the installed Rust toolchain: $installed (workspace needs $required)" >&2
    exit 0
fi

if test -n "$installed"; then
    echo "session-start: Rust $installed is older than the $required this workspace needs; updating stable" >&2
else
    echo "session-start: no usable rustc on PATH; installing the stable toolchain" >&2
fi

if ! command -v rustup >/dev/null 2>&1; then
    echo "session-start: rustup is not installed, so the toolchain cannot be updated; \`cargo build\` and \`cargo test\` will fail" >&2
    exit 1
fi

# Ten minutes is generous for a ~200MB toolchain and still fails in a knowable
# time. rustup has no timeout of its own, hence the wrapper. What is left of
# the budget after the probe is what this gets.
if ! bounded rustup update stable; then
    echo "session-start: \`rustup update stable\` failed; \`cargo build\` and \`cargo test\` will fail" >&2
    exit 1
fi

# Verify rather than assume. rustup exits 0 when stable is already at its
# newest, so a floor that has outrun the released compiler leaves the session
# looking fixed and still unable to build — the one failure worth catching
# here, because otherwise it surfaces as a cargo error with no connection to
# this hook.
installed=$(rustc_version) || installed=""
if test -z "$installed"; then
    echo "session-start: rustc does not run after the update; \`cargo build\` and \`cargo test\` will fail" >&2
    exit 1
fi
if version_lt "$installed" "$required"; then
    echo "session-start: stable is $installed, still below the $required this workspace needs; \`cargo build\` and \`cargo test\` will fail" >&2
    exit 1
fi

echo "session-start: updated the Rust toolchain to $installed (workspace needs $required)" >&2
