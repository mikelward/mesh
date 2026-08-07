#!/bin/sh
#
# Tests for rust-toolchain.toml and the CI jobs that depend on what it says.
#
# The pin is what makes a stale container install a working compiler: rustup
# resolves `stable` to whatever it already has and never looks for a newer one,
# but an exact release it does not have, it downloads. That only holds while the
# pin stays at or above the MSRV floor and while the jobs that deliberately
# override it keep doing so -- none of which any build failure would report,
# because a pin below the floor fails as a cargo error about `rust-version` and
# a job that has quietly stopped overriding just passes.
#

_root="$(dirname "$0")"
_toolchain="$_root/rust-toolchain.toml"
_manifest="$_root/Cargo.toml"
_ci="$_root/.github/workflows/ci.yml"
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

# Is $1 lower than $2, field by field and numerically? A string compare gets
# this wrong in both directions -- it sorts 1.9 above 1.95 and 1.100 below it --
# and `test` has no notion of a dotted version. A missing field reads as 0, so a
# two-component floor of 1.95 is met by 1.95.0.
_version_lt() {
    _i=1
    while test "$_i" -le 3; do
        _a=$(echo "$1" | cut -d. -f"$_i")
        _b=$(echo "$2" | cut -d. -f"$_i")
        test -n "$_a" || _a=0
        test -n "$_b" || _b=0
        if test "$_a" -lt "$_b"; then return 0; fi
        if test "$_a" -gt "$_b"; then return 1; fi
        _i=$((_i + 1))
    done
    return 1
}

if ! test -f "$_toolchain"; then
    echo "FAIL: $_toolchain is missing"
    exit 1
fi

_channel=$(sed -n 's/^channel *= *"\(.*\)".*/\1/p' "$_toolchain")
_floor=$(sed -n 's/^rust-version *= *"\(.*\)".*/\1/p' "$_manifest")

# An exact release is the whole point. `stable` reads like a pin and is not one:
# rustup resolves it against the toolchains already installed, so a container
# built before the floor moved resolves it to a compiler that cannot build the
# tree and never notices there is a newer one to fetch.
_exact=no
case "$_channel" in
    [0-9]*.[0-9]*.[0-9]*) _exact=yes ;;
esac
_check "rust-toolchain.toml pins an exact major.minor.patch release" \
    "yes" "$_exact"

# The floor is read from the manifest rather than repeated here, the same way
# the msrv CI job reads it. A pin below it means nothing builds, and the error
# arrives as a cargo message about rust-version with no mention of this file.
_check "Cargo.toml declares an MSRV floor" "yes" \
    "$(test -n "$_floor" && echo yes || echo no)"
# Guarded on $_exact, not just on emptiness: `test stable -lt 1` is not a
# comparison that returns false, it is an "Illegal number" error on stderr, and
# the case above has already reported a non-numeric channel.
if test "$_exact" = yes && test -n "$_floor"; then
    if _version_lt "$_channel" "$_floor"; then
        _failures=$((_failures + 1))
        echo "FAIL: the pinned release is at or above the MSRV floor"
        echo "  expected: >= $_floor"
        echo "  actual:   $_channel"
    else
        _passes=$((_passes + 1))
    fi
fi

# CI runs `cargo fmt` and `cargo clippy` against the pinned toolchain, so the
# pin has to carry them. Without these a fresh machine installs a toolchain the
# format and lint gates cannot run on.
_toolchain_text=$(cat "$_toolchain")
_contains "the pin includes rustfmt" "rustfmt" "$_toolchain_text"
_contains "the pin includes clippy" "clippy" "$_toolchain_text"

if test -f "$_ci"; then
    # Reported as yes/no rather than with _contains, whose failure output would
    # be the entire workflow file.
    _has_gate() {
        case "$(cat "$_ci")" in
            *"$1"*) echo yes ;;
            *) echo no ;;
        esac
    }

    # Pinning trades away the early warning that a new compiler broke
    # something, and the `+stable` job is what buys it back. If that override
    # is dropped the job inherits the pin, passes for the wrong reason, and the
    # warning is gone with nothing to say so.
    _check "CI still gates on stable, overriding the pin" \
        "yes" "$(_has_gate 'cargo +stable')"

    # The msrv job has to override the pin for the same reason in reverse:
    # inheriting it would build against the pinned release and report the floor
    # as fine whatever the floor actually is.
    _check "the msrv job overrides the pin rather than inheriting it" \
        "yes" "$(_has_gate 'cargo +${{ steps.msrv.outputs.version }}')"
else
    echo "SKIP: no CI workflow to compare against"
fi

if test $_failures -gt 0; then
    echo "toolchain: $_failures test(s) failed, $_passes passed."
    exit 1
fi
echo "toolchain: all $_passes tests passed."
