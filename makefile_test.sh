#!/bin/sh
#
# Tests for the Makefile, the one-line entry points over cargo.
#
# Nothing here builds anything: every case is a `make -n` dry run, so the suite
# is instant and needs no toolchain. What it checks is that the shortcuts still
# mean what they claim -- that `make install` names a package directory that
# exists, that `make check` really is what CI runs rather than a stale copy of
# it, and that `make help` lists every target instead of the subset someone
# remembered to add.
#

_root="$(dirname "$0")"
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

# `make -n` prints the recipe without running it, so a dry run costs nothing and
# cannot reach cargo. -C keeps the suite runnable from any directory.
_dry() {
    make -C "$_root" --no-print-directory -n "$@" 2>&1
}

if ! command -v make >/dev/null 2>&1; then
    echo "SKIP: make is not installed"
    exit 0
fi

# The headline promise: one word, no flags to recall.
_contains "make install installs the mesh package" \
    "cargo install --locked --path crates/mesh" "$(_dry install)"

# ...and the path in it has to be a real package, or the shortcut is a shortcut
# to an error message. This is the case that catches a crate being moved.
_package=$(make -C "$_root" --no-print-directory -n install | sed -n 's/.*--path //p')
_check "the installed package directory has a manifest" \
    "yes" "$(test -f "$_root/$_package/Cargo.toml" && echo yes || echo no)"
_contains "the installed package is named mesh" \
    'name = "mesh"' "$(cat "$_root/$_package/Cargo.toml")"
_contains "make uninstall removes that same binary" \
    "cargo uninstall mesh" "$(_dry uninstall)"

# A bare `make` should do the harmless thing, not install or clean.
_contains "the default target is a debug build" "cargo build --workspace" "$(_dry)"
_contains "make release builds optimized" "--release" "$(_dry release)"
_contains "make run starts the shell" "cargo run -p mesh" "$(_dry run)"
_contains "make test runs the cargo suites" "cargo test --workspace" "$(_dry test)"
_contains "make test runs the shell suites too" \
    "sh session_start_hook_test.sh" "$(_dry test)"

# check = lint then test, in that order: no point running a suite the formatter
# is about to reject.
_check "make check runs the gates before the tests" \
    "cargo fmt --all -- --check" "$(_dry check | head -1)"
_contains "make check runs clippy" "clippy" "$(_dry check)"
_contains "make check runs the tests" "cargo test --workspace" "$(_dry check)"

# The reason to trust `make check` before pushing is that it is CI. Every
# command it would run has to appear in the workflow -- if CI gains a gate or
# changes a flag, this fails until the Makefile follows.
if test -f "$_ci"; then
    _ci_text=$(cat "$_ci")
    _missing=$(_dry check | while read -r _line; do
        test -n "$_line" || continue
        case "$_ci_text" in
            *"$_line"*) ;;
            *) echo "$_line" ;;
        esac
    done)
    _check "every command make check runs is a command CI runs" "" "$_missing"
else
    echo "SKIP: no CI workflow to compare against"
fi

# help is the discovery path, so a target missing from it is invisible.
_help=$(make -C "$_root" --no-print-directory help)
for _target in $(sed -n 's/^\.PHONY: //p' "$_root/Makefile"); do
    _contains "make help lists $_target" "$_target" "$_help"
done

if test $_failures -gt 0; then
    echo "Makefile: $_failures test(s) failed, $_passes passed."
    exit 1
fi
echo "Makefile: all $_passes tests passed."
