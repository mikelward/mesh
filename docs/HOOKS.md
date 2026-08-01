# The hook registry: what exists, and what to decide

`DESIGN.md` §"Hooks and the prompt" specifies hooks as **insertion-ordered maps
of named callables** — `$sh.preprompt.git = …`. What is built is a builtin,
`on EVENT NAME FUNCTION`, over a flat list. The two are meant to be the same
registry seen from two angles, and nothing has yet had to reconcile them.

This document works out what reconciling them costs. It was written after the
`exit` hook was extended to every exit path, and after the question of how
signal handlers should be spelled came up — both of which push on the same seam.

No decision below gates another; a couple are merely cheaper taken early, and
[Suggested order](#suggested-order) says which and why.

Nothing here is decided. Each section ends with a lean, not a ruling. The
checkable list this produced lives in [`TODO.md`](../TODO.md) under
"Beyond M3 — The environment" and "Beyond M3 — External tool integration".

As in [`INTEGRATION.md`](INTEGRATION.md), every "today" claim below was run
against a built `mesh`, and every error message is the shell's own rather than a
reading of the source.

---

## What exists today

One surface: the `on` builtin.

```mesh
func arrived(from) { puts "now in $(pwd)" }
on postcd trace arrived        # EVENT NAME FUNCTION
on --remove postcd trace
```

Seven events, a closed set — `preprompt`, `preexec`, `postexec`, `precd`,
`postcd`, `jobdone`, `exit`. Storage is a flat `Vec<Hook>`, where
`Hook { event, name, function: String }` lives on `PromptConfig`.

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
$sh.preprompt.git = …                       # the map surface — no $sh.preprompt exists
$sh.postcd.fetch = func() { vcs auto-fetch & }   # a callable as the value
$sh.signal.INT.note = func() { … }          # signals, nested under `signal`
unset $sh.preprompt.jobs                    # removal by unset
```

Three gaps, each verified:

- **No map surface.** `puts $sh.preprompt` → ``mesh: $sh: no `preprompt` in this map``.
- **No callable values.** `on exit e func(s) { … }` → `mesh: a function value has
  no text form`. The stored handler is a `String`.
- **No commands as handlers.** `on exit e echo` → ``mesh: on: `echo` is not a
  function``, though `DESIGN.md` says "a command name or a callable".

**Forward references are *not* on that list**, and an earlier revision wrongly
put them there. `on exit e bye` before `func bye` is refused —
``mesh: on: `bye` is not a function`` — but `DESIGN.md` says a bareword names a
command/function "**run** late-bound", which is a claim about *dispatch*, and
mesh's dispatch already is (see the table above). Eager registration-time
validation and late-bound running coexist perfectly well, and the design
document does not ask for the first to be dropped. Whether to accept a
forward reference is a live ergonomic question — it is D3 below — but it is
mesh's to decide, not a promise being broken.

---

## D1 — What is a handler's *value*?

Today a `String`. `DESIGN.md` wants "a command name or a callable".

**(a) Keep the string.** Cheapest. The cost is that `$sh.postcd.fetch = func()
{ … }` — the map form exactly as `DESIGN.md` writes it — has nowhere to go, so
the map ships accepting names only. That is a limit on what a hook *value* may
be, and nothing more: it leaves D4 entirely open, since a string-only map can
still be a view over the one hook list.

**(b) A callable value type**, with a bareword still meaning "resolve this name
at dispatch". Costs a value-type change, and buys the map form. It raises no
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
and the distinction was got wrong here first. Adding callables to a shipped
string-only map is **additive, not breaking**: `Value::Map` is
`Vec<(String, Value)>` and already holds mixed types (a map can carry an
integer, a string, and a list at once), `Value::Function` already exists, and an
existing string entry keeps its read, write, and late-binding behavior when the
accepted value set widens. Nothing a user wrote stops working.

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

**Lean: (b).** It removes the stutter without giving up identity.

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
*predicate* rather than the check: once a bareword may name a command, absence
from `shell.funcs` stops being evidence of a typo. That argues for replacing the
predicate — validate against the whole command namespace, functions and builtins
and `PATH` — not for dropping eager validation, so (a) survives command handlers
perfectly well by widening what it looks in. D1 alone does not even raise the
question: a bareword stays a string, commands stay unusable, and an unknown name
really is an invalid registration.

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

**Lean: (a), emphatically** — and the reason to write it down now is that (a) is
nearly free today and gets harder with every feature that reads
`shell.prompt.hooks` directly.

## D5 — How are signals spelled?

The crux is a shape difference that is easy to miss. Events are **top-level**
(`$sh.preprompt`), but `DESIGN.md` nests signals one level deeper
(`$sh.signal.INT`). So a flat `on int` would not *mirror* the map form.

That is a cost, not a disqualifier, and saying otherwise is what an earlier
revision of this section got wrong: **the two surfaces can address one store by
different paths.** Nothing requires `on`'s first operand to be the map path.
What non-mirroring costs is derivability — someone who learns `on int` does not
thereby know where it lives in `$sh`.

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

Ordered by what is cheapest to do early rather than by dependency:

1. **D1** — callable values. Everything else is a little easier after it, and
   `DESIGN.md`'s documented examples need it to be true.
2. **D4** — the map surface as a view over the one store. The *view-not-second-store*
   part is the bit that gets harder with each new direct reader of
   `shell.prompt.hooks`, and it is nearly free today.
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
