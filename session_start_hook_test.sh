#!/bin/sh
#
# Tests for .claude/hooks/session-start.sh, the SessionStart hook that brings
# the Rust toolchain up to the floor the workspace declares.
#
# The hook shells out to `rustup update stable`, which downloads a couple of
# hundred megabytes, so no case here lets it reach the real thing: every run
# gets a hermetic PATH carrying stub rustc/rustup binaries. What is checked is
# the hook's decisions — does it leave an adequate toolchain alone, does it
# compare versions numerically rather than lexically, and does it fail loudly
# when it cannot deliver a compiler that can build the tree.
#

_hook="$(dirname "$0")/.claude/hooks/session-start.sh"
_failures=0
_passes=0

_check() {
    _desc=$1
    _expected=$2
    _actual=$3
    if test "$_expected" = "$_actual"; then
        _passes=$((_passes + 1))
    else
        _failures=$((_failures + 1))
        echo "FAIL: $_desc"
        echo "  expected: $_expected"
        echo "  actual:   $_actual"
    fi
}

_contains() {
    _desc=$1
    _needle=$2
    _haystack=$3
    case "$_haystack" in
        *"$_needle"*) _passes=$((_passes + 1)) ;;
        *)
            _failures=$((_failures + 1))
            echo "FAIL: $_desc"
            echo "  expected to contain: $_needle"
            echo "  actual:              $_haystack"
            ;;
    esac
}

_lacks() {
    _desc=$1
    _needle=$2
    _haystack=$3
    case "$_haystack" in
        *"$_needle"*)
            _failures=$((_failures + 1))
            echo "FAIL: $_desc"
            echo "  expected NOT to contain: $_needle"
            echo "  actual:                  $_haystack"
            ;;
        *) _passes=$((_passes + 1)) ;;
    esac
}

if ! test -x "$_hook"; then
    echo "SKIP: session-start.sh missing or not executable"
    exit 0
fi

# Every case below runs the hook directly, which says nothing about whether
# anything ever calls it. Claude Code does not execute a script just because it
# sits under .claude/hooks -- it runs what settings.json registers -- so
# without this the whole suite can pass while the hook is dead code and remote
# sessions keep the stale toolchain it exists to fix.
_settings="$(dirname "$0")/.claude/settings.json"
if command -v python3 >/dev/null 2>&1; then
    _registered=$(python3 - "$_settings" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as f:
        settings = json.load(f)
except (OSError, ValueError) as exc:
    print(f"unreadable: {exc}")
    sys.exit(0)
commands = [
    hook.get("command", "")
    for matcher in settings.get("hooks", {}).get("SessionStart", [])
    for hook in matcher.get("hooks", [])
]
print("yes" if any("session-start.sh" in c for c in commands) else "no")
PY
)
    _check "settings.json registers the hook to run at session start" "yes" "$_registered"
else
    echo "SKIP: hook-registration case (python3 not installed)"
fi

# A sandbox whose PATH is built from scratch, so `command -v rustc` answers
# what the case under test says and not what this machine happens to have.
# /usr/bin is deliberately NOT on it: a real rustc there would make every
# "toolchain is stale" case silently assert the wrong branch.
#
# Two disjoint sets, and the split is load-bearing rather than tidy. Symlinked
# names stay real; stubbed names are written with `>`, which follows a symlink
# and overwrites whatever it points at. A name appearing in both lists would
# therefore destroy the system binary of that name -- not the test, the actual
# file under /usr/bin. Keep _REAL and _STUBBED disjoint; _sandbox enforces it
# rather than trusting a reader to notice.
_REAL='grep sed cat mkdir rm date sleep'
_STUBBED='rustc rustup timeout'

# Checked once, at the top level, so a violation stops the run outright. The
# same test inside _sandbox would only exit its command substitution and let
# the suite carry on toward the write that does the damage.
for _real in $_REAL; do
    for _stub in $_STUBBED; do
        if test "$_real" = "$_stub"; then
            echo "BUG: $_real is both symlinked and stubbed; writing the stub" \
                 "would follow the symlink and overwrite /usr/bin/$_real" >&2
            exit 1
        fi
    done
done

_sandbox() {
    _dir=$(mktemp -d)
    mkdir -p "$_dir/bin"
    for _real in $_REAL; do
        _path=$(command -v "$_real") || continue
        ln -s "$_path" "$_dir/bin/$_real"
    done
    # `timeout` is under test -- the hook must bound anything that can reach
    # the network -- so it is stubbed rather than linked. Recording the whole
    # command and then exec'ing it keeps every other case behaving as if
    # timeout were real. The log is separate from the rustup one: the version
    # probe is bounded too, so sharing a file would make "rustup was never
    # called" indistinguishable from "something was bounded".
    printf '#!/bin/sh\necho "bounded $*" >>"%s/bounds"\nshift\nexec "$@"\n' \
        "$_dir" >"$_dir/bin/timeout"
    chmod +x "$_dir/bin/timeout"
    echo "$_dir"
}

# A manifest carrying just the floor the hook reads.
_manifest() {
    mkdir -p "$1"
    printf '[workspace.package]\nrust-version = "%s"\n' "$2" >"$1/Cargo.toml"
}

# A rustc that reports whatever is in the version file, so the rustup stub can
# change its answer the way a real update would.
_stub_rustc() {
    printf '#!/bin/sh\ntest -f "%s/version" || exit 1\necho "rustc $(cat "%s/version") (abc1234 2026-01-01)"\n' \
        "$1" "$1" >"$1/bin/rustc"
    chmod +x "$1/bin/rustc"
}

# A rustup that logs its arguments and moves rustc to $2. Pass an empty $2 to
# leave the version alone — that models `rustup update` succeeding with stable
# already at its newest.
_stub_rustup() {
    if test -n "$2"; then
        printf '#!/bin/sh\necho "rustup $*" >>"%s/calls"\necho "%s" >"%s/version"\nexit 0\n' \
            "$1" "$2" "$1" >"$1/bin/rustup"
    else
        printf '#!/bin/sh\necho "rustup $*" >>"%s/calls"\nexit 0\n' "$1" >"$1/bin/rustup"
    fi
    chmod +x "$1/bin/rustup"
}

_run() {
    CLAUDE_CODE_REMOTE=true MESH_REPO="$1/repo" PATH="$1/bin" "$_hook" 2>&1 >/dev/null
}

# A local checkout has whatever toolchain its owner installed; only the
# ephemeral remote container starts from a stale image. Getting this wrong
# means silently rewriting a developer's own default toolchain.
CLAUDE_CODE_REMOTE='' "$_hook" >/dev/null 2>&1
_check "does nothing when not in a remote container" 0 $?

_out=$(CLAUDE_CODE_REMOTE='' "$_hook" 2>&1)
_check "stays silent when not in a remote container" "" "$_out"

# An adequate toolchain is left alone, and says which one it is: the version
# the session actually built against belongs in the log, so a result that
# disagrees with CI has its explanation sitting right there.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
_stub_rustup "$_d" 1.99
echo 1.97.1 >"$_d/version"
_err=$(_run "$_d")
_check "exits cleanly when the toolchain already meets the floor" 0 $?
_contains "names the installed toolchain" "1.97.1" "$_err"
_check "never invokes rustup when the toolchain is adequate" "" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

# The case this hook exists for: the image's stable predates the floor.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
_stub_rustup "$_d" 1.97.1
echo 1.94.1 >"$_d/version"
_err=$(_run "$_d")
_check "exits cleanly after updating a stale toolchain" 0 $?
_contains "says the toolchain was too old" "older than" "$_err"
_contains "reports the version it updated to" "1.97.1" "$_err"
_contains "updates the stable channel" "update stable" "$(cat "$_d/calls" 2>/dev/null)"
# A stalled mirror would otherwise hold up session startup indefinitely, with
# nothing on screen to explain the wait.
# Only the timeout stub writes to `bounds`, so a command appearing there is
# proof it was wrapped. The duration is not asserted here because it is now
# whatever remains of the shared budget; the cases below cover that.
_contains "bounds the toolchain download" "rustup update stable" \
    "$(cat "$_d/bounds" 2>/dev/null)"
# The probe needs the same bound, and for a less obvious reason: the `rustc` on
# PATH is normally rustup's shim, which reads rust-toolchain.toml and installs
# the channel it names before printing anything. Unwrapped, it both escapes the
# bound and performs the install on a path that never meant to do one.
_contains "bounds the version probe too" "rustc --version" \
    "$(cat "$_d/bounds" 2>/dev/null)"
rm -rf "$_d"

# One deadline for startup, not one per command. `timeout` bounds a single
# invocation, so reusing the same number for the probe and the update bounds
# the pair at twice it -- a ten-minute promise that permits twenty. A probe
# that burns time must leave the update correspondingly less.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
# A probe that takes 2s of a 5s budget, so the update must get at most 3.
printf '#!/bin/sh\nsleep 2\ntest -f "%s/version" || exit 1\necho "rustc $(cat "%s/version") (abc1234 2026-01-01)"\n' \
    "$_d" "$_d" >"$_d/bin/rustc"
chmod +x "$_d/bin/rustc"
_stub_rustup "$_d" 1.97.1
echo 1.94.1 >"$_d/version"
CLAUDE_CODE_REMOTE=true MESH_REPO="$_d/repo" MESH_TOOLCHAIN_BUDGET=5 \
    PATH="$_d/bin" "$_hook" >/dev/null 2>&1
_update_budget=$(grep 'rustup update' "$_d/bounds" 2>/dev/null | sed -E 's/bounded ([0-9]+).*/\1/')
if test -n "$_update_budget" && test "$_update_budget" -le 3 && test "$_update_budget" -ge 1; then
    _passes=$((_passes + 1))
else
    _failures=$((_failures + 1))
    echo "FAIL: the update inherits what the probe left of the budget"
    echo "  expected: 1..3 seconds remaining of a 5s budget after a 2s probe"
    echo "  actual:   ${_update_budget:-<no bounded rustup call>}"
fi
rm -rf "$_d"

# Once the budget is gone, starting a download that cannot finish inside it
# just adds to the wait. Refuse, and say which call was dropped and why.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
printf '#!/bin/sh\nsleep 2\ntest -f "%s/version" || exit 1\necho "rustc $(cat "%s/version") (abc1234 2026-01-01)"\n' \
    "$_d" "$_d" >"$_d/bin/rustc"
chmod +x "$_d/bin/rustc"
_stub_rustup "$_d" 1.97.1
echo 1.94.1 >"$_d/version"
_err=$(CLAUDE_CODE_REMOTE=true MESH_REPO="$_d/repo" MESH_TOOLCHAIN_BUDGET=1 \
    PATH="$_d/bin" "$_hook" 2>&1 >/dev/null)
_check "exits non-zero when the budget runs out" 1 $?
_contains "says the budget is what stopped it" "budget is spent" "$_err"
_check "starts no download it cannot finish" "" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

# Version comparison is numeric, not lexical. Both of these are wrong under a
# string compare, in opposite directions, and either one silently ruins the
# decision: 1.100 would be judged older than 1.95, and 1.9 newer than it.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
_stub_rustup "$_d" 1.101
echo 1.100.0 >"$_d/version"
_run "$_d" >/dev/null 2>&1
_check "treats 1.100 as newer than the 1.95 floor" "" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
_stub_rustup "$_d" 1.97.1
echo 1.9.0 >"$_d/version"
_err=$(_run "$_d")
_contains "treats 1.9 as older than the 1.95 floor" "update stable" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

# A two-component floor is satisfied by that release's .0 patch: the missing
# field reads as 0, not as "unset and therefore smaller".
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
_stub_rustup "$_d" 1.99
echo 1.95.0 >"$_d/version"
_err=$(_run "$_d")
_check "accepts 1.95.0 against a 1.95 floor" 0 $?
_check "does not update for a satisfied two-component floor" "" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

# A nightly's pre-release suffix has to come off before the patch field is
# compared numerically, or `test 0-nightly -lt 0` aborts the compare.
#
# The floor here has to match the nightly's own major.minor. Against a lower
# floor the comparison returns at the minor field and never reads the patch,
# so the suffix is never parsed and the case proves nothing -- which is what
# an earlier version of this test did.
_d=$(_sandbox)
_manifest "$_d/repo" 1.98
_stub_rustc "$_d"
_stub_rustup "$_d" 1.99
echo 1.98.0-nightly >"$_d/version"
_err=$(_run "$_d")
_check "accepts a nightly of the floor release" 0 $?
_lacks "does not error on a nightly version string" "integer expression" "$_err"
_check "does not update for an adequate nightly" "" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

# Without rustup there is no way to fix the toolchain, and nothing in the tree
# compiles. Saying so now beats a cargo error later with no link back to here.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
echo 1.94.1 >"$_d/version"
_err=$(_run "$_d")
_check "exits non-zero when rustup is missing and rust is too old" 1 $?
_contains "says rustup is what is missing" "rustup is not installed" "$_err"
rm -rf "$_d"

# A failed update is fatal for the same reason.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
printf '#!/bin/sh\nexit 1\n' >"$_d/bin/rustup"
chmod +x "$_d/bin/rustup"
echo 1.94.1 >"$_d/version"
_err=$(_run "$_d")
_check "exits non-zero when the update fails" 1 $?
_contains "says the update failed" "failed" "$_err"
rm -rf "$_d"

# rustup exits 0 when stable is already newest, so a floor that has outrun the
# released compiler leaves the session looking fixed and still unable to
# build. Verifying after the update is the only thing that catches it.
_d=$(_sandbox)
_manifest "$_d/repo" 1.99
_stub_rustc "$_d"
_stub_rustup "$_d" ""
echo 1.97.1 >"$_d/version"
_err=$(_run "$_d")
_check "exits non-zero when the update leaves rust below the floor" 1 $?
_contains "names the version that is still too old" "still below" "$_err"
rm -rf "$_d"

# A rustc on PATH that will not execute is a broken toolchain, not an absent
# one; it must reach the install path rather than being accepted on the
# strength of `command -v` alone.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
rm -f "$_d/version" # the stub exits 1 without it
_stub_rustup "$_d" 1.97.1
_err=$(_run "$_d")
_check "exits cleanly after replacing a rustc that will not run" 0 $?
_contains "says no usable rustc was found" "no usable rustc" "$_err"
_contains "installs a toolchain for an unusable rustc" "update stable" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

# Without `timeout` the work still has to happen -- refusing to run would make
# a missing coreutils binary fatal to every session -- but the lost bound is
# said out loud, since an unbounded download that stalls is otherwise exactly
# the invisible startup hang the wrapper exists to prevent.
_d=$(_sandbox)
_manifest "$_d/repo" 1.95
_stub_rustc "$_d"
_stub_rustup "$_d" 1.97.1
echo 1.94.1 >"$_d/version"
rm -f "$_d/bin/timeout"
_err=$(_run "$_d")
_check "still updates when timeout is unavailable" 0 $?
_contains "says the bound was lost" "will not be bounded" "$_err"
_contains "updates anyway without timeout" "update stable" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

# No floor declared means nothing to enforce; touching the toolchain then
# would be acting on a number the workspace never asked for.
_d=$(_sandbox)
mkdir -p "$_d/repo"
printf '[workspace.package]\nedition = "2021"\n' >"$_d/repo/Cargo.toml"
_stub_rustc "$_d"
_stub_rustup "$_d" 1.99
echo 1.94.1 >"$_d/version"
_err=$(_run "$_d")
_check "exits cleanly when the manifest declares no floor" 0 $?
_check "never invokes rustup without a declared floor" "" "$(cat "$_d/calls" 2>/dev/null)"
rm -rf "$_d"

if test $_failures -gt 0; then
    echo "session-start hook: $_failures test(s) failed, $_passes passed."
    exit 1
fi
echo "session-start hook: all $_passes tests passed."
