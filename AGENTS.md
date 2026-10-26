---
trigger: always_on
alwaysApply: true
last_modified: 2026-10-26
---

# AGENTS.md

`CLAUDE.md` and `GEMINI.md` are symlinks to this file. Edit `AGENTS.md` and bump
`last_modified`.

**At the start of every session, print the full absolute path of the `AGENTS.md` you
loaded and its `last_modified` date**, so a stale or wrong copy is caught first.

Two tracks run in parallel: **language design** (`DESIGN.md`, still in progress) and a
**build track** (milestone M0; `DEVELOPMENT.md` for build/test/layout, `ROADMAP.md` for
the plan). Code-style and testing rules apply to `crates/`; process rules throughout.

Keep this file **under 24,000 bytes** (`wc -c AGENTS.md`): Antigravity silently truncates
rule files past 24 KB, and every session loads it whole. Add a rule the first time
something bites, in the fewest words that carry the *why*; rewrite or trim rather than
append; delete rules that have stopped biting.

## Responding to review comments

- **Codex is the automated reviewer** (not Copilot) and triggers itself; request one
  only to confirm a rebutted false positive, or where *Read the Codex verdict* says the
  `codex` status is still pending. Address its comments unasked, folding each fix into
  its commit (rebase / `--fixup`) — except a real out-of-scope finding, which you defer
  (see *Deferring a finding*).
- **`resolve_review_thread` takes the `PRRT_*` thread ID** (`review_threads[].id`), not
  a `PRRC_*` comment ID; without the MCP tools, `gh api graphql` `resolveReviewThread`
  takes the same. Push the fix, reply citing the new sha, then resolve.
- **Report each review in your next message**: the commit it read (say if no longer the
  head), the finding count, and one bullet per finding (review comment, top-level
  comment or review body), most blocking first — claim, location, what you did or the
  maintainer's options — ahead of *keep replies short*; ask only the most blocking
  question.
- **Read the Codex verdict, don't infer it.** It reacts to the PR body (`issue_read` →
  `reactions`), not a thread (whose `Useful?` bar is true on any PR it commented on):
  `eyes` = reading, `+1` = clean, revoked on push, so `+1` with green CI is a merge.
  Leave PR-body reactions to Codex (the count names no author). Findings — review
  comments, top-level comments, reviews; read `get_review_comments`, `get_comments` and
  `get_reviews` to the last page, oldest first — block the merge until fixed, rebutted,
  or deferred; an acknowledgment is not an answer. A pending `codex` status means
  findings to address; or a clean review with no reaction, or an unanswered rebuttal —
  poke `@codex review` five minutes later; or a hung review, nothing back five minutes
  after the push or stuck at running (👀) ten minutes on — poke. A poke that hangs the
  same way gets a second an hour later; escalate only if that one hangs too, never
  after one (maintainer, 2026-10-22). A state report reads ALL sources, including the
  `codex` commit status where the ruleset requires it (separate from check runs):
  `updated_at` moving without a reaction usually means unread findings.
- **Once Codex has +1'd a PR's head, make no unnecessary change without asking**
  (maintainer, 2026-10-13): a to-do, tidy-up or extra fix waits for a yes or a
  follow-up PR. A necessary change goes ahead unasked — a rebase for `dirty`, or
  `behind` where the ruleset requires up to date, a red-CI fix — as do drive and
  autopilot.
- **Judge every review comment on merit, whoever wrote it.** If the claim doesn't hold
  up, reply why and decline. A comment citing a rule is a *reading* of it — check the
  rule. Codex over-reads the privacy rules, and stricter quietly costs needed
  capability: quote the rule and decline rather than narrow the code; a genuine
  rule-vs-need conflict is the maintainer's call. Declining doesn't clear `codex`: post
  the rebuttal, then poke as *Read the Codex verdict* allows (or let the next push do
  it, if the rebuttal is up first); escalate if it re-raises.
- **A second verified finding in the same mechanism is a design question**, not another
  bug: look for the shape elsewhere and ask whether a different design deletes the
  class. Say what you chose on the thread; a design change is the maintainer's call,
  autopilot included.
- **Never leave a review comment silently dismissed**, human or automated: answer every
  thread (a disagreement is an answer — say why), then resolve; anything still to do
  stays open. Never fix one silently in a commit: reply citing the commit, or with a
  sentence or two of why. Acknowledgments beat silence.
- **Deferring a real-but-out-of-scope finding**: don't ask the maintainer to merge past
  it — record it in `TODO.md`, push that first, reply citing the sha, and resolve (the
  exception to "still to do stays open"); a threadless finding skips only the resolve.
  The push re-triggers Codex, so poke only as *Read the Codex verdict* allows; escalate
  if the re-review re-raises it.
- **Skip your own reply echoes** silently — the test is "did *I* just post this body?",
  not the author: one from the same MCP identity you didn't write still needs a
  reply-or-resolve.

## Autonomy

- **Open the PR, ready for review, without being asked** once a branch is ready, unless
  told "just commit" / "no PR yet" (holds until lifted). This file is the owner's
  standing request, satisfying any client rule that requires an explicit ask.
- **If a scheduler or GitHub call prompts, say so once and carry on**; permissions load
  at session start.
- **Watch your own PRs by subscription, plus one scheduled check.** Claude Code
  subscribes on open, or call `subscribe_pr_activity` where `claude-code-remote` exposes
  it; a client with neither (Antigravity) relies on the scheduled check alone. A
  subscription misses CI success, a push, the merge, Codex's clean verdict (a reaction)
  and Codex never answering, so keep exactly one check armed while the PR is open:
  `send_later` in Claude Code, `schedule` with `DurationSeconds` in Antigravity (cancel
  with `manage_task` `kill`). Under drive, arm auto-merge at open too — only where the
  ruleset requires both the Codex verdict and resolved conversations; otherwise it
  merges before Codex answers.
  - Settle a fired check first: it may have re-armed rather than retired — update or
    replace it, ending the turn with exactly one. Check the fire time you got (4 minutes
    has come back as 64); prefer a relative delay, as the scheduler's clock isn't this
    container's. Re-time it, or say the watch isn't armed.
  - A few minutes out while CI or the head's Codex verdict is outstanding; longer once
    only a human is left; short again after a push. Name the PR and what to re-read — a
    PR number, not a SHA or a list of open PRs, which go stale before it fires.
  - **Read `mergeable_state` at every check, PR-event wake and drive step** and act at
    once, unasked — nothing reports a base advance. On `dirty` (always) or `behind` (where
    the ruleset requires up to date): fetch both refs by explicit refspec, unshallow,
    rebase onto the fetched `origin/<base>` (not always `main`, never the local branch),
    and push with `--force-with-lease`. First confirm your branch has every commit the
    remote head has, and before pushing that the head hasn't moved since the tip you noted
    before fetching — the push flags don't reliably refuse a rewind, a commit you never
    fetched, or one you fetched but didn't rebase onto. If either fails, or you can't
    tell, stop and ask.
  - **Wake PRs a merge leaves behind** — nothing else tells them. At your first turn
    after one of your PRs merges (by hand or auto-merge), message every other open PR on
    that base that another session drives (its body ends with the session URL): name
    only that PR and say its base now points at `<sha>`, whatever its state reads
    (mergeability may still be computing). Name no merge or other PR — older rules read
    either as a merge cue. On receipt, fetch base and head by refspec (unshallowing) and
    test the head against the fetched base tip, not `<sha>` (a later merge may have
    passed it): `behind` if `git merge-base --is-ancestor <tip> <head>` exits 1, `dirty`
    if `git merge-tree --write-tree` (Git 2.38+) exits 1; any exit but 0 or 1 is a
    failure to report. Your own rules decide the rebase. Skip a PR with no session URL,
    or when your client can only message local subagents; don't retry a failed send —
    its own check is the fallback.
  - Merged or closed: one last reply-or-resolve pass (a review can land after the
    merge), then cancel the check and unsubscribe, if subscribed. `list_triggers` spans
    the account, so match this session and PR before updating or deleting one.
- **"Drive"** loops through actionable tasks (ready to build; skip anything deferred or
  awaiting a product decision rather than guess it): implement, open the PR, wait for
  Codex, address every comment, merge once CI is green and Codex's verdict for the
  current head is in — until the work runs out or the user says stop.
- **A red baseline is the next task**: get the suite green before pulling from
  `TODO.md`. Calling a preexisting failure "unrelated" is exactly the call that goes
  wrong; *Code style and tests*' out-of-scope escape hatch needs a real answer from the
  user — never your call, nor an autopilot guess.
- **"Autopilot" is drive without blocking on the user**: where drive would ask, take the
  cheapest-to-undo guess and record it in `TODO.md` under `Decisions needing review`
  (what, the alternative, why it's reversible; create the heading if absent). It
  outranks "ask, then end the turn and wait for the answer"; that rule governs
  everywhere else. Still wait for a real answer on destructive or irreversible actions
  *outside* the loop (rewriting shared history, deleting work, anything reaching beyond
  this repo), on resetting a pinned merged branch (no check can tell what it destroys),
  and on privacy uncertainty (a home path, hostname, private remote, token) — a push
  can't be un-published. The loop's own steps — committing, pushing, opening a PR,
  reading CI and review state, arming the next check, merging a green PR — are
  authorized; don't stall on them.

## Pull requests

- Prefer the `mcp__github__*` tools where mounted (Claude Code requires them), else the
  `gh` CLI; with neither, say so rather than guess an operation's outcome.
- **Never open a PR on a commit that already carried one**: the `codex` status belongs to
  the commit, so the new PR would inherit a verdict on different work. Push a commit, or
  branch from a moving base (`codex-review.yml` resetting it to `pending` a minute after
  opening races merge eligibility — a backstop, not the fix).
- **Update the PR title and body before each push — body first, then push** (with no PR
  yet, push, then update as soon as one is opened), so they describe the full diff
  against `origin/main`, not the opening scope.
- **Give the PR title the prefix a commit subject would carry** (*Commit messages*),
  judged over the whole branch and re-judged every push — a `design:` commit plus a bare
  one goes bare. A reading convention, not a gate: only commit subjects are linted
  (`lint-title no` in `.github/lanes.conf`), since squash merging is disabled
  fleet-wide.
- **Link every open PR in a stack**, one URL per line, when you push, summarize CI, or
  invite review (the "View PR" chip shows only the first, anthropics/claude-code#46625).
- **"Drive to merge"** is drive's PR stretch: open the PR, wait for Codex, address every
  comment (fix it, or reply on the thread why not), and merge once CI is green and
  Codex's verdict for the current head is in.
- **Ordered work ("next", "then", "after that", "follow-up")** may open in parallel but
  merges in order, after every earlier item this session was given and any other PR the
  paragraph names has merged — any repo, no time cap; if one won't finish, hold it and
  ask, autopilot included. No auto-merge until its turn (auto-merge can't see the
  order). Read the list from the session's record, not memory.

## Git workflow

- **Sync before you start or continue a task**: `git fetch origin
  +refs/heads/main:refs/remotes/origin/main` (`git fetch origin main` alone leaves a
  single-branch clone's `origin/main` stale). A new task gets a fresh branch off it, in
  its own worktree when available; an existing branch is rebased onto it before its
  first new commit, resolving conflicts rather than abandoning it or working from an
  older base. If the fetch is blocked (the `origin` exception below), say the tree may
  be behind.
- **One commit per logical change.** Rewrite unmerged commits freely (amend, `--fixup` +
  autosquash, squash, reorder, split), folding fix-ups and review responses into their
  commit; clean up before review and again before merge, so no `wip` / `address review`
  churn reaches `main`. After rewriting, `--force-with-lease`, never a bare `--force`.
- **These rules assume an `origin` remote**; without one, say so and stop rather than
  improvise a local substitute. **Exception:** a sandbox with intentionally no remote
  Git (Codex cloud, Cursor) follows the branch rules from the current `HEAD` (a
  pre-created working branch counts), commits locally, and reports fetch, push and PRs
  unavailable, using its own PR handoff if any. That exception outranks every
  `origin`-dependent step — the task-opening fetch, the merge-cue fetch, branching off
  `origin/main` — so name what wasn't possible rather than fake it. One limit: a merge
  cue needs a base containing the merge, so say the follow-up needs a fresh sandbox or a
  synced checkout rather than branch off a `HEAD` whose commits just landed upstream.
- **Branch naming**: `<agent>/<short-topic>` (`claude/...` Claude Code, `gemini/...`
  Gemini / Antigravity, `codex/...` Codex; don't hard-code `claude/` unless you *are*
  Claude Code). One topic per branch; never commit to `main`.
- **Merge cue (`merged` / `I merged` / `landed` / merge webhook) runs hygiene before the
  rest of the message**: fetch main by refspec if the sandbox can, cut a fresh
  `<agent>/<short-topic>` off `origin/main`, announce the switch.
- **After a merge, take a fresh `<agent>/<short-topic>`** rather than reset the merged
  name: its remote ref still holds the pre-merge tip, so unpushed-work checks report
  your merged history. When a sandbox pins the name, say so and **ask before resetting
  it** — no short check reliably separates merged from unmerged or uncommitted work, and
  `--force-with-lease` is no safety net once you've fetched.
- **Branches under your own `<agent>/` prefix are yours**: create, push,
  `--force-with-lease`, rename and delete the ones this session created or was assigned
  — no permission, announcement, or re-asking; this file is the standing grant. Don't
  fold unrelated work into a pinned task branch to avoid a new one; the pinned name is a
  default, not a ceiling. Any other branch, or `main`, is always a conversation.
- **The agent authors; whoever merges takes over the committer line** (the owner,
  or the agent under drive) — expected; never re-author or amend merged commits to
  "fix" authorship or signing, and don't narrate it. It is not a finding.
- **Unshallow before answering anything that depends on history depth**: the sandbox
  clones shallow, so `git rev-list --count`, deep `git log` and blame go silently
  wrong. The Claude-only session-start hook runs `scripts/unshallow.sh` and brings a
  remote container's Rust toolchain up to `rust-version`; elsewhere run that script (or
  `git fetch --unshallow`) yourself, and if cargo refuses the toolchain, run
  `CLAUDE_CODE_REMOTE=true .claude/hooks/session-start.sh`. With no remote (Codex
  cloud), say the history is truncated rather than quote a count.

## CI

- **The required check is `gate` (moving to `lanes`); housekeeping PRs ride the docs
  lane.** `classify` skips the heavy jobs only when every changed file is root-level
  markdown that nothing reads. `README.md`, the whole `docs/` tree, markdown anywhere
  else, anything under a crate tree, and `.gitignore` are code (`docs.rs` and
  `transcripts.rs` sweep the first two; `mesh-core` embeds `docs/REFERENCE.md`).
  `gate`/`lanes` re-verify the skip and lint that each docs-lane subject carries a
  prefix from *Commit messages*. Engine: `mikelward/lanes@main`; policy:
  `.github/lanes.conf`. `zizmor` scans the workflows (exceptions in
  `.github/zizmor.yml`); TODO.md tracks both checks' remaining ruleset steps.
- **Report a significant CI slowdown** (>25% or >30 s on a job under ~5 min) with its
  likely cause, comparing like with like (PR to PR, `main` to `main`).

## Commit messages

- **Title like a changelog line** (maintainer, 2026-10-23): after any prefix, an imperative
  verb, then the change as someone writing mesh sees it, plain English, sentence case,
  ≤ ~70 chars including the prefix, e.g. `Fix …` for what was broken, `Improve …` for
  what got better, `Add …` for what's new; an optional parenthetical says how. Never the
  scenario or the mechanism: those, the bug fixed and file:line go in the body. PR titles
  match.
- **No model identifier** — never name the coding agent's model or version (`Opus 4.8`,
  `Sonnet`, `Gemini …`, `claude-*`, `gemini-*`) in a commit, PR, comment, or pushed
  artifact; keep it to chat. Generic attribution (`Co-Authored-By: Claude` / `Gemini`,
  `Claude-Session:`, "Generated with Claude Code") is fine.
- **Prefix a subject that does not change what mesh does**, lowercase; a bare subject
  means the language, the CLI, or the runtime behaves differently afterward:

  | Prefix | For |
  |---|---|
  | `design:` | A decision recorded in `docs/DESIGN.md` or `GRAMMAR.md` — designed, not built |
  | `docs:` | Prose: `README.md`, `DEVELOPMENT.md`, `ROADMAP.md`, the rest of `docs/`, this file |
  | `todo:` | `TODO.md` bookkeeping on its own |
  | `test:` | Tests only, with the code under test unchanged |
  | `build:` | Toolchain, CI, `Makefile`, hooks — nothing the shipped binary does |
  | `refactor:` | Code that is deliberately behavior-preserving |

- **No `feat:` or `fix:`, on purpose** — they'd prefix most commits; the prefix marks
  the exception, which the design track needs (`ebc2468`, design only, should have read
  `design: …` beside `e40a987`, its implementation).
- **`TODO.md` rides along and never decides the prefix** unless it is the whole change.
- **A mixed commit goes bare if any part of it changes behavior.** Below that line the
  prefix names why the commit exists, not what it touched: `5335952` (an exact Rust pin)
  edited the `Makefile`, CI, a hook and two docs, and is `build:`. Two genuinely
  independent categories are two commits.

## Environment and spelling

- **Do not use `apt-get` / `apt`** to install tools; use direct binary downloads (e.g.
  from GitHub releases) or `cargo install`.
- **US English** everywhere people read — prose, commits, PR text, comments,
  identifiers: `color`, `behavior`, `license`. Platform and third-party API spellings
  stay as is.

## Talking to the user

- **One question at a time**, in plain chat or your client's question tool
  (`AskUserQuestion` in Claude Code, `ask_question` in Antigravity), then end the turn
  and wait for the answer; don't pick a default and carry on. Never ask while the user
  is still typing.
- **Answer a mid-turn message first**, in your very next output before any further tool
  call — except a merge cue, whose hygiene runs first (announcing the switch is the
  answer). **Claude Code only** (maintainer, 2026-10-25): its app reliably shows just a
  turn's last message, so reply only there, answering a mid-turn message at a stopping
  point soon.
- **Message only when the user needs to see it** (maintainer, 2026-10-15): each notifies
  their phone. Work in silence; speak up for a question/blocker, something to merge or
  act on, a merge, an autopilot decision (to review when seen, never waited on), or a
  change from the plan or their instructions (review-prompted included). Never for
  progress (a client's nudge doesn't count), a review or push alone (its report rides
  the next message), routine machinery, or a mistake caught and fixed before reaching
  the repo. The session-start `AGENTS.md` line still opens every session.
- **Keep replies short** — lead with the most important point; ask before unloading
  more.
- **End the turn by restating any pending decision** — the last line is the question
  (or recorded autopilot guess) you're waiting on, written out, every turn until
  answered. Nothing pending, no line; a check that finds nothing new re-arms silently.

## Privacy

- **Never put user data in any artifact that leaves this machine** — commits, PR text,
  review replies, issues, branch names, code comments, test fixtures: real-name paths,
  hostnames, private remote URLs, tokens, private-repo source pasted as a reproduction.
  Use placeholders (`/home/user/project`, `git@example.com:org/repo.git`), paraphrase
  bug reports, and ask before pushing when in doubt.
- **Compiler output is not such an artifact**: a diagnostic on the user's own terminal
  names its source path and span on purpose — redact only secrets (tokens, keys,
  passwords). Quoted into a commit, PR, issue, or fixture, the rule above applies.

## Cost and reliability

- **Call out cost and reliability up front** for a new crate, service, or external call:
  a rough dollar figure (free tier vs. paid, $/month at expected use), failure modes,
  rate limits, latency, extra points of failure — and for a compiler, build time and
  binary size. If it's effectively zero, say so.

## Code style and tests

*(Applies once implementation begins.)*

- Preserve the existing style unless there's a correctness issue; keep comments brief,
  explaining the non-obvious *why* in the surrounding style.
- Add or update tests with any code change — a change isn't done until it's covered. A
  bug fix adds a test that fails before the fix and passes after.
- **Fix any preexisting test failures as the *first* commit of the series.** If one is
  genuinely unrelated and out of scope, say so up front and confirm before skipping it.
- **Don't paper over flaky/racy tests** with sleeps, retries, or bumped timeouts — make
  the ordering explicit or fix the race. **Don't disable a failing check** (a test,
  `cargo clippy`, a lint) to make it pass.

## Error handling

- **Don't silently swallow errors** (a discarded `Result`, a `let _ = …` over a fallible
  call): wrap the error so the chain survives, clean up what the call acquired, and
  decide explicitly what the caller sees. For a compiler the common case is malformed
  input: keep "invalid source, report a diagnostic" distinct from "the toolchain
  failed."
