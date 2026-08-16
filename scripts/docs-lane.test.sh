#!/usr/bin/env bash
# Tests for scripts/docs-lane.sh, run by the classify job before it
# classifies anything — so a broken lane rule fails the workflow closed
# instead of skipping the heavy jobs on a diff nobody actually checked.
#
# The harness stubs `gh` on PATH: fixtures arrive as environment variables
# (FILES, SUBJECTS, CHANGED to override the changed_files count, *_FAIL to
# simulate API failures). Its own failure mode is a false pass, so every
# behavior is asserted in both directions — the case that must succeed and
# the case that must be refused.
set -u

here=$(cd "$(dirname "$0")" && pwd)
stub=$(mktemp -d)
cat > "$stub/gh" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *"/files"*)
    [ "${FILES_FAIL:-0}" = 1 ] && exit 1
    # Fixture entries are "newpath" or "newpath:oldpath" (a rename); emit the
    # same TSV shape the real --jq produces.
    for f in $FILES; do
      new=${f%%:*}; old=""
      [ "$f" = "$new" ] || old=${f#*:}
      printf '%s\t%s\n' "$new" "$old"
    done
    ;;
  *"/commits/"*"/pulls"*)
    [ "${PULLS_FAIL:-0}" = 1 ] && exit 1
    # Space-separated fixture of open-PR numbers heading this commit.
    for p in ${SHARED_PRS:-$PR}; do printf '%s\n' "$p"; done
    ;;
  *"/commits"*)
    [ "${COMMITS_FAIL:-0}" = 1 ] && exit 1
    # Fixture lines are "subject" (one parent) or "N:subject"; emit the same
    # parent-count TSV the real --jq produces.
    printf '%s\n' "$SUBJECTS" | while IFS= read -r s; do
      case "$s" in
        [0-9]:*) printf '%s\t%s\n' "${s%%:*}" "${s#*:}" ;;
        *) printf '1\t%s\n' "$s" ;;
      esac
    done
    ;;
  *".head.sha"*)
    [ "${HEAD_FAIL:-0}" = 1 ] && exit 1
    echo "${HEAD_SHA:-headsha}"
    ;;
  *".changed_files"*)
    if [ -n "${CHANGED:-}" ]; then echo "$CHANGED"; else echo $FILES | wc -w; fi
    ;;
  *".commits"*)
    if [ -n "${NCOMMITS:-}" ]; then echo "$NCOMMITS"; else printf '%s' "$SUBJECTS" | grep -c .; fi
    ;;
esac
EOF
chmod +x "$stub/gh"
export PATH="$stub:$PATH" GITHUB_REPOSITORY=example/repo PR=1 GITHUB_EVENT_NAME=pull_request GITHUB_SHA=headsha

fails=0
check() {
  local desc=$1 want_exit=$2 want_out=$3 got got_exit
  shift 3
  # Invoked exactly as CI invokes it — directly, not via `bash` — so a
  # missing executable bit fails here, before a push, instead of as a
  # Permission denied in the classify job.
  got=$(env "$@" "$here/docs-lane.sh" "${MODE:-classify}" 2>&1)
  got_exit=$?
  if [ "$got_exit" -ne "$want_exit" ]; then
    echo "FAIL: $desc — exit $got_exit, wanted $want_exit"; echo "$got" | sed 's/^/    /'
    fails=1
  elif [ -n "$want_out" ] && ! printf '%s' "$got" | grep -qF "$want_out"; then
    echo "FAIL: $desc — output lacks '$want_out'"; echo "$got" | sed 's/^/    /'
    fails=1
  else
    echo "ok: $desc"
  fi
}

# --- classify: the rule itself, both directions per shape
check "markdown-only diff is docs"        0 "docs_only=true"  FILES="README.md docs/DESIGN.md"
check "code file makes it code"           0 "docs_only=false" FILES="README.md crates/mesh/src/main.rs"
check ".gitignore is code like any config" 0 "docs_only=false" FILES=".gitignore README.md"
check "markdown under crates/ is a build input" 0 "docs_only=false" FILES="crates/mesh/README.md"
check "a shared head never rides the docs lane" 0 "docs_only=false" FILES="README.md" SHARED_PRS="1 2"
check "a lone foreign head PR never rides the lane" 0 "docs_only=false" FILES="README.md" SHARED_PRS="2"
check "unlistable head PRs are code on a PR event" 0 "docs_only=false" FILES="README.md" PULLS_FAIL=1
check "an unnamed dotfile is code"         0 "docs_only=false" FILES=".editorconfig"
check ".claude contents are code"          0 "docs_only=false" FILES=".claude/hooks/session-start.sh"
check "nested dot-directory files are code" 0 "docs_only=false" FILES=".cargo/config.toml"
check "markdown is docs even under a dot-directory" 0 "docs_only=true" FILES=".claude/NOTES.md"
check "workflow edits are code"           0 "docs_only=false" FILES=".github/workflows/ci.yml"
check "empty diff is not docs"            0 "docs_only=false" FILES="" CHANGED=0
check "non-PR events are code"            0 "docs_only=false" FILES="README.md" GITHUB_EVENT_NAME=push

# --- classify: a dispatched run judges the PR its input names, or nothing
check "dispatch naming a PR classifies it" 0 "docs_only=true"  FILES="README.md" GITHUB_EVENT_NAME=workflow_dispatch
check "dispatch without a PR is refused"   1 "must name the pull request" FILES="README.md" GITHUB_EVENT_NAME=workflow_dispatch PR=
check "PR-less dispatch is refused even on main" 1 "must name the pull request" FILES="README.md" GITHUB_EVENT_NAME=workflow_dispatch PR= GITHUB_REF_NAME=main
check "shared head is refused"             1 "cannot vouch for exactly one" FILES="README.md" GITHUB_EVENT_NAME=workflow_dispatch SHARED_PRS="1 2"
check "unlistable head PRs are refused"    1 "refusing to report" FILES="README.md" GITHUB_EVENT_NAME=workflow_dispatch PULLS_FAIL=1
check "dispatch for another PR's commit refused" 1 "must not label" FILES="README.md" GITHUB_EVENT_NAME=workflow_dispatch HEAD_SHA=otherhead
check "unreadable head refuses the dispatch" 1 "refusing to report" FILES="README.md" GITHUB_EVENT_NAME=workflow_dispatch HEAD_FAIL=1

# --- classify: renames are judged on both sides
check "md-to-md rename stays docs"        0 "docs_only=true"  FILES="docs/NEW.md:docs/OLD.md"
check "code renamed into docs is code"    0 "docs_only=false" FILES="docs/A.md:crates/mesh/src/a.rs"

# --- classify: an untrustworthy file list must never classify as docs
check "truncated file list is not docs"   0 "docs_only=false" FILES="README.md" CHANGED=3000
check "files API failure is not docs"     0 "docs_only=false" FILES="README.md" FILES_FAIL=1

# --- gate: every verdict combination (RESULTS carries the heavy jobs)
export MODE=gate
ALLOK="check=success msrv=success"
ALLSKIP="check=skipped msrv=skipped"
check "all heavy jobs green passes"       0 "" FILES="a.rs" CLASSIFY=success RESULTS="$ALLOK"
check "justified skip with prefixes"      0 "" FILES="README.md" SUBJECTS="docs: Fix a typo" CLASSIFY=success RESULTS="$ALLSKIP"
check "design prefix rides the lane"      0 "" FILES="docs/DESIGN.md" SUBJECTS="design: Let a block take a label" CLASSIFY=success RESULTS="$ALLSKIP"
check "merge commits are exempt"          0 "" FILES="README.md" SUBJECTS="2:Merge branch 'main' into x" CLASSIFY=success RESULTS="$ALLSKIP"
check "a fake merge subject is not exempt" 1 "lacks a housekeeping prefix" FILES="README.md" SUBJECTS="Merge installation sections" CLASSIFY=success RESULTS="$ALLSKIP"
check "bare subject fails the lane"       1 "lacks a housekeeping prefix" FILES="README.md" SUBJECTS="Fix a typo" CLASSIFY=success RESULTS="$ALLSKIP"
check "a prefix outside the table fails"  1 "lacks a housekeeping prefix" FILES="README.md" SUBJECTS="chore: tidy" CLASSIFY=success RESULTS="$ALLSKIP"
check "a dispatched skip still lints"     1 "lacks a housekeeping prefix" FILES="README.md" SUBJECTS="Fix a typo" CLASSIFY=success RESULTS="$ALLSKIP" GITHUB_EVENT_NAME=workflow_dispatch
check "gate refuses a mis-bound dispatch"  1 "must not label" FILES="README.md" SUBJECTS="docs: x" CLASSIFY=success RESULTS="$ALLSKIP" GITHUB_EVENT_NAME=workflow_dispatch HEAD_SHA=otherhead
check "gate refuses a shared head"         1 "cannot vouch for exactly one" FILES="README.md" SUBJECTS="docs: x" CLASSIFY=success RESULTS="$ALLSKIP" GITHUB_EVENT_NAME=workflow_dispatch SHARED_PRS="1 2"
check "skip on a code diff is refused"    1 "refusing the skip" FILES="crates/a.rs" SUBJECTS="docs: x" CLASSIFY=success RESULTS="$ALLSKIP"
check "skip on a shared head is refused"  1 "refusing the skip" FILES="README.md" SUBJECTS="docs: x" CLASSIFY=success RESULTS="$ALLSKIP" SHARED_PRS="1 2"
check "skip vouched by a foreign PR is refused" 1 "refusing the skip" FILES="README.md" SUBJECTS="docs: x" CLASSIFY=success RESULTS="$ALLSKIP" SHARED_PRS="2"
check "skip on a truncated list refused"  1 "refusing the skip" FILES="README.md" CHANGED=3000 SUBJECTS="docs: x" CLASSIFY=success RESULTS="$ALLSKIP"
check "commits API failure fails lint"    1 "cannot be verified" FILES="README.md" COMMITS_FAIL=1 CLASSIFY=success RESULTS="$ALLSKIP"
check "truncated commit list fails lint"  1 "Commit list incomplete" FILES="README.md" SUBJECTS="docs: x" NCOMMITS=300 CLASSIFY=success RESULTS="$ALLSKIP"
check "red job fails the gate"            1 "not all green" FILES="a.rs" CLASSIFY=success RESULTS="check=failure msrv=success"
check "half-skipped is not justified"     1 "not all green" FILES="a.rs" CLASSIFY=success RESULTS="check=skipped msrv=success"
check "cancelled job fails the gate"      1 "not all green" FILES="a.rs" CLASSIFY=success RESULTS="check=cancelled msrv=cancelled"
check "failed classify fails the gate"    1 "nothing vouches" FILES="a.rs" CLASSIFY=failure RESULTS="$ALLSKIP"

rm -rf "$stub"
if [ "$fails" -ne 0 ]; then echo "docs-lane tests FAILED"; exit 1; fi
echo "docs-lane tests passed"
