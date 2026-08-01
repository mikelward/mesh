# Integrating external tools

A bash or zsh user arrives with a toolbox: [starship] draws their prompt,
[atuin] owns Ctrl-R, [fzf] owns Ctrl-T, [zoxide] owns `z`, [carapace] answers
Tab for a hundred commands, [direnv] rewrites the environment on every `cd`.
None of those are shells, but all of them are *shell integrations* — each one
plugs into a hook, a keybinding, or a completion system that bash and zsh both
happen to have.

mesh is a clean break, so none of those plugs fit as-is. This document works out
what each tool actually needs, what already works today, and what is missing.
The missing pieces are collected in [What is missing](#what-is-missing) and
tracked under "Beyond M3 — External tool integration" in
[`TODO.md`](../TODO.md).

Nothing here is a promise about *which* tools mesh will support. The point is
that the handful of hooks these tools want are the same handful, and knowing
what they are should shape the hook design rather than be retrofitted to it.

Every "works today" snippet below was run against a built `mesh`, and every
"blocked" claim is the shell's own error message rather than a reading of the
source.

[starship]: https://starship.rs
[atuin]: https://atuin.sh
[fzf]: https://github.com/junegunn/fzf
[zoxide]: https://github.com/ajeetdsouza/zoxide
[carapace]: https://carapace.sh
[direnv]: https://direnv.net

## Six ways a tool attaches

Sorted by how much of the shell they need. Almost every tool in the ecosystem is
one of these, or two of them stacked.

| | Attachment | What the shell must offer | Tools |
| --- | --- | --- | --- |
| 1 | **It is just a command** | Nothing | ripgrep, fd, bat, eza, delta, jq |
| 2 | **It renders part of the display** | A prompt slot that takes external text | starship, `vcs prompt-info` |
| 3 | **It watches the session** | `preexec` / `postexec` / `precd` hooks | atuin (recording), zoxide (recording), mcfly |
| 4 | **It answers a question the shell asks** | A completion-provider hook | carapace, `fzf-tab`-style menus |
| 5 | **It takes over the keyboard** | Keybindings **and** a line-buffer API | fzf widgets, atuin's Ctrl-R, `thefuck` |
| 6 | **It rewrites the environment** | Applying an env diff the tool computes | direnv, mise, nvm, pyenv, keychain |

Class 1 needs no work and is worth stating plainly, because mesh already
improves on it: since completion is
[generated from man pages and `--help`](REFERENCE.md#where-a-commands-completions-come-from),
a newly installed `rg` or `fd` completes its own flags with no completion script
to install and nothing to source. The zsh user's `fpath` juggling has no mesh
equivalent because it has no mesh problem.

Classes 2 and 3 work **today** — the directory hooks class 3 was waiting on have
landed. Class 4 waits on `$sh.complete`. Class 5 is the big hole: mesh has no
configurable keybindings and no way for code to read or replace the line being
edited. Class 6 has its hook now and can apply a computed set of environment
changes; what it still waits on is a way to read the tool's payload.

## The bootstrap problem

Every one of these tools ships the same instruction:

```sh
eval "$(atuin init zsh)"
eval "$(zoxide init bash)"
eval "$(direnv hook zsh)"
eval "$(starship init bash)"
```

`tool init <shell>` prints shell source, and the shell evaluates a string. mesh
can do neither half: there is **no `eval`**, and `source` takes exactly one file
operand — no pipe, no string, no `-`:

```text
mesh$ puts 'x = 1' | source
mesh: source: needs a file to run
mesh$ puts 'x = 1' | source -
mesh: source: -: No such file or directory (os error 2)
```

That is not an oversight to patch over on the way to supporting these tools; it
is a fork in the road, and it should be taken deliberately.

**Three ways out.**

1. **Let generated code in.** Add `source -` (or a `run TEXT` builtin) so
   `atuin init mesh | source` works, exactly as
   [`DESIGN.md`](../DESIGN.md#conditionals-if-is-an-expression) already sketches
   it. Cheapest to build, and it is the world's convention. The cost is that a
   mesh session's behavior is then defined by a string another program printed,
   which is the mechanism behind every "my prompt broke after an upgrade"
   report, and it hands arbitrary code execution to whatever is on `PATH` under
   that name.
2. **Ask upstream for a mesh target.** `atuin init mesh` emits real mesh — hook
   registrations, functions, keybindings. Same trust model as (1), but the
   output is written against a documented mesh API rather than machine-generated
   POSIX, so it can be read, and mesh keeps the right to say what an `init mesh`
   output is *allowed* to contain.
3. **Exchange data, not code.** The tool prints a description of what it wants —
   an environment diff, a list of candidates, a prompt string — and mesh applies
   it. `direnv export json`, `carapace … export`, and `starship prompt` already
   work this way; `mise env --json` does too. Nothing is evaluated, the failure
   modes are mesh's, and a malformed payload is a diagnostic instead of a
   half-applied config.

**The recommendation is (3) wherever the tool already offers it, (2) where it
does not, and (1) never as the default path.** Concretely: mesh should be able
to read a structured payload (JSON is what these tools emit) and apply an
environment diff, a completion answer, or a prompt fragment from it. That single
capability covers direnv, mise, carapace, and atuin's search output at once —
and it is the one thing on this list mesh cannot do at all today.

**What that costs, since it means a parser:** nothing new. `serde_json` is
already in the tree — reedline depends on it, so it is compiled into every mesh
build today (`cargo tree -i serde_json`). Reading JSON adds no dependency, no
new build time, and no measurable binary size; the only cost is deciding how a
JSON document maps onto mesh values (a null, a nested array, a number that is
not an integer) — a design question, not a procurement one. Per-call runtime
cost is the tool's own process spawn, which is the same whether the shell reads
its output or evaluates it.

The one honest cost of refusing `eval`: mesh cannot follow a tool's published
install instructions verbatim. Every integration below is a few lines in
`rc.mesh` instead of one `eval`. That is a real ergonomic tax, and it buys a
session whose behavior is written down in the user's own config.

## starship — the prompt

**Works today.** The
[reference](REFERENCE.md#custom-prompts-and-hooks) already shows the one-liner:

```mesh
func refresh-prompt() { prompt "$(starship prompt)" }
on preprompt renderer refresh-prompt
```

The interesting version passes starship the context it wants, all of which mesh
already exposes as values:

```mesh
global _cmd_elapsed = 0
func record-time(cmd, status, elapsed) { global _cmd_elapsed = $elapsed }
on postexec timing record-time

func refresh-prompt() {
  prompt "$(starship prompt --status=${sh.status} --jobs=${sh.jobs:len} --cmd-duration=${_cmd_elapsed})"
}
on preprompt renderer refresh-prompt
```

`$sh.status`, `$sh.jobs` (a live, indexable map — `:len` is the job count
starship asks for), and `postexec`'s `elapsed` in milliseconds are all
implemented, so nothing here is aspirational.

**The private-global convention works.** The stash above is `_cmd_elapsed`,
which is the name every bash/zsh config would give it: a mesh name starts with a
letter **or `_`**, so the leading underscore that marks a private global carries
over unchanged, and kebab and underscore are both fine after the first character.
Worth stating because every integration in this document needs one or two private
globals to carry state between hooks. Only the **bare** `_` is reserved — it is
the discard.

**One thing to know.** Do not set `STARSHIP_SHELL=zsh` or `=bash`. Those make
starship wrap its escape sequences in the shell-specific
"this is zero-width" markers (`%{…%}`, `\[…\]`) that mesh does not read and
would print literally. mesh measures the real display width itself and already
discounts SGR and OSC sequences, so unwrapped ANSI is exactly what it wants.

**What is missing.**

- **The prompt is one string, not segments.** The whole point of the
  [prompt design](PROMPT.md) is that an external renderer is *one named segment*
  among your own — framed by your `[root]` marker and your session warning
  rather than owning the line. That needs the `$sh.prompt` map, which is not
  implemented.
- **A multi-line external prompt.** `DESIGN.md` says raw external output is the
  one place `\n` is honored; today's `prompt` builtin takes a single string, so
  starship's multi-line presets have not been through their paces.
- **No right prompt.** starship's `--right` output has nowhere to go until
  `fill` lands.
- **No transient prompt.** Redrawing the previous prompt as something shorter
  after Enter needs a redraw hook the editor does not expose.
- **Upstream:** `starship init mesh` and a mesh entry in starship's shell list.
  Not needed for the above — `starship prompt` is shell-agnostic — but it is
  what makes mesh appear in starship's docs.

## atuin — history

atuin is two features that arrive together, and they land very differently.

**Recording works today.** atuin's own bash/zsh integration is `preexec` /
`precmd`, and mesh has those:

```mesh
global _atuin_id = ""

func atuin-start(cmd) {
  global _atuin_id = $(atuin history start -- $cmd)
}

func atuin-end(cmd, status, elapsed) {
  if $_atuin_id != "" {
    atuin history end --exit $status --duration ($elapsed * 1000000) $_atuin_id
    global _atuin_id = ""
  }
}

on preexec atuin atuin-start
on postexec atuin atuin-end
```

(atuin times the command itself if `--duration` is omitted; the flag takes
nanoseconds, and `elapsed` is milliseconds.) Pair it with
`--no-save-history` if atuin should be the only store — see below for why that
question is not as simple as it looks.

**Search does not, and cannot be worked around.** `atuin search -i` is a
full-screen picker that prints the chosen command; the shell is supposed to bind
Ctrl-R to it and put the result *into the line buffer*, where the user can edit
it before pressing Enter. mesh can run the picker — terminal handoff to
full-screen programs landed in M2 — but it has nowhere to put the answer.
Everything is missing at once:

- no way to bind Ctrl-R (or anything else) from `rc.mesh`;
- no widget concept — a mesh function that runs *during* line editing rather
  than as a command;
- no line-buffer API: read the buffer and cursor, replace them, optionally
  accept the line;
- no redraw hook for after a full-screen program has scribbled on the screen.

There is no partial workaround worth writing down. A `func h() { atuin search -i }`
prints a command you then have to retype; that is worse than mesh's own Ctrl-R.

**The strategic question this raises.** mesh already has
[its own SQLite history](REFERENCE.md#history-and-recall) with richer columns
than a history file — command, cwd, tty, session, start, duration, status. That
is most of atuin's schema. So "integrate atuin" is really two different asks:

- *Use atuin's UI over mesh's store* — needs the store to be queryable from mesh
  code (`$sh.history`, currently deferred) or a documented on-disk contract.
- *Use atuin as the store* — needs mesh's recall motions (Up, Ctrl-R, `!$`) to
  read from a **pluggable** history backend rather than its own table, which is
  a much deeper change and one nobody has asked for yet.

Worth deciding before either is built, because the answer changes whether
`--no-save-history` is the integration point or a workaround. Adjacent, and
already deferred: importing bash/zsh/atuin history, and secret redaction.

## fzf — the keyboard

fzf is class 5, which is to say: the parts that are just a command work, and the
parts that are a keybinding do not.

**Works today.** Anything where fzf's output is a command's *argument*:

```mesh
func fcd() {                                   # Alt-C, as a command
  if dir = $(fd --type d | fzf) { cd $dir }
}
func fv() {                                    # Ctrl-T, as a command
  if file = $(fzf) { vim $file }
}
```

Full-screen handoff works, so fzf draws and restores correctly. A canceled fzf
exits nonzero (130) having printed nothing, so the selection is **guarded on the
status**: [a capture hands back its output whatever the command
exited with](REFERENCE.md#command-substitution--), so an unguarded
`cd $(fd --type d | fzf)` would run `cd ""` on a cancel and report a path error
for a path the user never asked for. `if dir = $(…)` binds the selection and
branches on fzf's status in one line, which is the same shape bash's
`if dir=$(…); then` has.

`FZF_DEFAULT_OPTS` and friends are ordinary environment writes.

**What is missing.** The bindings — `Ctrl-T` to paste a file into the line you
are typing, `Ctrl-R` for history, `Alt-C` to cd — all need the same
keybinding-plus-buffer surface atuin needs, and `Ctrl-T` needs the buffer half
specifically: it *inserts at the cursor*, it does not run anything.

fzf's other integration is the `**<Tab>` trigger, which is a completion
provider: fzf wants to be handed the current word and to answer with a
selection. That is `$sh.complete` plus a way for a completer to run a
full-screen program mid-completion. And `fzf-tab`-style *menus* — fzf replacing
the completion menu itself, for every command — needs the menu to be swappable,
which is a line-editor question mesh has not opened.

## carapace — completion

carapace is the most interesting one, because mesh and carapace overlap: both
exist to avoid hand-written completion scripts. mesh guesses from man pages and
`--help`; carapace ships curated, structured specs for ~1000 commands. They are
complementary — carapace is right where it has a spec, mesh's heuristics cover
the long tail carapace does not.

**The shape that fits.** Not `carapace _carapace mesh` printing a script (mesh
has no `eval`, and the script would be a shell dialect mesh does not speak), but
carapace's **export** interface: given a command and the words typed so far, it
prints candidates with descriptions as JSON. That is a data lookup, and it slots
into mesh's existing
[four-layer resolver](REFERENCE.md#where-a-commands-completions-come-from) as a
new layer between the curated file and the man page — ranked there because it is
curated data rather than a guess, but below the user's own file, which must
always win.

**What is missing.**

- **`$sh.complete`** — the override map is designed and not built
  ([`TODO.md`](../TODO.md), "Expose static and dynamic completion overrides").
  For a bridge like this it needs more than the design currently says:
  - a **fallback key** (`*`, or a documented default entry), since a bridge
    answers for *every* command rather than one named one;
  - a defined **callable contract**: the words so far, the cursor's word index,
    the partial word, and the cwd — bash's `COMP_WORDS` / `COMP_CWORD` in mesh
    shapes;
  - **descriptions alongside candidates**, since carapace's value is the
    description and mesh's menu currently shows bare candidates.
- **Reading structured output.** JSON, or a simpler line format mesh defines and
  asks upstream for. mesh has no parser for either. Doing the bridge in Rust
  instead of in mesh code sidesteps this, at the cost of building carapace
  knowledge into the shell.
- **`$sh.options.complete.probe`** — a user who has carapace probably wants
  mesh's `--help` probe off. The option is specified but the settings map is
  flat and cannot hold a nested key, which is
  [already tracked](../TODO.md).
- **Caching.** Generated specs are cached by their source's mtime; a per-word
  dynamic provider has no such key, so it must be exempt rather than
  accidentally cached.

## zoxide and `z` — directory jumping

**Half works today.** The query half is a function:

```mesh
func z(...args) { cd $(zoxide query -- ...$args) }
func zi() { cd $(zoxide query -i) }        # the fzf picker; full-screen handoff is fine
```

A no-match exits nonzero, so the capture aborts the statement and `cd` does not
run — the right behavior, with zoxide's own message as the report.

**The recording half works today too**, since `postcd` landed:

```mesh
func track-dir(previous) { zoxide add $env.PWD }
on postcd zoxide track-dir
```

That is the whole integration. The hook fires around **each actual move**, a
`cd` inside a function included, so nothing is missed and nothing needs the
`$env.PWD` guard every bash and zsh config hand-rolls.

It matters that this is a real hook rather than a `preprompt` check, because the
zsh workaround is not available here: **a function cannot shadow a builtin**, so
wrapping `cd` — what every directory-tracking tool does under zsh — is refused
by design.

```text
mesh$ func cd(dir) { zoxide add $dir }
mesh: func: `cd` is a reserved name and cannot be a function name
```

The same hook is what direnv, autoenv, mise, and a background `git fetch` on
arrival all want; the rest of what those need is in
[the environment section](#direnv-mise-nvm--the-environment).

## direnv, mise, nvm — the environment

All of class 6 have one shape: a hook fires, the tool computes a set of
environment changes, the shell applies them. bash and zsh apply them by
`eval`ing `export` statements. mesh should apply them from data.

**What each offers.** `direnv export json` prints the diff as JSON (a `null`
value meaning "unset this"). `mise env --json` does the same. `nvm` and `pyenv`
are shell functions rather than binaries and are the hardest of the set — pyenv
at least degrades to its `PATH` shims, which need no integration at all since
`$env.PATH` is already a real list.

**What is missing** — two things, both small:

- ~~A hook that fires at the right time.~~ **Landed:** `precd` runs before the
  move and `postcd` after it, around every actual `cd`. direnv hooks `PROMPT_COMMAND`
  under bash because bash has nothing better; here it can hook the move itself.
- ~~Writing `$env` under a computed key.~~ **Landed:** `$env[$name] = value` is
  the writing twin of the computed read, so a loop over a diff applies it
  directly (`for name, value in $changes { $env[$name] = $value }`).
  `unset $env[$name]` is the other half — the `null` in a direnv or mise diff
  means "unset this", and until now there was no way to remove an environment
  entry at all, only to empty one.
- **Reading the payload.** JSON again — the same gap carapace hits. This is the
  only thing still missing for direnv and mise.

A plausible end state is an `env-apply` that takes a map and applies it as one
transaction, so the tool-facing contract is "print a map" and the failure mode is
a diagnostic rather than a half-applied environment. The pieces it would be built
from all exist now; what it adds over the loop above is the transaction.

## Everything else, briefly

| Tool | Status |
| --- | --- |
| ripgrep, fd, bat, eza, delta, jq, difftastic | **Nothing to do.** Plain commands, and their flags complete from their own man page or `--help` |
| broot (`br`) | **Works today.** It writes a command to a file for the shell to run, and `source` takes a file: `func br(...args) { f = $(mktemp); broot --outcmd $f ...$args; source $f }` |
| tmux, shpool | Session management is [on the roadmap](../ROADMAP.md); no external integration needed beyond it |
| iTerm2 / VS Code / WezTerm shell integration | **Already shipped** — `OSC 133`, `OSC 633`, `OSC 7`, titles, and hyperlinks |
| thefuck, mcfly | Class 5 — same keybinding and buffer gap as atuin |
| zsh-autosuggestions, fast-syntax-highlighting | Not external tools, but the experience users expect. reedline has both a hint provider and a highlighter hook; mesh exposes neither |
| oh-my-zsh, prezto, plugin frameworks | **Non-goal.** No alias mechanism (`alias` defines a `wrapper func`), no plugin loader, no completion scripts — that is the premise, not a gap |
| keychain, ssh-agent | Class 6, environment diff |
| asdf, fnm, volta, jenv | Class 6, environment diff |

## What is missing

Every gap above, deduplicated, roughly by how many tools it unblocks. Each has a
matching entry under "Beyond M3 — External tool integration" in
[`TODO.md`](../TODO.md).

| # | Missing | Unblocks | Status |
| --- | --- | --- | --- |
| ~~1~~ | ~~**`precd` / `postcd` hooks**~~ | zoxide, direnv, mise, autoenv, background fetch | **Landed.** Around every actual `cd`, target resolved before `precd`, a handler's own `cd` does not re-dispatch |
| 2 | **Keybindings from `rc.mesh`** | atuin, fzf, thefuck, any widget | Deferred in `DESIGN.md` (§Line editing) |
| 3 | **A line-buffer API for widgets** | Same, and required *with* (2) — a binding that cannot touch the buffer is useless to fzf | Not designed. Needs: read buffer and cursor, replace, insert at cursor, accept-line, redraw after a full-screen program |
| 4 | **`$sh.complete`, extended** | carapace, fzf-tab, dynamic completers | Map is a known TODO; the fallback key, the callable contract, and candidate descriptions are not specified |
| 5 | **Reading structured output (JSON)** | carapace, direnv, mise, atuin | Nothing exists. The alternative is a mesh-defined line format plus upstream asks |
| 6 | **`$env` writes under a computed key**, and a bulk env-diff apply | direnv, mise, keychain, asdf | Explicitly ruled out today; the narrowest gap here |
| 7 | **`$sh.prompt` segment map**, multi-line external output, `fill` | starship as *a* segment rather than *the* prompt; right prompts | Designed in [`PROMPT.md`](PROMPT.md), unbuilt |
| 8 | **A decision on generated code** — `source -` / `run`, versus data-only | Every tool's published install line | See [The bootstrap problem](#the-bootstrap-problem). Recommendation: data-first, no string `eval` |
| 9 | **`$sh.options.complete.probe`** | Turning mesh's own probe off when carapace is authoritative | Blocked on nested keys in the flat settings map |
| 10 | **History: `$sh.history`, import, redaction, pluggable backend** | atuin, mcfly | Deferred in `DESIGN.md`; the pluggable-backend question is undecided |
| 11 | **Hint and highlighter hooks** | The zsh-autosuggestions experience | reedline supports both; mesh exposes neither |
| 12 | **A published integration contract**, so upstreams can add a `mesh` target | All of them | Nothing written. Ordering matters: define what `tool init mesh` may emit *before* asking anyone to emit it |
| 13 | **A name may not start with `_`** | Every hook-based integration, each of which needs a private global | A rule rather than a bug, and already tracked as "Reserve only bare `_` as discard, allow `_name`" — but the convention is universal and the diagnostic is a "command not found" |

## Upstream, and the order to do it in

Every tool here keys on a shell name: `atuin init <shell>`,
`starship init <shell>`, `zoxide init <shell>`, `direnv hook <shell>`,
`carapace _carapace <shell>`. Adding `mesh` to those lists is a small patch each
— and it is the *last* step, not the first. A `tool init mesh` that emits
keybinding registrations mesh cannot parse is worse than no target at all,
because it looks supported and is not.

The order that works:

1. Build the hooks (1 — **done**) and the buffer/keybinding surface (2, 3) — the
   tools that need them cannot be integrated any other way.
2. Decide (8), then specify (12): what an `init mesh` output may contain, and
   which integrations are data rather than code.
3. Ship the data path (5, 6) so direnv, mise, and carapace need no upstream
   change at all — they already print what mesh would read.
4. Then send the upstream patches, against a documented API.

Steps 1 and 3 between them cover starship, zoxide, direnv, mise, carapace, and
atuin's recording half without a single upstream change. That is most of the
toolbox, and it is the argument for doing the shell-side work first.
