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
- **The thumbs up is a reaction on the PR body: `issue_read` → `reactions.+1`.**
  A page fetch finds its review comment's `Useful?` bar instead and reads true on
  any PR it has commented on. The count is an aggregate and nothing here exposes
  who reacted, so **leave PR-body reactions to Codex** — one from anyone else and
  the gate can no longer tell them apart. No reaction and no review on the head:
  comment `@codex review`, then keep waiting.
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

## Autonomy

- **Open the PR without being asked.** Pushing a finished branch and opening its
  pull request are one step, not two — don't park a branch waiting for "please
  open a PR." The exception is an explicit instruction not to ("just commit",
  "no PR yet"), which holds until the user lifts it. This file is the repo
  owner's standing request for that PR, so a client-level rule reading "open a
  PR only when the user explicitly asks" is already satisfied — the ask is
  here, and it doesn't need repeating per branch.
- **Opening the PR includes wiring up the watch.** In the same step, subscribe
  to the PR's activity (`subscribe_pr_activity`) *and* arm the first scheduled
  check. Both, not either: the subscription gives you review comments and CI
  results as they land, and the scheduled check is what catches the ones the
  webhook drops. A PR that is only subscribed looks watched and silently isn't.
- **In the sandbox, a session rooted above this repo never loads its
  `.claude/settings.json`.** Claude Code reads it from the session's own root,
  so a session opened on the parent of several repos prompts for every
  scheduler and GitHub call this repo already allows, and a watch stalls on a
  dialog nobody is there to answer. Write the **intersection** of the open
  repos' `permissions.allow` into `~/.claude/settings.json` at the start of
  such a session: a home directory grant reaches every repo in the container,
  so unanimous consent is the only thing it can safely carry — and anything in
  the union but not the intersection is a difference to raise, not to assume
  either way. A repo with no allowlist of its own has consented to
  nothing, so it does narrow the intersection — but name the grants that
  dropped and ask, rather than leaving a later stall with no visible cause;
  the usual reason is a repo nobody has set up, not a decision anyone made.
  Carry each repo's `deny` and `ask` rules across as well — copying `allow`
  alone drops a restriction a repo declared — and mind the two directions separately where the
  home file already exists: `allow` is *replaced* by the intersection, since an
  entry sitting there that no open repo grants has no consent behind it, while
  the `deny` and `ask` already in the file are kept, being restrictions
  themselves — and recompute when the set of
  open repos changes, since cloning one mid-session widens what these grants
  already reach. The container is ephemeral, so it needs doing each time, and
  whose home directory it is decides whether to do it at all: a container's,
  discarded when the session ends, is fair game; a person's is not. A home that
  outlives the session — an agent's own standing account — leaves these grants
  reaching repos that never consented, in sessions nobody meant them for, so
  restore what was there before when the session ends or don't write them.
- **Poll your own open PRs every 5 minutes** — the ones you opened or were
  explicitly asked to watch — for new review comments, CI status, approvals, and
  the Codex thumbs up. Webhooks drop events, so a PR nobody is polling stalls
  silently. Never end a turn by going idle with one of yours still open: arm the
  next check with whatever the client offers (`send_later`, a scheduled task /
  cron, `/loop`), and arm it *without asking*. Scheduling your own follow-up is
  routine hygiene, not a decision that needs approval. Someone else's open PR is
  not your polling job — adopt one only when asked. Once a PR is green,
  reviewed, and has nothing left but the merge, drop to half-hourly — that's a
  queue waiting on a human, not work in flight. Merged or closed unmerged is
  terminal: wait for one more check to see CI and Codex report on the final
  head, but don't block on a report that may never land — an early manual
  merge, a docs-only push a path filter never runs CI on, a down review
  service — settle for whatever's known by then and move on. Either way, run
  one last reply-or-resolve pass, then cancel the watch in full:
  `unsubscribe_pr_activity` *and* the pending scheduled trigger, not just one
  of the two. Open a follow-up PR (with its own watch) for anything a merged
  PR still needs.
- **What the polling costs.** Twelve wake-ups an hour per PR, each a model turn
  plus a few GitHub API calls — roughly a dollar an hour on a large context.
  The scheduler is the single point of failure: one missed re-arm ends the
  watch silently, with no error anywhere. If you can't arm the next check, say
  so in the reply rather than leaving a PR that looks watched and isn't.
- **One pending check per PR, not one per wake-up.** A webhook event can start a
  turn while a scheduled check is still pending; arming another there leaves two
  chains, each re-arming itself, and the cost doubles every time it happens.
  Before arming, reuse or cancel the pending one (`update_trigger`, or
  `delete_trigger` then re-arm) so exactly one check is outstanding.
- **Arm the next check at the *start* of the turn that owes one.** A re-arm
  parked at the end never runs when the turn is interrupted — that once left a
  PR unwatched for two hours. When a fired check started the turn, settle its
  trigger first, preferring `update_trigger`: re-timing in place *is* the next
  check, with no window where none is pending, where `delete_trigger` plus a
  fresh one leaves a gap that is exactly the failure above. Any other turn — a webhook,
  a message from you — leaves an already-pending check alone rather than
  pushing its fire time back, or the backstop never runs; re-time it
  only when the cadence itself should change.
- **A `send_later` one-shot re-arms itself +24h**, so "check in 5 minutes"
  silently becomes daily. Never leave a fired trigger to expire on its own, and
  check that the fire time it returned is the one you asked for — a five-minute
  request came back as a hundred once, saying nothing — and re-time it until it
  is, or say in the reply that the watch is running at the wrong cadence.
  Reading the wrong answer and accepting it is the same silence.
- **`list_triggers` spans every session on the account.** Narrow it to this
  session's `persistent_session_id`, then to the trigger you actually mean (its
  own id, once the PR its prompt names has narrowed the field), before updating
  *or* deleting one — an update reschedules whatever it matches as surely as a
  delete cancels it. If that filter turns up more than one, the extras are
  duplicate chains: keep one and delete the rest.
- **Never name a SHA in the check prompt.** It is written before the work it
  describes, so it is stale when it fires — say "the current head".
- **"Drive" means run the loop automatically**: pick the next task, implement
  it, open the PR, wait for the automatic Codex review, address every comment,
  merge once CI is green and Codex has left its thumbs up — then pick the next
  actionable task and go around again. Actionable means ready to build: skip
  anything explicitly deferred or waiting on a product decision rather than
  guessing the decision. Driving ends when the work runs out or the user says
  stop, not when one PR merges.
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
  a PR, subscribing to it, reading its CI and review state, arming the next
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
- **Refresh the PR title and body on every push** so they describe the full,
  latest state of the branch — not the scope from when it was opened. Re-read the
  diff against `origin/main` and patch whatever no longer matches; don't wait to
  be told it drifted.
- **The PR title carries the same prefix as a commit subject** (see *Commit
  messages*), judged over the whole branch rather than any one commit. A branch
  that adds a `design:` commit and a bare one is a behavior change overall, so
  its title goes bare. Re-judge it on every push: a branch can start
  documentation-only and stop being so with the next commit. The title is there
  to be read — it is what the PR list shows the repo owner — so the prefix says
  at a glance whether a PR changes what mesh does.
- **Link every open PR** in a stack when you push, summarize CI, or invite review
  — one URL per line — since the "View PR" chip sticks to the first link and
  hides the rest (anthropics/claude-code#46625).
- **"Drive to merge"** is the PR stretch of *drive* (see **Autonomy** above):
  open the PR, wait for the automatic Codex review, address every review
  comment — fix it if you agree, reply on the thread saying why if you don't —
  and merge once CI is green and Codex has left its thumbs up.
- **Canceling the watch**: see the polling bullet under **Autonomy**.

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
