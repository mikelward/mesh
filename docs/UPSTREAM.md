# What could go upstream

mesh exists because some shell problems need a clean break. But not all of them
do — and it is worth knowing which of mesh's ideas could simply be *contributed*
to a shell people already use, rather than waiting for a new one.

This page sorts mesh's distinctive features by whether another shell could adopt
them, and names what each shell already has. Everything here was measured
against a shell built and run locally: zsh 5.9, fish 4.0.2, elvish 0.21.0,
nushell 0.114.1, bash 5.2.21 — except where a table names a different version,
which means that row was measured on the build actually to hand.

## The structural finding

**Most of what makes mesh distinctive is not upstreamable, by construction.**

`'…'` taking escapes, a command substitution that stays one string, zero-based
indexing, the `:kind` / `:where` modifiers — these are grammar and core
semantics. (Not *every* row is mesh-only. zsh, fish, elvish and nushell all
dropped word splitting already, and nushell's conditions are boolean-only —
shared positions rather than mesh inventions. Two more are partial: fish's
`'…'` takes `\'` and `\\`, and zsh indexes from zero under `KSH_ARRAYS`.) That
last one is the whole argument in miniature: the *option* already traveled, and
nobody uses it, because what a script depends on is the default. Adopting these
means breaking every existing script, which is the thing an established shell
cannot do and the reason mesh is a
[clean break](DESIGN.md#core-decisions) in the first place. A shell that *could*
accept them would not need them.

What is left is the **interactive plumbing** — hooks, completion, job
reporting. That layer is additive: a shell can gain a hook or a completion
source without changing what any existing line means. That is where the real
candidates are, and there are fewer of them than you would hope, because the
other shells have been busy.

## Where each idea stands

| mesh feature | bash | zsh | fish | elvish | nushell | upstreamable? |
| --- | --- | --- | --- | --- | --- | --- |
| Derived completion, on demand | — | `_gnu_generic`, opt-in, `--help` only | one Python batch at first start | — | — | **yes — the strongest candidate** |
| Exit hook | `trap EXIT` | `zshexit` | `fish_exit` | `$before-exit` | **none** | **yes — nushell** |
| Post-execution *event* | `PROMPT_COMMAND` + `DEBUG` trap | hand-rolled | `fish_postexec` | `after-command` | `pre_prompt` only | yes — nushell, narrowly |
| Native command duration | hand-rolled | hand-rolled | `CMD_DURATION` | `$edit:command-duration` | `$env.CMD_DURATION_MS` | yes — bash and zsh |
| Job-completion event *with a payload* | `trap … CHLD`, no payload | `TRAPCHLD`, signal number only | `fish_job_summary` | — | — | yes — the payload, not the event |
| Named, removable hook registry | — | `add-zsh-hook` arrays | events by function name | positional list | fixed key set | maybe — see below |
| Interpolating string | yes | yes | yes | **none** | `$"…"` | yes, but a hard sell |
| A failed early stage fails the pipeline | opt-in `pipefail` | opt-in `pipefail` | **no option** | always — exception | always — aborts | no — a default flip breaks scripts |
| No word splitting | — | yes | yes | yes | yes | **no — grammar** |
| Command substitution is not implicitly split | — | — | — | — | yes, as one *value* | **no — grammar** |
| `'…'` takes escapes | — | — | `\'` and `\\` only | — | — | **no — grammar** |
| Zero-based indexing | yes | 1-based; 0 under `KSH_ARRAYS` | 1-based | yes | yes | **no — grammar** |
| Conditions are not coerced | — | — | — | value-typed | **boolean-only** | **no — semantics** |

## The candidates, ranked

### 1. An exit hook for nushell

Measured on 0.114.1: the hook set is `pre_prompt`, `pre_execution`,
`env_change`, `display_output`, `command_not_found`. There is no exit hook, so
anything a session creates cannot be cleaned up when the session ends — the
concrete case in our own notes is a job-publish file that outlives the shell.

Small, self-contained, and clearly useful. nushell moves fast and takes feature
PRs. This is the single most likely thing on this page to land.

### 2. A post-execution hook for nushell

A narrower gap than it first looks, and worth stating carefully. There is no
event that fires *after a command*, but a post-command reaction is not homeless:
`pre_prompt` runs before the next prompt is drawn, and by then
`$env.LAST_EXIT_CODE` and `$env.CMD_DURATION_MS` are both set — which is exactly
how nushell users feed a duration to Starship today.

So what is missing is not the capability but the *shape*:

- **Timing.** `pre_prompt` fires when the next prompt is drawn rather than when
  the command ends. In the ordinary interactive case that is a distinction
  without a difference — drawing the prompt *is* the next thing the shell does,
  so a notification is not meaningfully delayed. It bites where no prompt
  follows: the session exiting, and non-interactive use, since nushell's hooks
  are REPL-only and do not run for `nu -c` or a script at all.
- **Payload.** The reaction reads ambient environment variables rather than
  receiving what just ran. There is no command text, and a handler cannot tell
  a fresh value from a stale one.

Still worth proposing, and still naturally the same PR as the exit hook — but as
"an event at completion, carrying what completed", not as filling a void. The
payload is where the design choice is: see [Payload shape](#payload-shape).

### 3. On-demand derived completion, for fish or zsh

The genuine novelty, and the one worth the most to the most people.

What exists today:

- **fish** runs `create_manpage_completions.py` **once**, as a background Python
  batch over the whole manpath, on first interactive start (it keys on the
  cache directory not existing). Install a tool afterwards and it has no
  completions until you remember `fish_update_completions`.
- **zsh** ships `_gnu_generic`, which reads a command's `--help` — but you opt
  in per command with `compdef`, and it never looks at man pages.

Nobody consults **both** sources, **lazily**, per command, at the moment you
press Tab. That is [what mesh does](DESIGN.md#completion), and nothing about it
requires mesh's grammar: it is a completion source, which is exactly the kind of
thing both shells are built to accept.

This is the largest of these by implementation effort — caching, invalidation,
and a parser that fails soft when a tool's `--help` is not machine-readable.
It is still purely additive; no existing completion changes behavior.

**The cost has to be stated up front, because it is not only effort.** The
man-page source runs a formatter over a data file, but the `--help` source
**runs the command itself**, at the moment you press Tab, which buys three
problems a maintainer will raise immediately:

- **Latency on the first Tab** for any command not yet cached.
- **Side effects.** Not every binary treats `--help` as inert; some connect to
  the network, some exit non-zero, some ignore the flag and do their actual job.
- **Misbehaving executables** that hang, or write megabytes.

mesh's own mitigations are the minimum an upstream proposal would need, and are
worth quoting rather than rediscovering: null stdin, a two-second timeout, a
one-MiB output cap, and a cache so the probe happens once per command
([`REFERENCE.md`](REFERENCE.md)). The ordering matters too — the curated file
and the man page are tried *first*, so the probe is the fallback rather than the
default path.

What the cost is *not* is money. Both sources are local: a formatter over a
file already on disk, and a process the user could have run themselves. There
is no service, no API key, no rate limit, and no per-Tab charge — **$0 in direct
cost at any usage**. The whole price is paid in local CPU and in the latency of
that first uncached Tab, which is why the mitigations above are about time and
blast radius rather than budget. Worth stating plainly, because "runs the
command at Tab time" invites the question.

None of that makes it a bad idea; the man-page half is free of the objection
entirely, and could go upstream on its own. But a proposal that leads with "we
run the command" and its mitigations will fare better than one that gets asked.

### 4. A native command duration for bash and zsh

fish has `CMD_DURATION`, elvish has `$edit:command-duration`, nushell has
`$env.CMD_DURATION_MS`. bash and zsh have nothing, so every prompt framework
hand-rolls it — an `$EPOCHREALTIME` (or `$SECONDS`) stamp in a `preexec`/`DEBUG`
trap, subtracted at prompt time — which is why the same twenty lines ship in
theme after theme. A built-in variable is a small, backward-compatible addition
to either.

### 5. A job-completion hook, for bash / zsh / elvish / nushell

fish is furthest along, though not strictly ahead: `fish_job_summary` receives
the job id, whether it was foreground, the command line, the signal name and
description, and the pid and name of the affected process. What it does *not*
pass is a numeric exit status — an ordinary completion arrives as the literal
`ENDED`, so a notifier built on these arguments alone cannot tell a build that
succeeded from one that failed. mesh's `jobdone` — [shipped
today](REFERENCE.md), carrying `id, command, status` — has the status and not
the foreground flag or the signal detail, so the two payloads trade against each
other rather than one containing the other.

Neither bash nor zsh is without an event — both trap `SIGCHLD` when a child
changes state, and that is genuinely event-driven, not polling:

```
$ bash -c 'trap "echo fired: args=[$@]" CHLD; sleep 0.2 & wait'
fired: args=[]

% zsh -f -c 'TRAPCHLD() { print "fired: args=[$@]" }; sleep 0.2 & wait'
fired: args=[17]
```

But that is the whole payload — nothing in bash, the signal number in zsh. The
handler is told *that* a child changed state, not which one or how it ended, and
`$!` is no help in either: it holds the last pid backgrounded rather than the one
that just changed, and stays identical across firings when two jobs finish. To
answer "what finished" you go and read the job table.

So the event saves a polling loop and still leaves you inspecting global state.
That is the gap worth proposing to all four: not the notification, which bash and
zsh already have, but a **payload identifying the job**.

One design constraint on that payload, which the signal mechanism imposes rather
than any shell: `SIGCHLD` is a standard signal, and standard signals do not
queue — several arriving while one is pending collapse into a single delivery.
So a hook must not be *specified* as firing once per job. Measured here it
effectively does (50 instant-exit children gave 50 firings in zsh and 51 in bash,
over three runs each), but that is an artifact of both shells reaping in a
`waitpid` loop rather than counting signals, and it is not a guarantee to design
against. A payload should therefore carry — or let the handler drain — *all*
jobs that changed since the last call, not one.

### 6. An interpolating string for elvish

A real gap: elvish has no interpolating string at all, so `"hi $n"` is literal
and you concatenate — `"hi "$n`. Every other shell here has one.

Listed last because it is a deliberate design position rather than an oversight,
so it is a hard sell however real the ergonomic cost.

## Will they take it?

The ranking above is about how *adoptable* each idea is. Whether a project
accepts outside work is a separate question, and easy to be wrong about in
either direction.

One data point beats speculation here: a patch from this repo's author landed in
zsh — `Fix %- (prevjob) picking wrong job after resuming`, touching `Src/jobs.c`
with tests in `A05execution.ztst` and `W02jobs.ztst`. That is job-control
internals, from an outside contributor, and it went in.

Two things follow. The first is that zsh takes real patches, so the zsh items
above deserve to be weighed on their merits rather than discounted in advance.
The second is more pointed: [DESIGN.md](DESIGN.md#footguns-we-avoid) cites
"a long tail of job-control surprises" as a reason zsh cannot be the answer, and
one of those surprises has now been fixed *in zsh*. The tail is not a fixed
property of the shell; it is a list of bugs, and bugs can be closed by whoever
cares enough to find them.

That does not undermine the case for mesh, because the grammar-level items are
still immovable and they are the ones that matter. But it does mean the
argument has to rest on those, not on an assumption that the fixable things
will never be fixed.

The acceptance question was also slow to answer: that patch was merged roughly
two months after it was sent, with no notification to its author. Worth knowing
before treating silence as rejection.

## Payload shape

Worth separating from the registry question, because the two get conflated.

**Registry** — how handlers are stored and identified. mesh keys on the
`(event, name)` pair, so re-registering replaces in place and
`on --remove exit k` removes one handler by identity. elvish's `after-command`
is a plain list: handlers are positional and anonymous, so the idiomatic append
duplicates every time an rc file is re-sourced, and removing one means
rebuilding the list. That is the `PROMPT_COMMAND` stacking bug, which
[HOOKS.md](HOOKS.md) exists partly to avoid. **mesh's registry is the better
design and has nothing to learn here.**

**Payload** — what a handler receives. This is where elvish is ahead: it passes
a single *map* (`src`, `duration`, `error`), so the payload can gain a key
without breaking any existing handler. fish passes *positional* parameters to
`fish_job_summary`, and — worse than a fixed list — **how many depends on what
happened**. From `summary_command` in `src/proc.rs`, which is commented as
implementing exactly what the function expects:

| Case | Arguments passed |
| --- | --- |
| A job stopped or ended | 4 — id, foreground, command, `STOPPED`/`ENDED` |
| One process died on a signal | 5 — the above, with the signal name and its description in place of the fourth |
| …and the job had several processes | 7 — plus the pid and name of the one that died |

So a handler cannot read its fifth argument without first working out which case
it is in, and there is no room to add a field to one case without shifting the
others. A map has neither problem: absent keys are absent, and a new key is
invisible to handlers that do not look for it.

For a shell adding a hook today — including nushell in items 1 and 2 above —
the map payload is the part worth copying, and it costs nothing to get right at
the start.

## Already solved upstream

Three things our own documentation treats as mesh advantages that measurement
does not support.

**`lastpipe` is table stakes, not differentiation.** DESIGN.md's [footgun
list](DESIGN.md#footguns-we-avoid) carries "the last stage of a pipeline runs in
the current shell" as *(planned)*. Measured with
`n=0; seq 3 | while read x; ... ; echo $n`:

| bash | zsh | fish | elvish |
| --- | --- | --- | --- |
| `0` (`3` under `lastpipe`) | `3` | `3` | `3` |

Bash is the only one that forks the last stage *by default*, and DESIGN.md is
right to name `lastpipe` as the opt-in it is. The bash cell has a second
condition the others do not: `shopt -s lastpipe` is ignored while job control is
on, so the option does nothing in the interactive shell where you would type it —
`set +m` is required as well.

That does not rescue the entry. Three of four shells here keep the last stage in
the current shell with nothing to enable, so getting this right puts mesh level
with every non-bash shell rather than ahead of any of them — which is the claim
the footgun list is making. The gap is a default, and it is bash's alone.

**A per-stage status array is table stakes too.** The table row above compares
only the *aggregate* status, because that is the part that differs. Seeing which
stage failed does not differ at all — bash, zsh, and fish each populate a
per-stage array unconditionally, with nothing to enable:

| | `false \| true` | aggregate status |
| --- | --- | --- |
| bash 5.2.21 | `PIPESTATUS=(1 0)` | `0` |
| zsh 5.8 | `pipestatus=(1 0)` | `0` |
| fish 4.0.2 | `pipestatus=(1 0)` | `0` |

The three agree on both axes by default, and fish is the one with no way out:
`pipefail` appears nowhere in its documentation or its `status features` list,
so an early failure can only ever be read out of `$pipestatus` by hand.
(elvish and nushell have no array because they need none — the failure raises,
and the error names every stage that failed.)

mesh's edge is that `$sh.pipestatus` is a real list rather than bash's magic
array, and that the pipefail rule is on with no way off — not that the per-stage
breakdown exists.

**fish's hooks are stronger than we imply.** fish has `fish_preexec`,
`fish_postexec`, `fish_exit`, `fish_job_summary`, and `CMD_DURATION`, and its
`--on-event` functions are a named, composable registry. The hook advantage
mesh has over fish is narrow — mainly the removable `(event, name)` key.

## What mesh could take

The exercise runs both ways:

- **elvish's map payload** for hook handlers (above).
- **two fields from fish's `fish_job_summary`** that mesh's shipped `jobdone`
  (`id`, `command`, `status`) lacks: the foreground flag, and the
  signal-versus-exit distinction. Both are things a notifier wants. This is a
  swap of fields, not a wholesale adoption — fish passes no numeric status,
  which `jobdone` has and should keep.

## Method

Every claim on this page was checked against a locally built shell rather than
documentation, because documentation and behavior have diverged repeatedly in
this comparison work. Where a claim rests on reading source rather than running
code — fish's completion trigger, elvish's hook payload — the file is named so
the next person can re-check it:

- fish's one-shot generation: `share/functions/__fish_config_interactive.fish`
- fish's hook events: `src/reader.rs`
- fish's job-summary arguments: `src/proc.rs` (`summary_command`), cross-checked
  against the parameter list of the shipped `fish_job_summary.fish`. A live run
  needs a real interactive session, which the sandbox could not provide.
- elvish's `after-command` payload: `pkg/edit/repl.d.elv`, `pkg/edit/repl.go`

### Absence is harder to measure than presence

A page like this is mostly *absence* claims — shell X cannot do Y — and those
need a different search than presence claims do. The first draft of this page
got that wrong in **ten** cells of one table, across the review rounds on the
pull request that added it — worth recording as a property of the exercise
rather than as ten separate slips:

| Claimed | Actually |
| --- | --- |
| nushell has no command duration | `$env.CMD_DURATION_MS`, already feeding Starship |
| nushell has no post-command reaction | `pre_prompt` with `$env.LAST_EXIT_CODE` set |
| zsh needs polling for job completion | `TRAPCHLD` is a real event |
| bash has no post-command mechanism | `PROMPT_COMMAND`, plus a `DEBUG` trap for the command text |
| zsh word-splits | it does not — `x="a b"; printf "[%s]" $x` gives `[a b]` |
| elvish has no exit hook | `$before-exit`, in `pkg/eval/eval.go` |
| bash has no job-completion event | `trap … CHLD` fires, with no payload |
| fish's `'…'` takes no escapes | `\'` and `\\` both work — "nearly raw", not raw |
| zsh indexes from 1 | by default; `setopt KSH_ARRAYS` makes it 0-based |
| bash forks the last pipeline stage | by default; `shopt -s lastpipe` with `set +m` does not |

Every one has the same shape: a feature was looked for in one place, not found,
and declared missing — when the capability lived somewhere else in the shell.
A hook list does not mention an environment variable. A hook list does not
mention a trap. An option you have never set does not appear in the behavior you
observe.

The last two rows are a sub-shape worth naming on its own: the mechanism was
*there*, gated behind an option, and a transcript of default behavior can never
show it. Both also turned out to argue *for* the page's conclusion rather than
against it — the option shipped years ago and changed nothing, because what a
script depends on is the default. `lastpipe` makes the point twice over: it is
opt-in, and even opted in it stays inert until you also disable job control.

Running a command proves what a shell *does*. It never proves what a shell
*cannot* do — a null result only says the mechanism is not the one you tried.
Before writing "X has no Y", check the neighboring mechanisms: an environment
variable rather than a hook, a trap rather than an event, an ambient value
rather than an argument.

Two practical consequences for anyone extending this page. **A `—` is a claim,
not a blank** — it asserts a shell cannot do something, and deserves the same
evidence as a filled cell, which is why the ones above are now spelled out as
mechanisms rather than dashes. And **the corrected claims are consistently more
useful than the ones they replaced**: the nushell gap turned out to be a hook's
*timing and payload* rather than a missing capability, and the zsh gap a
*payload* rather than a missing notification. Overstating an absence does not
just risk being wrong; it hides the real, narrower gap that is worth proposing.

Where this page and [COMPARISON.md](COMPARISON.md) describe the same behavior,
COMPARISON is the more heavily reviewed of the two and should win a
disagreement.
