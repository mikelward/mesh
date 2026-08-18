#!/usr/bin/env bash
# The docs lane: lets housekeeping-only pull requests merge without the
# heavy jobs, while one required check — the `gate` job — still reports on
# every PR.
#
# Why this exists: a ruleset can only require named checks, and a required
# check that never reports blocks the PR forever. A `paths:` filter on
# `pull_request` skips the whole workflow for housekeeping diffs, which is
# exactly that trap. So the workflow runs on every PR; `classify` decides
# whether the heavy jobs may skip, and `gate` — the only job a ruleset should
# require — independently re-derives that decision before blessing a skip, so
# a classification bug turns into a red check instead of a silent merge.
#
# One source of truth: both jobs run THIS file, so the housekeeping rule
# cannot drift between them. What the gate adds is re-execution plus a
# cross-check against what actually ran. scripts/docs-lane.test.sh exercises
# every branch below against a stubbed `gh`, and the classify job runs it
# before classifying — a broken rule fails closed, never green.
#
# This file is shared across the sibling repos (first landed in snoozemo,
# hardened by eight review rounds there). Everything below the config
# section is the ENGINE and stays identical everywhere; when you change it,
# change it in every repo that carries it.
set -euo pipefail

# --- Repo policy: the only section that differs between repos. -------------

# The lane's test is "can this change what CI validates?". Markdown is docs
# wherever it lives OUTSIDE the crate trees — prose cannot change a build,
# categorically — while anything under crates/ is a build input whatever its
# extension (cargo packages a crate's README into the published artifact,
# and include_str! can embed any file). Beyond that nothing rides the lane
# by pattern: "dotfile" is a naming convention, not a semantic category
# (.npmrc changes npm's resolution, .nvmrc picks a runtime), and .gitignore
# runs the code lane like everything else — ignore rules can hide files
# from CI's own staging steps, and the sibling repos' lanes treat it as
# code for exactly that reason. Everything else is code: the session hook
# under .claude/ (CI lints and tests it), workflows, and any executable
# configuration.
is_housekeeping() {
  case "$1" in
    crates/*) return 1 ;;
    *.md) return 0 ;;
    *) return 1 ;;
  esac
}

# The subject prefixes this repo's commit conventions give housekeeping
# commits (see AGENTS.md "Commit messages"). On the docs lane every commit
# must carry one, so nothing on this lane can ever read like a bare
# behavior-change subject.
LANE_PREFIXES="design docs todo test build refactor"

# ci.yml's workflow_dispatch requires the `pr` input, but that's a GitHub
# UI/API-level constraint this engine cannot see or trust on its own — the
# REST API accepts a dispatch that omits a "required" input regardless, and
# this function is the guard that actually runs. No dispatch is legitimate
# here without a named PR.
dispatch_without_pr_ok() {
  return 1
}

# --- Engine: identical across repos below this line. -----------------------

has_lane_prefix() {
  local subject=$1 p
  for p in $LANE_PREFIXES; do
    case "$subject" in "$p: "*) return 0 ;; esac
  done
  return 1
}

# The complete file list, or a hard failure — never a silent prefix of it.
# Two ways a naive listing lies: the endpoint caps at 3,000 files, returning
# a clean-looking truncation; and a pagination failure after the first page
# exits non-zero into a process substitution, where bash discards the status.
# So the output is captured with its status checked, and the count is
# reconciled against the PR's own changed_files figure before anything is
# classified.
pr_files() {
  local declared files listed
  declared=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${PR}" --jq '.changed_files') || {
    echo "::error::Could not read the pull request's changed_files count." >&2
    return 1
  }
  # Both sides of every entry: a rename carries its new path in `filename`
  # and its old one in `previous_filename`, and classifying only the new
  # side would let a source file renamed into docs ride the docs lane while
  # deleting code. One TSV line per entry keeps the count reconcilable
  # against changed_files.
  files=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${PR}/files" --paginate \
            --jq '.[] | [.filename, .previous_filename // ""] | @tsv') || {
    echo "::error::Could not list the pull request's files." >&2
    return 1
  }
  listed=$(printf '%s' "$files" | grep -c . || true)
  if [ "$listed" -ne "$declared" ]; then
    echo "::error::File list incomplete: listed ${listed} of ${declared} changed files (the API caps at 3,000) — refusing to classify." >&2
    return 1
  fi
  printf '%s\n' "$files"
}

# Every open pull request the given commit currently heads, one number per
# line — or a hard failure when the listing cannot be completed, because a
# per-commit check must not be minted on an association nobody verified.
open_prs_heading() {
  HEAD_Q="$1" gh api "repos/${GITHUB_REPOSITORY}/commits/$1/pulls" --paginate \
    --jq '.[] | select(.state == "open" and .head.sha == env.HEAD_Q) | .number'
}

# 0 = every changed file is housekeeping; 1 = code, or an empty diff;
# 2 = the file list could not be trusted (API failure or truncation).
docs_only() {
  case "${GITHUB_EVENT_NAME:-}" in
    pull_request)
      # A commit can head more than one open PR (stacked PRs: same branch,
      # different bases), and a check run is per-commit — a gate minted for
      # this PR's justified skip would satisfy the other PR's required
      # check too, even where that PR's diff is code. So a shared head
      # never rides the docs lane: classified as code, the heavy jobs run,
      # and the gate on this SHA is backed by real validation whichever PR
      # reads it. (On pull_request events GITHUB_SHA is the merge commit,
      # not the head, so the PR's own head is looked up first.)
      test -n "${PR:-}" || return 1
      local prhead prs
      prhead=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${PR}" --jq '.head.sha') || return 2
      prs=$(open_prs_heading "$prhead") || return 2
      # The sole open PR must be THIS one, not merely a count of one: the
      # originating PR can close while its run is in flight, leaving a
      # stacked twin as the single open PR the gate would then vouch for.
      if [ "$(printf '%s' "$prs" | grep -c .)" -ne 1 ] || [ "$(printf '%s' "$prs" | head -n1)" != "$PR" ]; then return 1; fi
      ;;
    # A dispatched run may stand in for a PR run, but only by naming the PR,
    # so classification still judges the PR's real diff rather than waving
    # the branch through. verify_dispatch_binding (below) has already bound
    # the named PR to the checked-out commit before this runs.
    workflow_dispatch) test -n "${PR:-}" || return 1 ;;
    *) return 1 ;;
  esac
  local files any=false new old
  files=$(pr_files) || return 2
  while IFS=$'\t' read -r new old; do
    test -n "$new" || continue
    any=true
    is_housekeeping "$new" || return 1
    # A rename is only housekeeping if the path it LEFT was housekeeping too.
    if [ -n "$old" ]; then is_housekeeping "$old" || return 1; fi
  done <<< "$files"
  # An empty diff is not a docs diff; refuse to vouch for it.
  test "$any" = true
}

# On the docs lane every commit subject must carry a housekeeping prefix. A
# commits listing that cannot be completed fails the lint — an unverified
# prefix is not a verified one.
lint_prefixes() {
  local declared subjects listed bad=0 subject
  # Same reconciliation as pr_files, for the same reason: the commits
  # endpoint stops at 250 commits and exits cleanly, so an unprefixed
  # subject past the cap would simply never be seen. The PR's own commit
  # count says how many there are supposed to be.
  declared=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${PR}" --jq '.commits') || {
    echo "::error::Could not read the pull request's commit count — the prefix rule cannot be verified."
    return 1
  }
  # Parent count travels with each subject so merge commits are identified
  # structurally — a docs commit whose subject merely starts with "Merge "
  # is not a merge commit and gets no exemption.
  subjects=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${PR}/commits" --paginate \
               --jq '.[] | [(.parents | length), (.commit.message | split("\n")[0])] | @tsv') || {
    echo "::error::Could not enumerate the pull request's commits — the prefix rule cannot be verified."
    return 1
  }
  if [ -z "$subjects" ]; then
    echo "::error::Commit enumeration returned nothing — the prefix rule cannot be verified."
    return 1
  fi
  listed=$(printf '%s' "$subjects" | grep -c . || true)
  if [ "$listed" -ne "$declared" ]; then
    echo "::error::Commit list incomplete: listed ${listed} of ${declared} commits (the API caps at 250) — the prefix rule cannot be verified."
    return 1
  fi
  local parents
  while IFS=$'\t' read -r parents subject; do
    # Merge commits are exempt — the repo rebase-merges, so they never land
    # on main — and a merge commit is one with more than one parent, not one
    # whose subject happens to start with the word.
    if [ "${parents:-1}" -gt 1 ]; then continue; fi
    if has_lane_prefix "$subject"; then continue; fi
    echo "::error::Docs-lane commit subject lacks a housekeeping prefix:" \
         "'${subject}' — prefix it ($(printf '%s:/' $LANE_PREFIXES | sed 's,/$,,'))" \
         "so it never reads like a behavior-change subject."
    bad=1
  done <<< "$subjects"
  return "$bad"
}

# On a dispatched run the named PR must BE the checked-out commit: `--ref`
# selects the branch and `-f pr=` supplies the input independently, so
# nothing else stops a dispatch on code PR A's branch from naming docs PR B
# and landing B's clean verdict on A's head SHA. Verified in BOTH modes —
# classify failing already cascades to a red gate, and gate re-checks so the
# required check never reports for a commit the named PR does not head.
# (Kept in the shared engine even where a repo's workflow declares no
# dispatch trigger: engines stay identical, and a trigger added later is
# born guarded.)
verify_dispatch_binding() {
  test "${GITHUB_EVENT_NAME:-}" = "workflow_dispatch" || return 0
  # An unnamed PR cannot be verified, so it is refused — unless the repo's
  # config says a PR-less dispatch is legitimate here (deploy-force on the
  # default branch), in which case docs_only classifies it as code and the
  # full lane runs.
  if [ -z "${PR:-}" ]; then
    if dispatch_without_pr_ok; then return 0; fi
    echo "::error::A dispatched run must name the pull request it reports for (the pr input) — refusing without one."
    return 1
  fi
  local head
  head=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${PR}" --jq '.head.sha') || {
    echo "::error::Could not read PR #${PR}'s head SHA — refusing to report for it."
    return 1
  }
  if [ "$head" != "${GITHUB_SHA:?}" ]; then
    echo "::error::Dispatched commit ${GITHUB_SHA} is not PR #${PR}'s head (${head}) — a verdict computed for one pull request must not label another's commit."
    return 1
  fi
  # SHA equality alone is not a complete association: a commit can head more
  # than one open PR (same branch, different bases), and a check run is
  # per-commit, so a gate minted for the docs PR would satisfy the code PR
  # too. Require the named PR to be the ONLY open PR this commit heads;
  # ambiguity is refused rather than resolved, the fail-closed direction.
  local heads
  heads=$(open_prs_heading "${GITHUB_SHA}") || {
    echo "::error::Could not list the pull requests this commit heads — refusing to report for it."
    return 1
  }
  if [ "$(printf '%s' "$heads" | grep -c .)" -ne 1 ] || [ "$(printf '%s' "$heads" | head -n1)" != "$PR" ]; then
    echo "::error::Commit ${GITHUB_SHA} heads these open pull requests: $(printf '%s' "$heads" | tr '\n' ' ')— a per-commit gate cannot vouch for exactly one, so a dispatched run refuses to report."
    return 1
  fi
}

case "${1:?usage: docs-lane.sh classify|gate}" in
  classify)
    verify_dispatch_binding || exit 1
    # Any failure to establish docs-only — code paths, an untrustworthy file
    # list, a non-PR event — classifies as code: the heavy jobs run, which is
    # always the safe direction. The gate is where an unjustified SKIP fails.
    if docs_only; then echo "docs_only=true"; else echo "docs_only=false"; fi
    ;;
  gate)
    verify_dispatch_binding || exit 1
    # Results arrive via env: CLASSIFY (needs.classify.result) and RESULTS —
    # space-separated `job=result` pairs for every heavy job, supplied by the
    # workflow so the engine needs no per-repo job names.
    if [ "${CLASSIFY:?}" != "success" ]; then
      echo "::error::classify did not succeed (result: ${CLASSIFY}) — nothing vouches for this diff."
      exit 1
    fi
    all_success=true
    all_skipped=true
    for pair in ${RESULTS:?}; do
      case "${pair#*=}" in
        success) all_skipped=false ;;
        skipped) all_success=false ;;
        *) all_success=false; all_skipped=false ;;
      esac
    done
    if [ "$all_success" = true ]; then
      exit 0
    fi
    if [ "$all_skipped" = true ]; then
      # The skip is only as good as the reason for it: re-derive the
      # classification here, independently of the output that caused it.
      # docs_only's failure modes (code file, truncated or unlistable file
      # list) all land here as a refusal.
      if ! docs_only; then
        echo "::error::Heavy jobs were skipped but the diff could not be verified as housekeeping-only — refusing the skip."
        exit 1
      fi
      lint_prefixes
      exit 0
    fi
    echo "::error::Heavy job results '${RESULTS}' — not all green, and not a justified skip."
    exit 1
    ;;
  *)
    echo "unknown mode: $1" >&2
    exit 2
    ;;
esac
