# The hook registry: what exists, and what to decide

`DESIGN.md` §"Hooks and the prompt" specifies hooks as **insertion-ordered maps
of named callables** — `$sh.preprompt.git = …`. What was built first was a
builtin, `on EVENT NAME FUNCTION`, over a flat list. The two are meant to be the
same registry seen from two angles, and nothing had yet had to reconcile them.

This document works out what reconciling them costs. It was written after the
`exit` hook was extended to every exit path, and after the question of how
signal handlers should be spelled came up — both of which push on the same seam.
D4, the reconciliation itself, has since been taken.

No decision below gates another; a couple are merely cheaper taken early, and
[Suggested order](#suggested-order) says which and why.

**D4 has since been decided and built** — the map surface is a view over one
store; see that section for what shipped. The other five are still open, and each
of those sections ends with a lean rather than a ruling. The checkable list this
produced lives in [`TODO.md`](../TODO.md) under "Beyond M3 — The environment" and
"Beyond M3 — External tool integration".

As in [`INTEGRATION.md`](INTEGRATION.md), every "today" claim below was run
against a built `mesh`, and every error message is the shell's own rather than a
reading of the source.

---

## What exists today

Two surfaces over one store — the `on` builtin, and the `$sh.<event>` map D4
added.

```mesh
func arrived(from) { puts "now in $(pwd)" }
on postcd trace arrived        # EVENT NAME FUNCTION
on --remove postcd trace

$sh.postcd.trace = arrived     # the same write
unset $sh.postcd.trace         # the same removal
```

Seven events, a closed set — `preprompt`, `preexec`, `postexec`, `precd`,
`postcd`, `jobdone`, `exit`. Storage is a flat `Vec<Hook>`, where
`Hook { event, name, function: String }` lives in `Hooks` on `Vars`; the map a
read sees is rebuilt from it per access.

Five behaviors are already right, and are the ones worth not losing:

| Behavior | Evidence |
| --- | --- |
| The `(event, name)` pair is the key; re-registering replaces in place | two `on exit k …` registrations run only the second |
| Handlers run in registration order | `on exit one a` / `on exit two b` prints `A` then `B` |
| One handler is removable by name | `on --remove exit k` |
| A handler is resolved **at dispatch**, by name | redefining the function after registering runs the new body |
| One handler's failure does not stop the rest | an arity error in the first still runs the second |

The fourth deserves emphasis, because it is easy to state backwards: dispatch is
already late-bound. It is only the *registration-time existence check* that is
eager.

## What `DESIGN.md` promises that is not built

```mesh
$sh.preprompt.git = …                       # the map surface — built (D4)
$sh.postcd.fetch = func() { vcs auto-fetch & }   # a callable as the value — not built
$sh.signal.INT.note = func() { … }          # signals, nested under `signal` — not built
unset $sh.preprompt.jobs                    # removal by unset — built (D4)
```

Two gaps left, each verified:

- **No callable values.** `on exit e func(s) { … }` → `mesh: a function value has
  no text form`, and `$sh.exit.e = func() { }` → ``mesh: $sh.exit.e: a handler is
  a function's name; a callable value is not stored yet``. The stored handler is
  a `String`.
- **No commands as handlers.** `on exit e echo` → ``mesh: on: `echo` is not a
  function``, though `DESIGN.md` says a handler is a reference to "a command or
  function."

**Forward references are *not* on that list**, and an earlier revision wrongly
put them there. `on exit e bye` before `func bye` is refused —
``mesh: on: `bye` is not a function`` — but `DESIGN.md` calls a handler reference
"**resolved late**", which is a claim about *dispatch*, and mesh's dispatch
already is (see the table above). Eager registration-time validation and
late-bound running coexist perfectly well, and the design document does not ask
for the first to be dropped. Whether to accept a forward reference is a live
ergonomic question — it is D3 below — but it is mesh's to decide, not a promise
being broken. The `&name` section says so directly: it fixes when a reference
*resolves*, not when it is *checked*, and names D3 as the open half.

**A newer gap, from the `&name` decision.** `DESIGN.md` now spells a handler
reference `&handler` rather than a bare `handler`, and makes a bare word in a
hook slot an ordinary string. Nothing here is built either way, so this document's
questions are unchanged in substance — **D1's option (b) is written in the new
spelling below**, rather than translated in passing here — and the eager-check
question (D3) is untouched, since it is about *when* a reference is validated, not
how it is spelled. See `TODO.md` §"Beyond M3
— Function references (`&name`) and lambda capture".

---

## D1 — What is a handler's *value*?

Today a `String`. `DESIGN.md` wants an `&name` reference or a callable.

**(a) Keep the string.** Cheapest. The cost is that `$sh.postcd.fetch = func()
{ … }` — the map form exactly as `DESIGN.md` writes it — has nowhere to go, so
the map ships accepting names only. That is a limit on what a hook *value* may
be, and nothing more: it leaves D4 entirely open, since a string-only map can
still be a view over the one hook list.

**(b) A callable value type**, holding an `&name` reference — resolved at
dispatch — or a lambda. A bare word is then an ordinary **string**, not a
callable, which is `DESIGN.md`'s spelling; an earlier revision of this option
read "with a bareword still meaning resolve-this-name-at-dispatch", and that
spelling is retired, so `$sh.exit.k = f` becomes `$sh.exit.k = &f`. Costs a
value-type change, and buys the map form. It raises no
question about how a stored lambda *prints*: `docs/REFERENCE.md` already settles
that a function value has **no text form** — "the one value that cannot be
bytes" — so a lambda in a hook map simply keeps that behavior, and the existing
output path already refuses it.

What it does **not** buy is a *command* as a handler, and an earlier revision
claimed it did. That gap is not in the value type at all — it sits in two other
places, which is why "commands as handlers" is really its own question:

- **Registration** rejects it. `register_hook` refuses a name absent from
  `shell.funcs`, which is exactly D3's eager check; dropping that is what would
  let `on exit e echo` register at all.
- **Dispatch reaches only half the command namespace, and hands it nothing.**
  `call_func` falls back to `exec::run` for a name it does not know, which is
  the *external-program* runner — it builds a one-stage pipeline marked
  `in_shell: false`, whose in-shell branch is `unreachable!("an external command
  is never an in-shell stage")`. Builtin and shell-state resolution lives in
  `run_expanded` instead, so `on exit e echo` could work while
  `on exit e puts` could not, and `DESIGN.md`'s "a command name" covers both.
  Separately, that fallback runs the name *alone*, so a command handler would
  receive none of the event's arguments — `on postcd trace mycommand` would not
  be told the previous directory.

  So commands-as-handlers needs a dispatch change after all: one that shares the
  normal dispatcher rather than bypassing it, and that passes the event's
  arguments. *(Read from the source rather than run, since registration blocks
  the case before it can be tried.)*

**Lean: (b), and preferably before D4** — but as a preference, not a constraint,
and the distinction was got wrong here first. Two changes ride together under
(b), and they differ in compatibility:

- **Widening the value type is additive.** `Value::Map` is
  `Vec<(String, Value)>` and already holds mixed types (a map can carry an
  integer, a string, and a list at once), and `Value::Function` already exists,
  so accepting a callable where only a `String` was accepted breaks nothing by
  itself.
- **Adopting `&name`'s reading is breaking.** Once a bare word in a slot is an
  ordinary string rather than a name to resolve, every `$sh.preprompt.x =
  handler` already written becomes `= &handler`, and a stored plain string stops
  being dispatched at all. `TODO.md` pairs that with a requirement that a handler
  slot given a plain string *say so* and name the `&` fix, rather than silently
  doing nothing.

An earlier revision of this paragraph said an existing string entry "keeps its
read, write, and late-binding behavior" and that "nothing a user wrote stops
working." That is true of the first bullet alone and false of the pair, which is
the whole reason the second one needs its own migration note.

What doing D4 first actually costs is narrower, and worth naming honestly:

- `DESIGN.md` documents the map surface *with lambdas* —
  `$sh.postcd.fetch = func() { vcs auto-fetch & }` is its own example. A
  string-only map rejects the exact line the design document shows, so the
  surface ships not matching its spec.
- The map's read/write plumbing gets written against `String` and then revisited.

Both are real; neither blocks independent map work.

**That example needs D6 as well**, which is worth following through because it
is the design document's own line. `postcd` supplies the previous directory, and
today's binder is exact, so a zero-parameter `func() { … }` is rejected *at
dispatch* — `mesh: …: expected 0 argument(s), got 1`. D1 and D4 together would
therefore accept the assignment and then fail every time the hook fired. Making
that one line work end to end takes callable values, the map surface, **and**
either prefix binding (D6) or a signature that accepts the argument
(`func(from) { … }`, or a rest). A useful reminder that the decisions are
independent as *decisions* while a given user-visible example can still need
several of them at once.

## D2 — Is the name mandatory?

Today, yes: three operands, always. So the common case reads
`on exit bye bye` — the identity and the function are usually the same word.

**(a) Keep it mandatory.** Consistent, and the repetition is mild.

**(b) Derive the name from a bareword handler when it is omitted.** Two operands
means "event and callable, key it by the callable's own name"; three keeps
today's explicit form. The operand count disambiguates, so both spellings can
coexist:

```mesh
on exit bye              # key is `bye`
on exit tmp clean-up     # key is `tmp` — an explicit identity, still available
```

A lambda has no name to derive, so it would still need the explicit form.

**(c) Allow anonymous handlers.** Rejected outright, and worth writing down why:
an unnamed handler cannot be replaced or removed, so re-sourcing an rc file
stacks duplicates. That is precisely the bash `PROMPT_COMMAND` bug the keying
exists to prevent, and `DESIGN.md` calls re-source safety the reason for the
design.

**Deferred — (b) is not being taken.** It reads well, but two things came up
that the stutter does not outweigh:

- **It ties the hook's identity to the function's *name*, so renaming stacks
  rather than replaces.** `on exit bye` keys on `bye`; rename the function to
  `farewell`, re-source, and you have *two* hooks. Both **run** — `Funcs::define`
  replaces the names a file defines but never removes the ones it stops
  defining, so `bye` is still callable in that session and the old handler fires
  alongside the new one:

  ```text
  func bye(s) { puts BYE } ; on exit bye bye
  func farewell(s) { puts FAREWELL } ; on exit farewell farewell
  → BYE
    FAREWELL
  ```

  Keying exists to make re-sourcing safe, and this makes it safe only while
  nothing is renamed: the `PROMPT_COMMAND` stacking bug returning by a side
  door. (A *fresh* shell has neither the stale definition nor the stale hook, so
  the symptom is a long-lived session's, which is the one an interactive shell
  has.)
- **The sugar is backwards.** A lambda has no name to derive, so `on exit bye`
  works while `on exit func(s) { … }` still needs an explicit key. The terse form
  is available for the pre-defined function and absent for inline logic, which is
  the more interactive case and the reason to want it.

It also does not survive into the map: `$sh.exit.<key> = …` must always name a
key, so the stutter returns there as `$sh.exit.bye = bye`. That makes it a
builtin-only affordance, which is a poor trade for a surface meant to be one
registry seen two ways.

## D3 — Should registration require the function to exist?

Today it must. That rejects the forward reference — registering in one file the
handlers defined in another, or below — while, as noted above, *dispatch* is
late-bound already, so the check buys less than it looks like it does.

**(a) Keep the eager check.** A typo is caught at the line that made it, which
is a real ergonomic win for an interactive shell: the alternative reports
`nosuch` at exit, when the session is leaving and nobody is reading.

**(b) Drop it.** Allows forward references, and lets a command name *register* —
but only that. Per D1 above, a registered command still would not reach a
builtin and would be handed none of the event's arguments, so working command
handlers wait on the dispatch change regardless of what happens here. Costs the
early typo report.

This option has **no backing in `DESIGN.md`**, and saying it "matches the
late-bound wording" was an earlier over-reading: that wording is about when a
handler is *run*, which mesh already does late. So (a) and (b) are both
compliant, and the choice between them is mesh's own ergonomic call rather than
a spec being satisfied.

**(c) Warn, register anyway.** Both, at the cost of a diagnostic that is
sometimes noise — a config that legitimately registers before defining would
warn on every startup.

**Lean: (b)**, and independently of everything else — an earlier revision said
"once D1 lands", which was wrong for the same reason the D1-gates-D4 claim was.
Today's `String` can already hold a name resolved late, so dropping the check
enables forward references on its own; D1 changes what a handler may *be*, not
whether an unresolved name may be stored.

What **command-handler support** would change is narrower still, and it is the
*predicate* rather than the check: once a handler may hold a reference to a
command — `&echo`, `&puts` — absence from `shell.funcs` stops being evidence of a
typo. That argues for replacing the predicate — validate against the whole
callable namespace, functions and builtins and `PATH` — not for dropping eager
validation, so (a) survives command handlers perfectly well by widening what it
looks in.

**D1's option (b) is what raises that question**, which is a correction to an
earlier revision of this paragraph. It read "D1 alone does not even raise the
question: a bareword stays a string, commands stay unusable" — true of the
retired bareword-is-a-callable reading, and false of `&name`, which resolves over
the command namespace (`builtin → func → external`, per `DESIGN.md`). A callable
value type is therefore precisely the form that can carry a command reference.
Under **(a)** the point still holds: a name is a string, commands stay unusable,
and an unknown name really is an invalid registration.

So the honest statement of (a) versus (b) is about **forward references**, and
nothing else: (a) cannot accept a handler defined later, whatever it validates
against, because the name genuinely does not exist yet. Worth pairing (b) with a
`--check` or a lint rather than losing the typo report entirely.

## D4 — How does the map surface relate to `on`?

**(a) One store, `on` is sugar.** `$sh.exit` is a view; `on exit k f` and
`$sh.exit.k = f` are the same write, and `unset $sh.exit.k` is
`on --remove exit k`. mesh maps are insertion-ordered, so a map keyed by name
represents today's `Vec` exactly.

**(b) Two stores.** Never deliberately chosen, but it is what happens if the map
is added beside the list rather than over it. The failure mode is drift: a hook
registered one way and removed the other.

**Decided: (a), and built.** `$sh.<event>` is a view over the one store; `on
EVENT NAME FUNCTION` and `$sh.<event>.NAME = FUNCTION` are the same write, and
`unset $sh.<event>.NAME` is `on --remove EVENT NAME`. Documented under
[Custom prompts and hooks](REFERENCE.md#custom-prompts-and-hooks); the anti-drift
property is tested in **both** directions, since drift is the failure mode.

**What (a) actually required: one *authority*, not one location.** Hooks lived on
`Shell::prompt.hooks` in `repl.rs` while `$sh` is built in `vars.rs`, so
`shell_namespace()` could not see them — but that was a plumbing problem, not an
ownership one, and an earlier revision of this section wrongly concluded the
store *had* to move onto `Vars`.

A snapshot is not a second store. `shell_namespace()` already rebuilds an
ephemeral map on every read — `$sh.options` included, whose authority is the
`Options` on `Vars` — so a `$sh.<event>` map rebuilt from the hook list on each
read, with writes and removals mutating that same vector, is (a) wherever the
vector lives. The namespace builder just has to be *given* the hooks.

Giving it them is what the store move settled, and the choice was between two
plumbings rather than between one store and two. `$sh` is resolved in
`expand::resolve_value`, deep in expansion, holding only `&Vars`: reaching the
hooks from there meant either moving them onto `Vars` — a field, two accessors,
and seven call sites in `repl.rs` — or threading a second parameter through the
~10 `vars: &Vars` signatures in `expand.rs` and their callers. The move is the
smaller change, and it puts the hooks beside `Options`, which is already there
for the same reason.

What would have made it (b) is a snapshot that became **authoritative** — written
to directly, or rebuilt only sometimes — so that two copies could disagree. That
is the thing designed out: `Hooks` is the only mutable state, and the map is
built from it per read.

The rest is the shape `$sh.options` already had, and the refusals matter as much
as the writes:

| | `$sh.options` | `$sh.<event>` |
| --- | --- | --- |
| whole-map assign | refused: *"assign one setting at a time"* | refused: *"assign one handler at a time"* |
| one key | assigns that setting | registers, replacing in place |
| deeper | refused | refused — `$sh.exit.k.member` reaches *inside a handler* |
| `+=` | refused | refused — a name is replaced, not combined |
| removal | refused: the key set is fixed | `unset`, the `on --remove` spelling |

Refusing `$sh.exit = [ … ]` is the important one: a map literal that omits a key
would have to mean either "leave it" or "remove it", and a config that guessed
wrong would silently drop every other handler for that event — the composition
property the keying exists to protect.

Two smaller things fell out of building it. A map is present for **every** event
from the start, so `$sh.exit.k = f` has somewhere to land and `$sh.preprompt:len`
answers `0` rather than failing before anything is registered. And the map path
makes the **same** validity check the builtin does — a name absent from `Funcs`
is refused either way — because a handler one surface would reject must not be
admissible through the other, or "one registry" is true of the storage and false
of what may enter it.

The deeper refusal is for a reason of its own, and not D5's: `$sh.exit.k.member`
is reaching *inside a handler value*, which has no members to reach. D5's nested
shape is the different path `$sh.signal.<NAME>.<key>`, and the two do not
constrain each other.

**Was not blocked by D1**, and shipping it first bore that out. Handlers are
`String`s, so a map view over them prints fine and `$sh.exit:repr` writes back as
source (`['e': 'bye']`). Both stop working once values are callables: `puts`
refuses a function, and `:repr` refuses it too and *should* — a lambda has no
literal form, and `:repr`'s guarantee is that what it returns reads back.

What survives either way is `$sh.<event>:keys`, which never touches a value, so
the **identities** stay listable and it is the handlers that go dark. Rendering
those needs a **new callable-value renderer** — `type` does not already compute
it, and saying otherwise here was wrong: `whence::signature` runs for a *named*
entry in `Funcs`, while a `Value::Function` out of the map falls through to the
bare `a function`. [`TODO.md`](../TODO.md) tracks that work. Not this decision's
problem either way, and not function-specific: a map holding a job handle is
already unprintable the same way.

## D5 — How are signals spelled?

The crux is a shape difference that is easy to miss. Events are **top-level**
(`$sh.preprompt`), but `DESIGN.md` nests signals one level deeper
(`$sh.signal.INT`). So a flat `on int` would not *mirror* the map form.

That is a cost, not a disqualifier, and saying otherwise is what an earlier
revision of this section got wrong: **the two surfaces can address one store by
different paths.** Nothing requires `on`'s first operand to be the map path.
What non-mirroring costs is derivability — someone who learns `on int` does not
thereby know where it lives in `$sh`.

**A nested map path is not blocked, though it looks like it might be.** An
earlier revision claimed every nesting-preserving option waited on the
`$sh.options.complete.probe` question. It does not. That path is refused *inside
the `options` branch*, which accepts exactly one boolean-setting key and says so
in its own terms — not by the parser or the `$sh` namespace forbidding depth:

```text
$sh.options.complete.probe = false   → a setting is a boolean, with nothing inside it
$sh.signal.INT.note = x              → $sh: no `signal` in this map
```

The second failure is a *missing entry*, not a rejected depth. So a
`$sh.signal.<NAME>.<key>` branch can implement `DESIGN.md`'s nesting on its own
terms, and whether **settings** eventually spell their extra level as a submap
or a dotted key is a separate decision that neither waits on the other.

**(a) Flatten both** — `on int k f` ↔ `$sh.int.k`. Simple, but abandons
`DESIGN.md`'s nesting and puts lifecycle events and signals in one flat set,
where `on int` and `on precd` look alike but are not.

**(b) Flat builtin, nested map** — `on int k f` ↔ `$sh.signal.INT.k`. **This is
the shape [`TODO.md`](../TODO.md) already plans** ("`on int NAME FUNC` should
work too… both, over one store"). Terse where terseness is used — at a prompt —
and it keeps `DESIGN.md`'s nesting untouched. The cost is the derivability
above, plus an inconsistency between the two: `on exit` sits at `$sh.exit`,
while `on int` sits at `$sh.signal.INT`, so the operand *sometimes* equals the
map path.

**(c) A dotted event path** — `on signal.INT k f` ↔ `$sh.signal.INT.k = f`.
Mirrors the map exactly, keeps three operands, and generalizes if another nested
family appears. Costs a small new syntax in operand position and more typing
than (b).

**(d) A four-operand form** — `on signal INT k f`. Reads well, but it is a
second operand shape for one builtin, and `on` already carries `--remove`.

**(e) Signals only via the map**, leaving `on` for events. Fewest decisions, but
gives up the terse interactive spelling that prompted the question.

**Lean: (b) or (c), and the choice between them is a real one.** (b) is already
the plan of record and is what a hand at a prompt would rather type; (c) buys
exact correspondence between the surfaces. Picking (c) would supersede the
`TODO.md` entry, which is worth doing deliberately rather than by drift.

One argument does separate them, beyond taste. Whatever is chosen, **`on exit`
must keep meaning the lifecycle event** — bash's `trap` conflates the EXIT
pseudo-signal with real signals, and `DESIGN.md` deliberately does not,
describing `$sh.exit` as belonging with the hooks. Under (b) the flat operand
set holds `exit` next to `int`, `term`, and `hup`, so a reader may well take
`on exit` for the EXIT signal; under (c) the two kinds are visibly different
(`on exit` against `on signal.INT`), which prevents the confusion structurally
rather than by documentation.

## D6 — Arity, and whether an event can ever gain an argument

For a **fixed-arity** handler, arity is exact in both directions:

```text
func a()      → mesh: a: expected 0 argument(s), got 1
func a(x, y)  → mesh: a: expected 2 argument(s), got 1
```

**Two signature shapes already tolerate a surplus**, though, and that qualifies
the whole section. `bind_scanned` skips the surplus-argument error when a rest
parameter is present, and accepts any count from the required minimum through
the optional maximum — so an unused **optional trailing positional** absorbs an
added argument just as `...rest` does. All of these work today as `exit`
handlers:

```mesh
func a(...rest) { puts "got $rest:len" }               # rest absorbs it
func a(status, ...rest) { puts "left with $status" }   # named, and extensible
func a(status, reason = unknown) { puts "$status/$reason" }  # optional tail
```

The third is the one worth noticing: an event that later gains an argument would
*bind* it to `reason` rather than break the handler, so a signature written with
a default is forward-compatible without any `...` ceremony.

So the consequence is real but narrower than "every handler": **adding an
argument to an existing event breaks a handler whose positionals are all
required and which declares no rest.** If `postcd` ever passed the reason for a
move alongside the previous directory, `func arrived(from)` would start
erroring — at dispatch, on somebody's prompt — while both
`func arrived(from, ...rest)` and `func arrived(from, why = none)` would not.
Since the natural way to write a handler is to name exactly the parameters you
were told about, with no defaults, the exposure is still most configs rather
than all of them.

**(a) Keep exact arity.** Every handler documents its event's full signature,
and the two shapes above — a rest parameter, or an optional trailing
positional — are the escape hatches for anyone who wants forward compatibility.
It costs writing `...rest` or a default on handlers that never use either.

**(b) Prefix binding** — a handler may declare *fewer* parameters than the event
supplies, and the extras are dropped; declaring more stays an error. Then
`func note() { … }` is a valid handler for any event, and an event can gain a
trailing argument without breaking anyone — no `...rest` or default required.

**Lean: (b)**, but it is an ergonomic argument rather than an urgent one, since
(a) is already survivable. It makes event signatures extensible by default
instead of on request, and the error it gives up ("you declared too few") is not
one that catches real mistakes.

---

## Suggested order

**No decision here blocks another.** An earlier draft claimed D1 had to precede
D4 or the callable case became a breaking change; that was wrong, for the reason
D1 now gives, and the correction is the useful part — none of these six is a
gate on the others, so they can be taken in whatever order suits.

**D4 was taken first**, against the order below, and nothing about D1 got harder
for it: widening what a handler value may be is additive to a shipped
string-only map, exactly as D1 says. (Additive is the *value-type* half. The
`&name` spelling that rides with it is the breaking half — see D1.)

Ordered by what is cheapest to do early rather than by dependency:

1. **D1** — callable values. Everything else is a little easier after it, and
   `DESIGN.md`'s documented examples need it to be true.
2. ~~**D4**~~ — done: the map surface is a view over the one store.
3. **D6** — prefix binding, before the event set grows enough to make a
   signature change painful. A handler whose positionals are all required, with
   no rest, is one that breaks if an event gains an argument later; a `...rest`
   or optional-tail handler is not, which is what keeps this a preference rather
   than a deadline.
4. **D2**, **D3** — surface ergonomics, independent of everything above.
5. **D5** — signals, which waits on plumbing tracked separately in `TODO.md`.
   No handler exists to hang them off yet, and the baseline is narrower than
   "interactive": `ignore_interactive_signals` is called only from
   `run_interactive`, the terminal-owning loop, where it ignores
   INT/QUIT/TSTP/TTOU/TERM outright. A session that is interactive by *flag* —
   `mesh -i script.mesh`, `mesh -i -c …`, piped `mesh -i` — runs through
   `run_batch` or `run_piped` and keeps every default disposition. HUP is
   handled nowhere.

None of the six is urgent. D6 comes closest, and only in the weak sense that
all-required-positional handlers accumulate in the wild while it is undecided —
a rest parameter and an optional trailing positional are both already working
escape hatches for anyone who wants one.
