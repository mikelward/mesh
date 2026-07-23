# AGENTS.md

Conventions for AI agents working in this repository.

`CLAUDE.md` and `GEMINI.md` are symlinks to this file, so every agent reads the
same conventions. Edit `AGENTS.md`.

The repo runs two tracks in parallel. **Language design** is still in progress in
`DESIGN.md`. A **build track** has started (milestone M0): see `DEVELOPMENT.md`
for build/test/layout and `ROADMAP.md` for the plan. The code-style and testing
rules below are now in force for anything under `crates/`; the process rules
apply throughout.

## Responding to review comments

- **Never leave a review comment silently dismissed** — every thread ends in
  either a reply or a resolve, not "left open and ignored." This holds for human
  and automated (bot) reviewers alike.
- **Say what you did.** If you addressed it, reply describing the change and
  reference the commit (`Narrowed the claim in <sha>; it now says …`). If you
  disagree or are not making the change, reply explaining why — one or two
  sentences of reasoning on the thread (e.g. "this is intentional because …") is
  exactly what the reviewer wants, and it is more useful on the PR than buried in
  chat. Acknowledgement replies are fine and preferred over silence.
- **Do not fix a comment silently in a commit** without also leaving the reply —
  the reply is the record of how each point was resolved.
- **Skip your own reply echoes.** The `mcp__github__*` reply tools post under the
  MCP identity (usually the repo owner), so a moment after you reply the same
  body arrives back as a `<github-webhook-activity>` event authored by that
  identity. That is the echo of your own reply, not new feedback — skip it
  silently. The test is "did *I* just post this body?", not "who is the author?"
  (a real comment from the same identity that you did not author still needs a
  reply-or-resolve).

## Pull requests

- Use the `mcp__github__*` MCP tools for all GitHub operations; the `gh` CLI is
  not available here.
- **Open pull requests ready for review**, not as drafts.
- **Refresh the PR title and body on every push** so they describe the full,
  latest state of the branch — not the scope from when it was opened. Re-read the
  diff against `origin/main` and patch whatever no longer matches; don't wait to
  be told it drifted.
- **Link every open PR** in a stack when you push, summarise CI, or invite review
  — one URL per line — since some UIs only render the first link.
- **Keep watching a merged PR for late comments.** Reviewers and bots routinely
  comment after merge; stay subscribed and handle each new comment per the rule
  above until they're all answered/resolved.

## Git workflow

- Before starting or continuing any task, run `git fetch origin main`. For a new
  task, create a fresh worktree on a fresh branch based on the latest
  `origin/main` when worktrees are available, using
  `git worktree add -b <branch> <path> origin/main`; otherwise create a fresh
  branch from it. When continuing an existing task branch, rebase it onto the
  latest `origin/main` before the first new commit, resolving any conflicts
  rather than abandoning the branch or working from an older base.
- Clean up the branch history before requesting review and again before merge —
  no `wip` / `fix typo` / `address review` churn shipping to `main`.
- After rewriting history, push with `git push --force-with-lease`, never a bare
  `--force`.

## Commit messages

- Write a clear, plain-English subject in sentence case; keep it short
  (≤ ~70 chars) and free of internal jargon.
- Put the mechanism, the bug fixed, and file:line detail in the body, after a
  blank line — the body is not size-constrained.

## Language and spelling

- Use **US English** everywhere read by people: prose, commit subjects and
  bodies, PR titles and descriptions, comments, and identifiers — `color` not
  `colour`, `behavior` not `behaviour`, `license` not `licence`. Platform and
  third-party API spellings stay as those APIs spell them.

## Environment

- **Do not use `apt-get` / `apt`** to install tools. Use direct binary downloads
  (e.g. from GitHub releases) or `cargo install`.

## Code style and tests

*(Applies once implementation begins.)*

- Preserve the existing code style unless there's a correctness issue.
- Keep comments brief: explain the non-obvious *why*, not the *what*, matching
  the surrounding style.
- Add or update tests with any code change; a change isn't done until it's
  covered. When fixing a bug, add a test that fails before the fix and passes
  after.
