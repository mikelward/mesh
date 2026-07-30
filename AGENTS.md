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

- **Codex is the automated reviewer on this repo** — not Copilot. Its reviews
  are triggered automatically; you don't request them. Address its comments
  without being asked, folding each fix into the commit it belongs to
  (rebase / `--fixup`) rather than tacking on an "address review" commit.
- **`resolve_review_thread` works — pass the `PRRT_*` thread node ID** from
  `pull_request_read` / `get_review_comments` (`review_threads[].id`) as
  `threadId`. A comment's `PRRC_*` node ID fails; they're different objects.
  Order of operations: push the fix commit first, then reply citing the new
  sha, then resolve.
- **Report when Codex finishes reviewing a fresh push** — a one-liner naming
  the SHA and comment count, e.g. `Codex reviewed 87d9f02 — 0 comments`. Tie
  it to the *latest* pushed SHA so a stale review of a superseded commit isn't
  conflated with the current state.
- **Judge every review comment on merit, whoever wrote it.** Verify the claim
  before acting; if it doesn't hold up, reply saying why and decline.
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

- Prefer the `mcp__github__*` MCP tools for GitHub operations; the `gh` CLI is
  not installed here. If your client exposes neither, say so rather than
  guessing at the outcome of an operation you couldn't perform.
- **Open pull requests ready for review**, not as drafts.
- **Refresh the PR title and body on every push** so they describe the full,
  latest state of the branch — not the scope from when it was opened. Re-read the
  diff against `origin/main` and patch whatever no longer matches; don't wait to
  be told it drifted.
- **Link every open PR** in a stack when you push, summarize CI, or invite review
  — one URL per line — since the "View PR" chip sticks to the first link and
  hides the rest (anthropics/claude-code#46625).
- **"Drive to merge"** is shorthand for the whole loop: open the PR, wait for
  the automatic Codex review, address every review comment — fix it if you
  agree, reply on the thread saying why if you don't — and merge once CI is
  green and Codex has left its thumbs up.
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
- **One commit per logical change.** Rewrite unmerged commits freely — amend,
  `git commit --fixup` + autosquash, squash, reorder, split — so each commit
  that lands is one coherent change, with fix-ups and review responses folded
  into the commit they belong to.
- Clean up the branch history before requesting review and again before merge —
  no `wip` / `fix typo` / `address review` churn shipping to `main`.
- After rewriting history, push with `git push --force-with-lease`, never a bare
  `--force`.
- **These rules assume an `origin` remote.** Without one you can't fetch,
  branch from `origin/main`, push, or open a PR — say so and stop rather than
  improvising a local substitute. **Exception:** in a sandbox that
  intentionally provides no remote Git support (Codex cloud, say), follow the
  normal branch rules from the current `HEAD` — a pre-created working branch
  counts — commit locally, and report that fetch, push, and pull requests are
  unavailable, using the sandbox's own PR handoff if it has one. That exception
  outranks every `origin`-dependent step around it — the `git fetch origin main`
  that opens a task, the merge-cue fetch, cutting a branch off `origin/main` — so
  work from the current `HEAD` and name what wasn't possible instead of faking it.
  One limit: a merge cue needs a base that *contains* the merge, and an offline
  sandbox can't fetch one. Say the follow-up needs a fresh sandbox or a synced
  checkout rather than branching off a `HEAD` whose commits just landed upstream.
- **Branch naming.** Feature branches are prefixed with the agent's own short
  name: `<agent>/<short-topic>` (`claude/...` for Claude Code, `codex/...` for
  Codex, and so on). One topic per branch; never commit to `main`. The
  placeholder `<agent>` stands in for whichever prefix you use — don't
  hard-code `claude/` unless you *are* Claude Code.
- **Merge cue (`merged` / `I merged` / `landed` / merge webhook) runs hygiene
  *before* engaging with the rest of the message:** `git fetch origin`, cut a
  fresh `<agent>/<short-topic>` branch off `origin/main`, announce the switch.
- **Unshallow before answering anything that depends on git history depth.** The
  sandbox clones shallow, so `git rev-list --count`, `git log` past the shallow
  boundary, and blame return wrong answers without warning. If
  `git rev-parse --is-shallow-repository` says `true`, run
  `git fetch --unshallow` first, then re-check — it exits 0 even when
  it deepened nothing, so if `--is-shallow-repository` is still `true`, say the
  history is truncated instead of quoting a count.

## CI

- **Report significant CI timing regressions.** After CI finishes on a push,
  compare against recent runs of the same job on the same kind of ref. Only
  call out significant slowdowns (rule of thumb: >25% or >30s on a job under
  ~5min) — don't narrate routine wobble. Name the likely cause: a new
  dependency, a slow new test, cache invalidation. Compare like with like —
  PR against PR, `main` against `main`.

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
- **Do not use Claude's `AskUserQuestion` tool** — its multiple-choice prompt is
  broken on mobile, so the question never becomes answerable. Ask in plain
  message text instead, then end the turn and wait for the answer; do not pick a
  default and carry on.

## Talking to the user

- **One question at a time.** Never stack multiple questions in a single turn —
  ask the most important one, wait for the answer, then ask the next if you
  still need it. A wall of bundled questions is harder to answer than a short
  back-and-forth.
- **Don't interrupt.** Never fire off a question while the user is still typing.
  Let them finish; a half-typed message isn't an invitation to jump in.
- **Keep replies short — don't dump a full page.** Lead with the single most
  important point and stop. If there's more, say the first point and ask whether
  they're ready for the next one rather than emptying everything at once.

## Privacy

- **Never put user data in any artifact that leaves this machine** — commit
  subjects and bodies, PR titles / descriptions / comments, review replies,
  issue text, branch names, code comments, or test fixtures. That covers
  absolute paths containing the user's real name, hostnames, private remote
  URLs, tokens, and source files from a private repo pasted in as a
  reproduction. Use generic placeholders (`/home/user/project`,
  `git@example.com:org/repo.git`) in examples and fixtures. If a bug report
  contains any of it, paraphrase in the commit / PR — don't quote verbatim.
  When in doubt, ask before pushing.
- **Compiler output is not one of those artifacts.** A diagnostic prints on the
  user's own terminal, and naming the source path and span is the point of the
  message. Redact only secrets: tokens, keys, and passwords. Quoting that
  output into a commit, PR, issue, or fixture republishes it, and the bullet
  above governs again — paraphrase or use a placeholder path there.

## Cost and reliability

- **Call out cost and reliability up front** when recommending a new crate,
  service, or external call. Include a rough dollar figure where one applies —
  free-tier vs. paid thresholds and $/month at expected use — and note
  reliability implications: new failure modes, rate limits, added latency,
  extra points of failure. For a compiler, added build-time and binary-size
  cost count too. If the impact is effectively zero, say so rather than
  omitting the note.

## Code style and tests

*(Applies once implementation begins.)*

- Preserve the existing code style unless there's a correctness issue.
- Keep comments brief: explain the non-obvious *why*, not the *what*, matching
  the surrounding style.
- Add or update tests with any code change; a change isn't done until it's
  covered. When fixing a bug, add a test that fails before the fix and passes
  after.
- **Fix any preexisting test failures as the *first* commit of the series.**
  Don't stack new work on a red baseline. If the failure is genuinely unrelated
  and out of scope, say so up front and confirm before skipping it.
- **Don't paper over flaky/racy tests** with sleeps, retry loops, or bumped
  timeouts. Make the ordering explicit, or fix the underlying race. A test that
  passes "most of the time" is broken.
- **Don't disable a failing check** (a test, `cargo clippy`, a lint) to make it
  pass — fix the underlying issue.

## Error handling

- **Don't silently swallow errors.** A discarded `Result` or a `let _ = …` over
  a fallible call hides real failures. Wrap the error so the chain survives,
  clean up what the call acquired, and decide explicitly what the caller sees.
  For a compiler the common case is malformed input: keep "invalid source,
  report a diagnostic" distinct from "the toolchain failed."
