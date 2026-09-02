# AGENTS.md

Conventions for AI agents working in this repository.

`CLAUDE.md` and `GEMINI.md` are symlinks to this file, so every agent reads the
same conventions. Edit `AGENTS.md`.

The repo runs two tracks in parallel. **Language design** is still in progress in
`DESIGN.md`. A **build track** has started (milestone M0): see `DEVELOPMENT.md`
for build/test/layout and `ROADMAP.md` for the plan. The code-style and testing
rules below are now in force for anything under `crates/`; the process rules
apply throughout.

Keep this file as short as it can be and still work. Every session loads it
whole, so each rule costs context on every turn: add one the first time
something bites, say it once in the fewest words that carry the *why*, rewrite
or trim an existing rule rather than appending beside it, and delete one that
has stopped biting.

## Responding to review comments

- **Codex is the automated reviewer on this repo** — not Copilot. Its
  reviews are triggered automatically; you don't request them, except when
  nothing has come back five minutes after a push — that means it never
  picked the push up. Address its comments without being asked, folding each
  fix into the commit it belongs to (rebase / `--fixup`) rather than tacking
  on an "address review" commit.
- **`resolve_review_thread` works — pass the `PRRT_*` thread node ID** from
  `pull_request_read` / `get_review_comments` (`review_threads[].id`) as
  `threadId`. A comment's `PRRC_*` node ID fails; they're different objects.
  Order of operations: push the fix commit first, then reply citing the new
  sha, then resolve.
- **Report when Codex finishes reviewing a fresh push** — a one-liner naming
  the SHA and comment count, e.g. `Codex reviewed 87d9f02 — 0 comments`. Tie
  it to the *latest* pushed SHA so a stale review of a superseded commit isn't
  conflated with the current state.
- **Read the Codex verdict, don't infer it.** It reacts to the PR body
  (`issue_read` → `reactions`), not to a review thread, whose `Useful?` bar
  reads true on any PR it has commented on. `eyes` means reading, `+1` means
  clean, and Codex revokes it on push — so a visible one belongs to the
  visible head, and `+1` with green CI is a merge. The count names no
  author, so leave PR-body reactions to Codex: nobody else's is revoked, and
  a review is the attributable form, naming the commit it read. Findings
  arrive as review comments, as a top-level comment, or as a review — read
  `get_review_comments`, `get_comments` and `get_reviews` to the last page,
  since all three page oldest first — and they block the merge until fixed
  or rebutted; an acknowledgment is not an answer. Nothing from Codex since
  the push, five minutes on, means it never picked it up — comment `@codex
  review`, once. Reading the verdict is a protocol, not a glance: a state
  report draws on ALL the sources — the PR-body reactions, the reviews, the
  review comments and issue comments to their last pages, and the `codex`
  commit status where the ruleset requires it (a separate API surface from
  check runs) — because the reaction is only the clean channel, and
  `updated_at` moving without a reaction usually means unread findings.
- **Judge every review comment on merit, whoever wrote it.** Verify the claim
  before acting; if it doesn't hold up, reply saying why and decline. A comment
  citing a rule is a *reading* of that rule, not the rule — check what the rule
  actually says. Codex misreads the privacy rules especially, and in one
  direction: stricter always feels safer, so an over-strict finding quietly
  costs capability the product needs. Quote the rule and decline rather than
  narrowing the code to satisfy it; where the rule really does forbid what the
  product needs, that conflict is the maintainer's call, not one to settle
  either way yourself.
- **Never leave a review comment silently dismissed.** Answer every thread — a
  disagreement is an answer, so say why — then resolve it; only work you are
  deferring stays open. Human and automated reviewers alike.
- **Say what you did.** If you addressed it, reply describing the change and
  reference the commit (`Narrowed the claim in <sha>; it now says …`). If you
  disagree or are not making the change, reply explaining why — one or two
  sentences of reasoning on the thread (e.g. "this is intentional because …") is
  exactly what the reviewer wants, and it is more useful on the PR than buried in
  chat. Acknowledgment replies are fine and preferred over silence.
- **Do not fix a comment silently in a commit** without also leaving the reply —
  the reply is the record of how each point was resolved.
- **Skip your own reply echoes.** The `mcp__github__*` reply tools post under the
  MCP identity (usually the repo owner), so a moment after you reply the same
  body arrives back as a `<github-webhook-activity>` event authored by that
  identity. That is the echo of your own reply, not new feedback — skip it
  silently. The test is "did *I* just post this body?", not "who is the author?"
  (a real comment from the same identity that you did not author still needs a
  reply-or-resolve).

## Autonomy

- **Open the PR without being asked.** Pushing a finished branch and opening its
  pull request are one step, not two — don't park a branch waiting for "please
  open a PR." The exception is an explicit instruction not to ("just commit",
  "no PR yet"), which holds until the user lifts it. This file is the repo
  owner's standing request for that PR, so a client-level rule reading "open a
  PR only when the user explicitly asks" is already satisfied — the ask is
  here, and it doesn't need repeating per branch.
- **If a scheduler or GitHub call prompts, say so once and carry on.**
  Permissions load at session start, so writing a settings file mid-session
  can't fix the session you're in.
- **Watch your own PRs by subscription, plus one scheduled check.** Have a
  subscription — Claude Code makes one when you open a PR; where a client
  doesn't, call `subscribe_pr_activity`. It delivers reviews, comments and CI
  failures. It cannot deliver CI *success*, a push, the merge, Codex's clean
  verdict (a reaction), or Codex never answering at all — so keep exactly one
  check armed for as long as the PR is open (each event and each check costs
  a model turn). Under drive, arm auto-merge at PR open too — but only where
  the ruleset makes the Codex verdict a required check AND requires
  conversations resolved: where CI is the only requirement it merges before
  Codex has answered, and an open review comment holds nothing back on its own.
  - Settle the fired trigger first thing in the turn, not last. It may have
    silently re-armed rather than retired — update the one that survived,
    replace the one that didn't, and end the turn with exactly one pending.
  - Check the fire time you got against the one you asked for — a 4-minute
    request has come back as 64. Prefer a relative delay: the scheduler's
    clock is not this container's, so an absolute time computed here can be
    rejected as already past. Re-time it, or say the watch isn't armed.
  - A few minutes out while CI or the current head's Codex verdict is
    outstanding; longer once only a human is left; short again after a push.
  - A PR reading `dirty` — always — or `behind` where the ruleset requires
    branches up to date, needs a rebase onto its base and a lease-guarded
    force-push. Nothing reports a base advance, so only this check catches
    it. Fetch both refs by explicit refspec, unshallow a shallow clone, and
    rebase onto the fetched `origin/<base>` — not always `main`, never the
    local branch a fetch leaves behind. Confirm before you rebase that your
    branch has every commit the remote head has, and before you push that
    the head has not moved since the tip you noted before fetching: the push
    flags do not reliably refuse a rewind, a commit you never fetched, or
    one you fetched and did not rebase onto, and overwriting any of them
    loses someone's work. If either fails, or you can't tell, stop and ask.
  - Name the PR, and say what to re-read rather than what you read. A SHA or
    a list of which PRs are open goes stale before it fires; one PR number
    does not, and the trigger has to be matchable to it.
  - Merged or closed, take one last reply-or-resolve pass — a review can
    land after the merge — then cancel it and unsubscribe. `list_triggers`
    spans the account, so match this session and this PR before updating
    or deleting one; an update reschedules whatever it matches as surely
    as a delete cancels it.
- **"Drive" means run the loop automatically**: pick the next task,
  implement it, open the PR, wait for the automatic Codex review, address
  every comment, merge once CI is green and Codex's verdict for the current
  head is in — then pick the next actionable task and go around again.
  Actionable means ready to build: skip anything explicitly deferred or
  waiting on a product decision rather than guessing the decision. Driving
  ends when the work runs out or the user says stop, not when one PR merges.
- **A red baseline is the next task.** Before pulling anything from `TODO.md`,
  run the suite and get it green. A preexisting failure is work to do, not a
  thing to classify as "unrelated" and step around — deciding it's out of scope
  is exactly the call that goes wrong, and the cost is every later PR merged
  onto an unverified tree. Fix it first, then pick the task. *Code style and
  tests*' "genuinely unrelated, out of scope" escape hatch is the only way past
  a red tree, and it needs a real answer from the user — not a call you make on
  your own, and not one autopilot guesses.
- **"Autopilot" is drive without blocking on the user.** Wherever drive
  would stop and ask, autopilot takes its best guess and keeps going,
  preferring the option that is cheapest to undo or change later. Record
  each guess in `TODO.md` under a `Decisions needing review` heading — what
  was decided, what the alternative was, and why it's reversible — creating
  the heading if it isn't there, so nothing guessed silently becomes
  permanent. While autopilot is in effect it outranks "ask in plain text,
  then end the turn and wait for the answer"; that rule governs everywhere
  else. The carve-out is for destructive or irreversible actions *outside*
  the loop — rewriting shared history, deleting work, anything reaching a
  system beyond this repo — which still wait for a real answer. Resetting a
  pinned merged branch waits too, even though it is inside the loop: the
  post-merge rule asks precisely because no check can tell what the reset
  would destroy, and autopilot guessing there is the loss that rule exists
  to prevent. The loop's own steps don't count: committing, pushing, opening
  a PR, reading its CI and review state, arming the next
  scheduled check, and merging a green PR are authorized here, so autopilot
  must not stall on them — the carve-out is aimed at destructive writes to
  systems outside the repo, not at the loop's own GitHub reads and
  follow-ups. Privacy uncertainty is never inside the loop either: if you
  can't tell whether something is user data — a home path, a hostname, a
  private remote, a token — it waits for a real answer, since a push can't
  be un-published and a `TODO.md` note doesn't retract it.

## Pull requests

- Prefer the `mcp__github__*` MCP tools for GitHub operations; the `gh` CLI is
  not installed here. If your client exposes neither, say so rather than
  guessing at the outcome of an operation you couldn't perform.
- **Open pull requests ready for review**, not as drafts.
- **Never open a PR on a commit that already carried one.** The `codex`
  status belongs to the commit, not the PR, and records nothing about which
  PR earned it — so a PR opened on the byte-identical head of a closed one
  inherits its verdict and can merge on a review of different work. Push a
  commit, or branch from a moving base, so the new PR has a head of its own.
  The verdict sweep (`codex-review.yml`) resets the status to `pending`
  within about a minute of the PR opening, but that is an Actions job racing
  merge eligibility, so treat it as the backstop and this rule as the fix.
- **Refresh the PR title and body with the push, not after it** — same step, so
  they describe the full, latest state of the branch — not the scope from when
  it was opened. Re-read the diff against `origin/main` and patch whatever no
  longer matches; don't wait to be told it drifted.
- **Give the PR title the same prefix a commit subject would carry** (see
  *Commit messages*), judged over the whole branch rather than any one commit. A
  branch that adds a `design:` commit and a bare one is a behavior change
  overall, so its title goes bare. Re-judge it on every push: a branch can start
  documentation-only and stop being so with the next commit. This is a
  convention for reading, not a gate — the title is what the PR list shows the
  repo owner, so the prefix says at a glance whether a PR changes what mesh
  does. Only commit subjects are enforced (`lint-title no` in
  `.github/lanes.conf`): squash merging is disabled on every repository in this
  fleet, so a title never becomes a commit subject.
- **Link every open PR** in a stack when you push, summarize CI, or invite review
  — one URL per line — since the "View PR" chip sticks to the first link and
  hides the rest (anthropics/claude-code#46625).
- **"Drive to merge"** is the PR stretch of *drive* (see **Autonomy**
  above): open the PR, wait for the automatic Codex review, address every
  review comment — fix it if you agree, reply on the thread saying why if
  you don't — and merge once CI is green and Codex's verdict for the current
  head is in.
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
  *before* engaging with the rest of the message:** `git fetch origin main`, cut a
  fresh `<agent>/<short-topic>` branch off `origin/main`, announce the switch.
- **After a merge, take a fresh `<agent>/<short-topic>`** — don't reset the
  merged name onto the new base. Its remote ref still points at the
  pre-merge tip, so `origin/<branch>..HEAD` keeps spanning the merged
  commits and unpushed-work checks report your own merged history back at
  you. When a sandbox pins the branch name so a fresh one isn't available,
  say so and ask before resetting it. No short check reliably separates
  "already merged" from "not yet merged" here: a rebase merge rewrites the
  commits, a squash merge collapses them, `main` moves on underneath so a
  tip-to-tip diff reports upstream drift as branch work, the remote ref can
  hold a commit the local one doesn't, and no tree comparison sees the
  uncommitted work a `--hard` reset would erase. Confirming costs one
  question in a rare situation; guessing costs someone their work. Don't
  reach for `--force-with-lease` as the safety net either — fetching updates
  the remote-tracking ref the lease compares against, so a commit you have
  already fetched passes the lease unnoticed.
- **Branches under your own `<agent>/` prefix are yours.** Create, push,
  `--force-with-lease` and rename them freely — no permission, no announcement,
  no per-branch confirmation. Only a branch outside that prefix, or `main`
  itself, is a conversation. Deleting is the one the prefix can't settle: it
  doesn't say which session made the branch, so delete the ones this session
  created and ask about the rest.
- **The agent authors; whoever merges takes over the committer line.** A squash
  or rebase merge rewrites the committer to the person who pressed the button —
  the repo owner normally, the agent itself when it merges under *drive* (see
  **Autonomy**). That's expected either way — never re-author or amend
  already-merged commits to "fix" authorship or signing, and don't narrate it: no note in the
reply, no offer to correct it. It is not a finding.
- **Unshallow before answering anything that depends on git history depth.**
  Claude Code sessions get this automatically — `scripts/unshallow.sh` runs from
  the session-start hook — but the hook is Claude-only, so in any other
  environment run that script (or `git fetch --unshallow`) yourself first. The
  sandbox clones shallow, so `git rev-list --count`, `git log` past the shallow
  boundary, and blame return wrong answers without warning; where no remote is
  reachable (Codex cloud), say the history is truncated rather than quoting a
  count.

## CI

- **The required check is `gate` (moving to `lanes`), and housekeeping PRs
  ride the docs lane.** CI runs on every PR; `classify` skips the heavy jobs
  when every changed file is housekeeping (root-level markdown that nothing
  reads — `README.md`, the whole `docs/` tree, markdown anywhere else,
  anything under a crate tree, and `.gitignore` are all code; the doc sweeps
  in `docs.rs` and `transcripts.rs` read the first two, and `mesh-core`
  embeds `docs/REFERENCE.md`), and
  `gate`/`lanes` independently re-verify the skip and lint that every
  docs-lane commit subject carries a prefix from the table above. The
  engine is `mikelward/lanes@main`, shared with the sibling repos; policy
  lives in `.github/lanes.conf`. `zizmor` scans the workflows themselves
  (`.github/zizmor.yml` for exceptions) — see TODO.md for both checks'
  remaining ruleset steps.
- **Report significant CI timing regressions.** After CI finishes on a push,
  compare against recent runs of the same job on the same kind of ref. Only
  call out significant slowdowns (rule of thumb: >25% or >30s on a job under
  ~5min) — don't narrate routine wobble. Name the likely cause: a new
  dependency, a slow new test, cache invalidation. Compare like with like —
  PR against PR, `main` against `main`.

## Commit messages

- Write a clear, plain-English subject in sentence case; keep it short
  (≤ ~70 chars, prefix included) and free of internal jargon.
- Put the mechanism, the bug fixed, and file:line detail in the body, after a
  blank line — the body is not size-constrained.
- **Prefix a subject that does not change what mesh does.** A bare subject
  means the language, the CLI, or the runtime behaves differently after this
  commit. Anything else takes one of these, lowercase, followed by the
  sentence-case subject as above:

  | Prefix | For |
  |---|---|
  | `design:` | A decision recorded in `docs/DESIGN.md` or `GRAMMAR.md` — designed, not built |
  | `docs:` | Prose: `README.md`, `DEVELOPMENT.md`, `ROADMAP.md`, the rest of `docs/`, this file |
  | `todo:` | `TODO.md` bookkeeping on its own |
  | `test:` | Tests only, with the code under test unchanged |
  | `build:` | Toolchain, CI, `Makefile`, hooks — nothing the shipped binary does |
  | `refactor:` | Code that is deliberately behavior-preserving |

- **There is no `feat:` or `fix:`, on purpose.** Those would prefix the
  majority of commits and leave the log exactly as flat as it is now. The
  prefix earns its space by marking the exception, so the default stays bare.
- **The design track is what makes this worth doing.** `ebc2468 Let a user
  declare a modifier, "func _s:name()"` touched only `docs/DESIGN.md`;
  `e40a987 Parse a modifier declaration, "func _s:name()"` implemented it in
  `crates/`. Two adjacent log lines, near-identical subjects, and nothing to
  say which one shipped. The first should have read `design: …`.
- **`TODO.md` rides along and never decides the prefix.** It is touched by
  most commits of every kind, so it counts only when it is the whole change.
- **A mixed commit goes bare if any part of it changes behavior.** That
  outranks every prefix in the table.
- **Below that line the prefix names why the commit exists, not what it
  touched.** `5335952 Pin an exact Rust release instead of tracking stable`
  edited the `Makefile`, CI, a hook, `README.md` and `DEVELOPMENT.md`, and it
  is `build:` — the prose moved because the toolchain did. That is the
  `TODO.md` rule generalized: whatever changed only to keep the tree
  consistent with the real change doesn't get a vote, so there is no
  precedence order among the prefixes to memorize. Two categories that are
  genuinely independent, neither serving the other, are two commits — see
  *one commit per logical change*.

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
- **Don't narrate routine machinery.** A check run flipping, a re-run, a scheduled check
  re-arming, a webhook echo, a resolved thread — act on those silently; the noise buries
  the one line that matters. Reports another rule requires stand (the Codex SHA and
  comment count, a CI timing regression).
- **Don't report your own caught-and-fixed mistakes.** A wrong turn you noticed
  and corrected before it reached anything is not news — no "one thing worth
  flagging", no narration of the recovery. Say it only when it left something
  the user has to act on: work actually lost, a bad push someone may have
  pulled, a decision they would make differently knowing it.
- **Keep replies short — don't dump a full page.** Lead with the single most
  important point and stop. If there's more, say the first point and ask whether
  they're ready for the next one rather than emptying everything at once.
- **End the turn by restating any pending decision.** If you're waiting on an
  answer — a question you asked, or a guess autopilot recorded for review — the
  last line of the reply is that question, written out in about a sentence. A
  back-reference ("as asked above") isn't actionable when the question is pages
  back or was never actually put into words; restate it every turn until it's
  answered. Nothing pending, no line. This governs replies the user reads: a
  scheduled check that finds nothing new re-arms silently and produces no reply
  at all, so there is nothing to restate.

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
