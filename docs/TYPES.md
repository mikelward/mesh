# The type system — options for a simpler end state

**Status: a proposal. Packages A, B and C are undecided; §5 is not — it has
been accepted and is being built.** This document exists because `match`, `==`
and the value types have absorbed a disproportionate share of the design
effort, and the entries settling them keep needing another entry to scope the
last one. It asks whether that is bad luck or structural, concludes structural,
and lays out three coherent end states.

**§5 (L3) went first and has landed.** `DESIGN.md` records the decision — "a
`proc` / `func` split — decided: no second keyword. A `func` may declare a
return type, and a `func` that declares none has no value channel" — and the
parser, `type`, `help` and the lambda path are built. §5 below is kept as the
argument of record rather than as an open question, and it is written in the
**shipped spellings**, which differ from the draft it was argued in:

| §5 argued | Shipped | Why |
|---|---|---|
| `string func` | **`str func`** | short forms throughout, since `int` is already the language's word |
| `any func` | **`any func`** | argued as `any`; shipped first as **`value func`** — `any` "reads as a claim about a type system this deliberately is not", and `value` is the design's own word for the channel — then reversed back to `any`, because `value` is *also* `return`'s channel word: `return value func() { … }` read the marker as the channel and produced an untyped lambda in silence. Confirmed by the repo owner: `any` is **kept**. `job`, `regex` and `func` took away the gap-filling for every previously unnameable kind the declared-type harvest observed, so its main use is now "a value channel, kind not the point" — `glob` and `stream` have since joined too, so what is left for `any` is a flag (pending §8), and the only honest answer for a polymorphic identity function |

The vocabulary is a **closed set** — `status`, `int`, `str`, `bool`, `list`,
`map`, `job`, `regex`, `glob`, `stream`, `func`, `any` — with `float` reserved in the declaration position only.
`job` was added on the repo owner's decision, which also settled the rule that
keeps the set closed: a kind joins **on use at a function boundary**, not on
existing in the runtime. `any` is kept, for a value channel whose kind is not
the point. Where
this document still weighs something about §5, it is weighing something that
is **not** part of the accepted decision or not yet built.

**What landed is the *result* half only.** A `func` may declare a return type;
one that declares none has no value channel. The parser reads it, `type` and
`help` report it, and `Callable::Lambda` carries it.

**The narrowing is now enforced.** `run_call_body_for_value` maps a call's
outcome against the declared type on every call path, so a typeless `func`
called for a value yields a `Status` instead of whatever its body produced
last. **Still not built**: *none* of the accepted checks are — the parse-time
ones, the run-time warning at `x = f()` against a typeless callee (the
narrowing binds a `Status` there silently, which is what that warning is for),
and the dispatch-time typeless-lambda-in-a-value-slot check. The
bare-`return` half of the parse-time check was dropped rather than deferred,
being unanswerable from the syntax alone.

**And the accepted check is narrower than this document argued.** `DESIGN.md`
draws the boundary explicitly and it is worth quoting, because §5 below reads
as though it were wider: mesh has no typed variables and no typed parameters,
so "what the declaration makes checkable is the **shape of a function's
exits**, not the types flowing through its body: `return $x` against a declared
`int` is unanswerable until `$x` has a type… Calling this 'type checking' would
promise the second thing while delivering the first. It is **channel
checking**." What is accepted — and **not all at parse time**, which the
table's own last column says:

| Checked | When | Needs |
|---|---|---|
| `return $v` in a func declaring no type | **parse** | the declaration and the body |
| a command tail in a func declaring **any type but `status`** | **parse** | the same. *(Phrased on the declared **word**. It used to read "whose declared type a `Status` does not satisfy"; under `DESIGN.md`'s `T | Status(n≠0)` widening a satisfaction test would fire for a **successful** command tail and let a **failing** one through, so the check has to be syntactic.)* |
| `return "hi"` against a declared `int` | **parse** | the same, and **only where the operand is a literal** |
| `x = f()` where `f` declares no type | **run** | the callee |
| a typeless lambda in a value-taking hook slot | **dispatch** | the slot's declared kind |

**A bare `return` was on that list and came off it** — `DESIGN.md` records it
in full beside its own copy of this table. The check would have rejected
`str func f() { x = hello; $x; return }`, where the result so far *is* a `str`
and the function is correct: a bare `return` carries the result so far, so
whether it satisfies the declared type depends on what the body produced
before it, which no syntactic check can see. It is off the list for sitting
outside the boundary, not for wanting a cleverer version. The command-tail
half stays, a command's result being exactly a `Status`.

So §5's "checking is by *type*, not by class" holds for a literal and is
**unanswerable otherwise** — an exact check on every `return` and every
implicit tail is a further proposal, not unbuilt accepted work, and it needs
typed variables to mean anything.

**What was considered and declined** is the other half of this section's
argument, and §5 marks it inline where it appears: `DESIGN.md` keeps **argument
grammar with the caller**, so `f arg` stays valid whatever a function declares,
and a return type says nothing about how it is called. That closes the result
half "and leaves the other half where it is, deliberately". Advantage 4 below,
and everything resting on it, is therefore a *proposal this document made that
the decision did not take*.

Compatibility is deliberately ignored throughout. The question is what the
right shape is, not how to get there.

## 1. The diagnosis

The edge cases are not produced by any one type. They are produced by mesh
answering **"is this value acceptable here?"** with **six different relations**,
each of which every type must answer separately.

| # | Relation | Where it lives | Rule |
|---|---|---|---|
| 1 | **operator equality** | `==` / `!=` | refuses across types; one declared cross-type pair (status/int); scoped to the top-level operand pair only |
| 2 | **total equality** | `Value::eq` | never refuses — backs `in`, `:has`, `:dedup`, list `-`, hashing, and `match` literal arms |
| 3 | **condition** | `if` / `while` / `and` / `or` / `not` | a bool, a status, or a command's exit status; everything else is an error |
| 4 | **presence** | the `if lhs = rhs` RHS | everything binds and passes except a nonzero status, which takes `else` and binds nothing |
| 5 | **status projection** | `return v`, command position | `false`→1, status→its code, **everything else**→0 |
| 6 | **order** | `<` `<=` `>` `>=` | int/int numeric, string/string lexical, otherwise a fall-through to text |

Relations 1 and 2 are the same question with two answers, and `DESIGN.md`
spends roughly 200 lines explaining why they must differ. Relations 3 and 4 are
the same question with two answers, and disagree on `false`, `""`, `[]` and `0`.
Relation 5 gives a status meaning to types that have none, and gives them the
affirmative one. Relation 6 silently answers `10.0 < 2.0` with `true`.

On top of that there are **fourteen** value types shipped —

```
String  Styled  Flag  FlagTerminator  Integer  Boolean  Status
List  Map  Regex  Glob  Stream  Job  Function
```

— plus three designed and unbuilt (`Float`, `Instant`, `Duration`). Seventeen
types × six relations is a 102-cell table that nobody is maintaining, and every
new type adds six cells and a paragraph.

`match` then layers its own vocabulary on top: a bare word is a **literal** at
the top level and a **binder** inside `[ ]` or after `--flag=`; a flag has four
arm spellings; `~` is a documented strict subset of the arm grammar, which is a
second table.

**Three levers follow directly.**

- **L1 — collapse the six relations to four:** equality, condition, order —
  the three every type has to answer for itself — plus a **status
  projection** that survives, but stops being one of them (see B3).
- **L2 — collapse the seventeen types to ten classes.**
- **L3 — move the words-in/values-in choice from the call site to the
  declaration**, so it is a property of the callee rather than something each
  call site re-decides. (Not a *parse-time* fact — mesh resolves command names
  at call time on purpose, and §5 works through what that does and does not
  cost.)

## 2. Package A — repair in place

Keep every type and every relation. Fix only what is outright wrong.

- Add `Float`; make the cross-class ordering fall-through an error instead of a
  lexicographic guess.
- Build the decided-but-unbuilt pieces (flag patterns, `:name` / `:value`,
  status/int arms).
- Leave relations 1–5 as they are.

*For:* no rewriting; the build track keeps moving.
*Against:* nothing gets simpler. The rate at which review finds a new corner
stays exactly where it is, and each new type still costs six answers and a
section. This is the do-nothing option, listed so the others are measured
against something.

## 3. Package B — one relation each (recommended)

Six changes. They are separable, but they reinforce each other, and the
argument is that the set is what buys the simplicity rather than any one of
them.

### B1. One equality

**Delete the `==` refusal.** `==` *is* the total equality: structural, never
errors, and `false` across classes.

This is not a lowering of standards, because the case worth catching is a
**static** one. `1 == "1"` is wrong at the moment it is written, and both
operand types are visible in the source, so a resolver pass **reports** it
with no runtime rule at all. `$a == $b` on two unknowns is not catchable by
any rule, today's included — today it merely fails later and louder.

**The resolver reports; it does not refuse**, and the same policy covers every
construct that asks whether two values are equal — `==` / `!=`, `in`, `:has`,
list `-`, and a `match` literal arm — on the same terms: *this comparison
cannot be true, because no operand pair shares a class*. Both halves of that
matter.

*One policy*, because a check that fires on `1 == "1"` and stays silent on
`1 in ["1"]` would rebuild the operator-versus-total-equality seam this
section deletes, one layer up. The seam is what makes the current design
expensive; moving it from runtime to the resolver would not be deleting it.
That is why list `-` is in the list: `[1] - ["1"]` is a provable no-op, and
leaving it out would be the same seam under a third name.

The line is *asks a question*, not *calls `Value::eq`*. §1 names two more
consumers — `:dedup` and hashing — and neither has a pair of written operands
to compare: a heterogeneous list is the ordinary input to both, not a mistake,
so there is nothing a diagnostic could truthfully say. The policy is exhaustive
over the constructs it can speak about rather than over the callers of one
function.

*Reporting rather than refusing*, because refusal cannot be applied uniformly.
A `match` whose arms span classes is not a mistake — it is **the** argv-dispatch
idiom, and B5 cites heterogeneous dispatch as the reason equality has to be
total in the first place. So a hard error would have to exempt `match`, which
is another seam. A diagnostic needs no exemption: a never-matching arm is worth
saying out loud and is not worth refusing to run.

The claim "equality never errors" is therefore literal and holds everywhere,
in every construct, at compile time and at run time alike.

What this deletes outright:

- §"Comparison across types" — the whole "the refusal lives in the operator,
  not in equality" argument, and the four-example block showing what stays
  total beneath it.
- The `Flag` equality carve-out, and both `TODO.md` entries scoping it.
- §"Matching"'s "a literal arm compares totally, even where `==` refuses" — the
  seam vanishes rather than being explained.
- The `in` / `:has` inconsistency, where a quiet `false` is the same confusion
  `==` refuses, one operator over.

**The equivalence-class rule survives and does the real work.** It is the best
thing in the current design and it is why the refusal is not needed: a type
joins a class only through a *lossless* projection, so equality can be total
without ever making two distinct values equal by accident.

### B2. Ten classes

| Class | Members | Equal when | Notes |
|---|---|---|---|
| **text** | string, styled | the text matches | style is **presentation, not identity** — `red("x") == "x"`, and `red("x") == blue("x")` |
| **number** | int, float, status | the number matches | `status(0) == 0`, `0 in $pipestatus`, `$s < 2` all just work; `:code` becomes optional sugar |
| **bool** | bool | trivially | |
| **list** | list | element-wise | |
| **map** | map | key/value-wise | keys are text, as today |
| **pattern** | glob, regex | same **dialect**, source and flags | one type, two syntaxes — see the dialect note below |
| **instant** | instant | same point in time | as designed |
| **duration** | duration | same length | as designed |
| **flag** | flag, flag terminator | same name and same text payload; two terminators are equal | **kept, stripped** — see below; `--force` is never equal to `"--force"`, which is the whole point of the class, and `--` is the nameless member |
| **handle** | stream, job, func | **identity** | no byte form, no literal form, never crosses a process boundary |

Five merges (text, number, pattern, handle, flag) get from seventeen to ten.

**A class is a set of mutually comparable types**, which is why `instant` and
`duration` are two rows rather than one `time` row. Grouping them would say
they compare, and they do not — there is no answer to "is this instant equal to
that duration" beyond `false`, and no answer at all to which is smaller. The
invariant is what makes the ordered subset in B5 fall out instead of needing
its own partition.

**Styled folds into text.** Today it is a separate variant papered over by
`type_phrase` returning `"a string"` for both, which is a hack that exists
precisely because the classification is right and the type list is wrong. The
one thing to state out loud: `red("x") == blue("x")` is `true`, because style
is display metadata rather than data. That is already what "a styled value has
to behave exactly as its text" means; this just stops pretending it is a
separate type.

**Glob and regex become one `pattern` type.** They are both "a test on a
string". `~` takes a pattern. A `match` arm takes a pattern. `:match` extracts
from one. The two syntaxes (`*.txt`, `/re/`) stay — only the type list shrinks,
and `~`'s RHS rule becomes one word instead of two.

**The dialect is part of a pattern's identity, not just its source text.** The
glob `a.*` and the regex `/a.*/` are the same characters and different
predicates — the glob does not match `abc`, the regex does. Since B1 makes one
equality back hashing, `:dedup`, `in` and `match` dispatch, an identity of
"same source and flags" would collapse those two into one value everywhere at
once. So a pattern is equal to another when the **dialect**, the source and the
flags all match, and `*.txt` as a glob is never equal to `*.txt` as a regex.

This is what keeps the merge honest: one *type* with two dialects is a smaller
thing to learn than two types, but it is only sound if the dialect survives
into the value. A merge that erased it would be a merge of the type list at the
cost of the semantics, which is not the trade being proposed.

**Stream, job and func become one `handle` class.** They already share every
property that matters: no canonical byte form, no literal form, no way across a
fork, equality by identity. Saying that once retires three separate paragraphs
saying it three times.

**`Flag` shrinks to a marked string, and `FlagTerminator` folds into it.** This
recommendation is a **revision** — an earlier draft deleted `Flag` outright and
called it the single largest simplification available. Two corrections turned
that around, and both are worth keeping written down.

**Correction 1: B1 already paid most of `Flag`'s bill.** What made the type
expensive was never the type — it was the **refusal**. `Flag` forced the
operator/`Value::eq` split, the two `TODO.md` entries scoping it, and the
`match`-arm seam where an arm compares totally where `==` refuses. B1 deletes
all of that *whatever happens to `Flag`*, because a separate class then costs a
plain `false` instead of an error. Measured after B1, keeping the type costs a
row in the class table and one literal form in an arm — not a section.

**Correction 2: "deduce the type at the definition, never mix or cast" argues
for keeping the distinction and against the *payload* typing.** Those are two
different things the current design bundles:

| What | Deduced where | Verdict |
|---|---|---|
| **that a word is an option** — `--force` vs `"--force"` | written at the **call site**, and preserved so it survives into a list | **keep** — nothing else can recover it |
| **what the payload's type is** — `--n=2` carrying an int, `--n='2'` a string | sniffed from the word's **punctuation**, at the call site | **drop** — this is exactly deduction *away* from the definition; the payload becomes text and nothing replaces the typing |

Reading `2` out of the punctuation decides a type from how the caller spelled
the word, which is the deduction this correction is against, and it is what
generates the arm spellings. So the minimal type is:

> A **flag** is a string carrying one extra bit: *written as an option*. It has
> a name and an optional text payload, and nothing else. The bare terminator
> `--` is the member with neither.

**What the payload's type becomes, stated plainly: there isn't one.** The
payload is text, and a body that wants a number converts it — `$n:int` — the
same way it already must for a positional parameter or an element of
`$sh.args`. Dropping the punctuation rule removes a typing mechanism without
adding one.

That is deliberate, and it is the honest reading of §5's line. A valued flag
declares a **default expression**, not a type (`ParamKind::Flag(Expr)`,
`parser.rs`), and the binder stores whatever value arrives (`bind_named_option`,
`repl.rs`) — so nothing in a signature today says `--n` takes an int, and §5
rules out adding parameter types to say it. Two ways to recover typed payloads
exist and neither is proposed here:

- **Parameter types** — `func f(--n: int = 0)`. Coherent, and exactly the line
  §5 declines to cross; it is a separate proposal that has to argue for itself.
- **Convert to the default's type** — `func f(--n = 0)` coerces the payload to
  an int because the default is one. Cheaper, but it is type inference by
  another name, and it has no answer when the default is computed
  (`--n = width()`), where the type is not known until the call.

Until one of those is argued, `--n=2` binds the text `2`.

That keeps the distinction and still deletes:

- the four flag arm spellings (`--verb`, `--verb="max"`, `--verb=n`,
  `--verb=_`), the first-win ordering rule they need, and the "the binder takes
  only the slot's *string* case" paragraph written to keep every typed payload
  spellable;
- `--force` vs `--force=true` as *typed* variants — they stay distinct as text,
  which is all anyone needed;
- the `--n=2` / `--n='2'` distinction;
- `FlagTerminator` as a **separate type** — `--` becomes the nameless flag, one
  member of the flag class rather than a class of its own.

`:name` and `:value` survive and now always answer text, so `:value` on a bare
switch is still an error for the same reason (there is no payload, and mesh has
no null).

**The terminator keeps its mark, and for the same reason the flag does.** An
earlier draft made `--` a plain string, reasoning that option parsing handles
it where it already handles position. That is the *argument-parsing* argument,
which this section has already rejected for `Flag`: it is true at the call
site and false one storage round trip later. `args = [-- --force]` then
`f ...$args` is the case — if `--` reduces to the text `"--"`, nothing in the
spread says whether that element was written as a terminator or is a literal
`--` being passed as data, and `DESIGN.md` §"Parameters" pins the first
reading ("a single `--` element produced by a spread terminates parsing the
same way"). The shipped parser already keys the stop off the *value* rather
than the text for exactly this reason. So the type stops being its own class;
the bit it carries survives.

**Where the distinction actually has to live, which is narrower than it looks.**
`f --force` versus `f "--force"` is visible **in the source**, so *argument
parsing* never needed a value type — the parser sees a bare `--`-word against a
quoted one, and the callee's signature says which names are options. The type
earns its keep only once a word is **stored**, and the minimal case is a plain
scalar: `x = --force` then `f $x`. `vars.rs` explains the variant with exactly
that pair — "`x = --force` binds one where `a = \"--force\"` binds the string" —
and adds the rule that makes it work, "you cannot tell from `f $x` what `$x`
is, you tell from the line that made it". `REFERENCE.md` ships the contract
(`x = --help; f $x` prints the usage). Lists are the same fact one container
out: `args = [--force out.txt]`, `f ...$args`. Once a word is a bound value or
a list element the quoting is gone, and only a tag can say which words were
meant as options. So the question is not
"does mesh need flags" but **"must option-ness survive a round trip through a
variable?"** — and for `wrapper func co(..._args) { git checkout ...$_args }`,
which is what argv forwarding looks like here, the answer is yes.

**It is not the external boundary, in either direction.** A flag renders to
argv as its text exactly as the string does, so nothing going *out* depends on
the type; the bytes are identical either way. Nothing coming *in* can carry a
mark either, and `$sh.args` is the case worth naming because an earlier draft
cited it as evidence for keeping one: it is built from the process's own
arguments — `StartupOptions.args` is a `Vec<String>` (`repl.rs`), wrapped
element-wise into `Value::String` by `invocation_entries` (`vars.rs`) — so
every element is text and no representation could recover which of them the
caller wrote bare. The quoting was gone before mesh started. Listing it as
storage that needs the mark contradicted this paragraph one line above it.

The distinction is **in-shell only**, and the storage that actually needs it is
a variable assigned in mesh source — a scalar (`x = --force`) as much as a list
(`args = [--force out.txt]`) — and a `wrapper func`'s rest parameter.

**`Flag` stays its own class either way.** It cannot join text: `--tag=v2`
projects to `"--tag=v2"`, which collides with the string of that spelling, and
that collision *is* the distinction. Under the lossless-projection rule it is
its own class or it is gone — and under B1 its own class is cheap.

**The full deletion remains on the table** as the more aggressive option, and
the cost is the same one either way: a wrapper forwarding argv cannot tell
which words were meant as options, leaving `--` and the signature as the only
mitigations. Note that even there the **terminator keeps its mark** — a `--`
that forgot it was written as one would take the `--` mitigation with it — so
the aggressive option deletes the named flag and retains the nameless one.
What has changed is the price of *not* taking it, which after B1 is roughly
one table row.

### B3. One failure

**A nonzero `Status` is the only value-level failure. `false` is a bool and
nothing else.**

`false` used to do two jobs: the boolean, and "no result" — what `gets()`
answered at EOF, what a no-`else` `if` is reaching for, what made the presence
relation a separate relation from the condition one. Overloading it is what
forced `gets()`'s pinned contract to be argued rather than derived (a blank
line must bind, EOF must not, and both are `""`-adjacent).

Under one failure:

- `gets()` at EOF returns a **failing status**. A blank line returns `""`,
  which binds. The pinned contract becomes a consequence instead of a rule.
  **Shipped**: `gets()` answers `status(1)` at end of input.
- `if x = f()` is one sentence: **run it; if its status succeeded, bind its
  value and take the branch; otherwise bind nothing and take `else`.** Relation
  4 disappears into relation 3. **The absent set has shipped ahead of the rest
  of B3**: a nonzero status is the only absence, and `false` binds like any
  other value. What is left of relation 4 is relation 5 disagreeing with it —
  see the projection note below, where `false`→`1` is still live.

  The "bind nothing" half is load-bearing, and it has since **shipped ahead of
  the rest of B3**: a failing status now takes `else` and binds nothing. It used
  to bind anyway "so the `else` branch can read the code". Binding
  unconditionally would leave `while line = gets() { … }` with `$line` holding a
  **`Status`** after the loop ends — the EOF sentinel written over the string the
  name is for, which is the absence-shaped value B3 exists to abolish, smuggled
  back in through the binder.

  The **code is not lost with it**, though two earlier drafts of this paragraph
  said so. The binding is what the rule withholds; the status channel is a
  separate one, and the right-hand side ran — so it publishes, and the `else`
  branch reads the rejected code in `$sh.status`. That used to be unreliable
  rather than unavailable, which is the worse of the two: the reading tracked
  whatever ran before the `if` (`0` fresh, `3` after an earlier `exit 3`) and
  coincided with the rejected code only when the right-hand side happened to *be*
  the failing command. `a_rejected_status_publishes_its_code_to_the_branch_it_picks`
  pins the fix across four histories, and pins the two positions agreeing: `x =
  f()` and `if x = f()` report the same code on identical text, which they did
  not before.
- A predicate that answers "not found" returns a failing status, so
  `if rootdir { … }` means what a shell reader thinks it means. If the same
  name is also to *hand back* the directory it found, that is a `str func`
  and the spelling is `if dir = rootdir() { … }`. Under §5's advantage 4 that
  would be forced — one declaration not getting both spellings — but the
  decision keeps `f arg` legal, so `if rootdir { … }` stays writable and reads
  the status. Either way B3 is what makes the failing half work, which is the
  part this bullet needs.
- `false` as *data* keeps working everywhere, including as a bound value, which
  it could not cleanly do before. **Shipped**, for the presence relation: `if x
  = false { … }` binds `false` and takes the branch. Relation 5 still projects
  it to `1`, so a `bool func` tailing in `false` still reports failure.

This also settles the `TODO.md` question about **what status a `return` of a
non-bool leaves**, which was revised upward on five consecutive reviews and
ended at "Option C costs more than the inconsistency it removes". Under one
failure the mapping needs no exception at all: producing a value **is** success,
so **every non-status return is `0`** — `false` included — and a function that
wants to report a miss has a spelling for it. Option A becomes correct instead
of merely cheapest.

**That sentence is exact under B alone, and needs one scope note once L3 is
taken.** With no declared return types, every func is untyped and the rule has
no exceptions. Under §5 the projection applies to the returns that are
**accepted**, because the type check runs first and rejects some of them: an
explicit `return "x"` from a `status func` is not projected to `0`, it is an
error telling you to write `str func` — §5's migration bullet is the same
rule read from the other end. So the order is *check, then project*, and what
reaches the projection is a typed func's checked value (status `0`), an `any`
func's unchecked one (`0`), or a status (its code). A `status func`'s
*incidental tail value* never reaches it either, being discarded for the status
of the last statement.

**`false`→1 goes with the rest of the exceptions**, and keeping it would have
contradicted B3 one paragraph after stating it. It is a truthiness remnant: it
exists so that `func is-x() { return $a == $b }` can be tested in command
position. So the whole of relation 5 collapses to one line: **a status forwards
its code; every other value is `0`.**

**That deletion has a live cost, and an earlier draft hid it behind advantage
4.** The draft said command position "refuses a typed func outright", so a
`bool func` could only be called `is-x()` and `if is-x() { … }` would test the
bool with no projection in the path. The decision declined that refusal:
`f arg` stays legal whatever a function declares. So under B3 *plus the
accepted grammar*, `if is-x` — bare, no parens — reads the **status**, which is
`0` whether the func returned `true` or `false`, and the branch is always
taken. `if is-x()` reads the bool and behaves. Two spellings of one call
disagree, silently.

That is the `if rootdir { … }` trap B3 is proud of fixing, reappearing one type
over — and it is worth stating plainly rather than discovering later:

- **Keeping `false`→1** would fix it and undo B3, since the whole point is that
  producing a value is success. Not proposed.
- **Requiring `if is-x()`** is the accepted-grammar answer, and on its own it is
  just the same silent trap with a rule written beside it.
- **The resolver report** — already proposed above for a bare `f` naming a
  visibly-typed func — is the mitigation that costs nothing new: *this is a
  `bool func`; a bare call tests its status, not its result*. It catches what
  the resolver can see, and late binding means that is not everything.

Under advantage 4 this case cannot arise, which is one concrete thing that axis
would still buy.

**Relation 5 is simplified, not deleted, and L1 counts it.** The projection
still exists — `return $v` has to leave *some* status — so Package B keeps
four relations rather than three. What changes is that it stops being a
relation **each type answers**: it asks one question, *is this a status?*, and
every other value in every class answers identically. Nothing is added to it
by adding a type, which is what the 102-cell table was measuring.

It is worth being explicit that this leaves the projection and the condition
**disagreeing about `false`** — a `bool func` returning `false` leaves status
`0`, and that same `false` fails a condition. That is not an inconsistency
surviving the cleanup; it is B3's actual content. The two answer different
questions: *did this function succeed?* — yes, it produced the answer it exists
to produce — and *is this value true?* — no. The old design conflated them by
projecting `false` to `1`, which is exactly the "no result" overload B3
removes.

### B4. One condition

A condition is a **bool or a status**. Command position produces a status, so
the command arm is not a third case. `and` / `or` / `not` take the same two and
answer a bool.

That is already the shipped stance; B3 is what removes the exception that was
eating it.

### B5. Order stays partial — and says why

Equality is total, order is not, and the reason is worth one sentence in the
docs because it is the only asymmetry left:

> Equality must be total because it backs **dispatch and membership** on
> genuinely heterogeneous collections — argv holds strings beside whatever else
> a program passes, and a `match` that aborted on the first arm of the wrong
> type would break programs that work. Order backs **sorting**, and a sort of
> mixed types is a mistake worth reporting rather than an answer worth
> inventing.

**Being in one class is necessary but not sufficient.** Four classes carry an
order; the rest carry none, and `<` errors inside them as readily as across
them:

| Class | `<` | Order |
|---|---|---|
| **text** | ✔ | lexical |
| **number** | ✔ | numeric, across int / float / status alike |
| **instant** | ✔ | chronological |
| **duration** | ✔ | by length |
| bool, list, map, pattern, flag, handle | ✘ | none — comparing two of them is an error |

**The ordered classes are a subset of the classes, not a re-partition of
them** — which is the payoff of B2's "a class is a set of mutually comparable
types". Had `time` stayed one class, `:sort` would admit a list holding an
instant beside a duration and the comparator would have no result to give;
splitting it at the equality level means the ordering rule needs no exception
of its own.

Ordering by class membership alone was the sloppy version of this rule: it
promises that two maps or two handles compare, and there is no answer to give.
`bool` is excluded because ranking `false` below `true` is a convention with no
use; `list` because an elementwise order needs every element pair to be
orderable, which smuggles a partial relation back inside a total-looking one.
Both are easy to add later if a use turns up, since widening accepts strictly
more than it did.

So `<` compares within an **ordered** class and errors everywhere else, `:sort`
works on a list drawn from one ordered class and errors otherwise, and the
numeric-text fall-through (`10.0 < 2.0` → `true`) becomes an error rather than
a lexicographic guess.

### B6. One pattern vocabulary

Define `~` by `match` rather than beside it:

> `$x ~ P` is `match $x { P => true ; _ => false }`.

That deletes the subset table in §"Matching" and the "which to reach for"
table with it, and it makes `~` work on ranges, alternation and non-string
literals for free — the three rows that table currently marks ✗ for no reason
other than that `~` was specified separately.

**Binding is `=`, not `~`.** `if [a b] = $x` already exists and already means
"bind if it fits". So a `match` arm holds a literal, a pattern, a range or an
alternation — and **a bare word is a literal in every position**, which retires
the top-level-vs-sub-pattern rule.

Whether list-shape arms survive is a sub-choice:

| | Keep `[cmd ...rest]` arms | Drop them |
|---|---|---|
| For | `match $args { [cmd ...rest] => … }` is the argv-dispatch idiom | one meaning for a bare word, everywhere; the deferred "richer element sub-patterns" question never has to be answered |
| Against | the position rule survives as the last special case, **and `~` needs an exception** — see below | `[cmd ...rest] = $args` then `match $cmd { … }` is two lines instead of one |

**The `~` equivalence holds only over non-binding arms**, and this is the real
price of the left column. If list-shape arms survive, `$x ~ [a b]` expands to a
`match` whose arm *binds* `a` and `b` — so a predicate spelled as a question
would answer `true` and leave two names behind, which is precisely what
"binding is `=`, not `~`" rules out. Under the left column the equivalence
therefore has to read *`P` ranges over the non-binding arm forms*, and
`$x ~ [a b]` is refused with `[a b] = $x` as its spelling.

That is an exception inside the section whose whole claim is *one* pattern
vocabulary, so it counts against keeping list arms rather than being a footnote
to them. Under the right column the equivalence needs no scope at all: with no
binding arm left anywhere, every `P` is a question and `~` asks it.

Dropping them is also cheaper than it looks *because B2 deleted the
payload-binding flag arm spellings*: with no `--verb=n` arm to support —
`--verb` survives as a plain literal, since `Flag` stays a class — list arms
are the only remaining binder position, so they are the whole cost of the rule
rather than half of it.

### B7. What falls out for free

**The reopened `if`/`match` totality question answers itself.** With no "no
result" value, a value-position `match` that hits no arm and a value-position
`if` with no `else` have nothing to yield — inventing `""` is exactly the
silent-absence guess the model refuses. So **in expression position, `if`
requires `else` and `match` must be total**; in statement position neither
does, because the value is discarded. That resolves a question currently marked
*reopened, no lean* by deriving it rather than by taste.

The terse `tag = if $root { "[root]" }` is the casualty, and the flat soft-bind
word already under discussion (`x = expr else "default"`) is its replacement —
which is the same shape, one construct over.

### B8. The whole thing, written out

> Values fall into ten classes: text, number, bool, list, map, pattern, flag,
> instant, duration, handle. `==` compares within a class and answers `false`
> across; it never errors, and `in`, `:dedup`, `match` arms and hashing are the
> same relation. `<` orders text, number, instant and duration, and errors on
> everything else. A condition is a bool or a status. A nonzero status is the
> only failure a value can carry, and a function's own status is that status if
> it returned one and `0` otherwise. A pattern is one thing and `~` asks it. A
> value-producing `if` or `match` must cover every path.

Seven sentences, against roughly 600 lines of `DESIGN.md` today.

## 4. Package C — shell-native

Package B, plus one of two further collapses. Both are listed so that B reads
as a choice rather than a default.

**C1 — delete `Boolean`.** `$a == $b` yields a **status**; `true` and `false`
become `status(0)` and `status(1)`. One condition type, **nine** classes
instead of ten, and success-is-zero throughout — the most internally consistent
shell answer available.

**Everything that answered a bool under B answers a `Status` under C1**, with
**success `0`** throughout — it is not enough to change `==` and the two
literals, because B leaves several other operations producing bools and they
would otherwise have no result type at all:

| Under B | Under C1 |
|---|---|
| `==` `!=`, `<` `<=` `>` `>=`, `in`, `:has` | `status(0)` when the relation holds, `status(1)` when it does not |
| `and` / `or` / `not` — B4 has them take a bool or a status and answer a bool | take a status and answer one; `not` flips zero and nonzero |
| `~`, defined by B6 as a `match` with `true` / `false` arms | the arms become `status(0)` / `status(1)`, so `~` still answers what a condition takes |
| the **predicate modifiers** — `:tty`, `:exists`, `:read`, `:write`, and `:f` `:d` `:l` `:x` applied to one path (`expand.rs`) | `status(0)` for yes, `status(1)` for no — they are relations in everything but name |
| **`:bool`** and `:bool(DEFAULT)`, which parse a string into one (`expand.rs`) | answers a status, and the modifier is left naming a type that no longer exists |
| a **switch parameter** — `func f(--force)` binds `$force` to a bool (`repl.rs`, `ParamKind::Switch`) | binds `status(0)` when the switch is given, `status(1)` when it is not |
| **stored settings read back** — every `$sh.options` entry (`options.rs`) and `$sh.interactive` (`vars.rs`) | become statuses in the map, so inspecting one yields `0` when it is on |
| the `bool` class in B2, and its `puts` rendering | gone — C1 is **nine** classes, and a stored predicate prints as `0` or `1` |
| **`bool func`**, a shipped member of §5's closed set (`ReturnType::Bool`, `parser.rs`) | needs an answer C1 does not currently give — see below |

B4's "a condition is a bool or a status" collapses to "a condition is a
status", which is C1's whole point; B8's summary sentence loses the same word.

The bottom four rows are what a sweep for `Value::Boolean` finds beyond the
relations, and they split two ways. The **predicate modifiers** translate as
cleanly as `==` does: `$f:exists` is a question, so answering it with a status
is the same move C1 makes everywhere, and `if $f:exists { … }` is unchanged
source. The **stored settings** do not — nothing is being asked there, a
recorded flag is being read back, which is where C1's polarity lands rather
than where it is neutral. One producer that looks like it belongs here is
already gone before C1 applies: `gets()` used to answer `false` at EOF, and
**B3's failing status has since shipped in its place** (`repl.rs`) — C1
inherits no work from it.

**`:bool` is the sharp one**, and not because of the tree cost. Its parser
reads `"1"` as true and `"0"` as false, so under C1 `"1":bool` is `status(0)`
and `"0":bool` is `status(1)`: a numeral that goes through the modifier comes
back as its opposite numeral. That is the inverted-number problem at its worst,
in the one place a user is explicitly converting between a written spelling and
mesh's answer. The modifier is also left named for a type C1 deletes, so C1
owes it either a rename or an explanation.

**`bool func` is the same problem in the declaration vocabulary, and it is new
— it exists only because §5 shipped.** `bool` is a member of the closed set the
parser reads today, so C1 deletes a type that a declaration can already name.
Translating those declarations to `status func` is *not* automatic, and the
reason is §5's own distinction: `status func` means **no value channel**, while
a func returning `true` under C1 hands back `status(0)` as a **value**. Those
are different functions, and the slot check tells them apart. So C1 owes one of:

- **use `any func`**, which is already shipped and needs nothing new. It
  keeps the value channel — so the slot check still tells it from a
  `status func` — while retaining no deleted name and coining no word. What it
  gives up is the *exact* result check, and under C1 there is nothing left to
  check: the type that check would have named is the one C1 deleted;
- **keep `bool func` as the spelling** for a status-valued channel — coherent,
  and it leaves a second annotation named for a deleted type, beside `:bool`;
- **coin a word** for it, which is a new member of a set C1 is supposed to be
  shrinking;
- **let `status func` cover both**, which collapses "no value channel" into
  "returns a status" and takes the slot check's only distinction with it.

**The first is the answer**, and an earlier draft omitted it while calling the
list exhaustive — which both overstated C1's cost and left the fourth option,
the destructive one, as the default an implementer would reach for. With
`any func` in the list, C1's naming cost lands **once**, on `:bool`, not
twice; the declaration vocabulary already has a word that fits. The loss is
real but small and belongs to C1 rather than to the annotation: a reader of
`any func is-x()` learns less than a reader of `bool func is-x()` did.

**The bool surface is wider than the operators, and it is a cost in the tree
rather than at the call sites.** Counting the relations understates the work:
six places in the shipped tree *require* a `Boolean` and refuse anything else,
and five of them are named configuration arguments rather than relations —

| Site | Written today | Under C1 |
|---|---|---|
| `re()` — `repl.rs` | `re("x", literal: true)` | unchanged |
| `style()` — `repl.rs` | `style($s, bold: true)` | unchanged |
| a modifier's default — `repl.rs` | `:has("k", false)` | unchanged |
| `:filter`'s predicate — `repl.rs` | `$xs:filter(bool func(x) { $x:len > 2 })` | unchanged |
| a switch passed by name — `repl.rs` | `f(force: true)` | unchanged |
| `$sh.options` — `options.rs` | `$sh.options.errexit = true` | unchanged |

**Not one of those call sites moves**, and the reason is the literal rule at
the top of this section: C1 deletes the *type*, not the *words*. `true` and
`false` go on being written and go on meaning what the site needs — they simply
denote `status(0)` and `status(1)`. Every relation in the table above already
produces exactly what these sites accept, so a predicate result flows into
`bold:` as readily as a literal does. What changes is six type tests inside the
interpreter, their polarity, and the wording of their diagnostics.

The cost that is real is the one already stated: **reading a stored one back.**
`$sh.options.errexit` prints `0` when it is on. That is C1's inverted-number
problem reappearing wherever a setting or a switch is inspected rather than
written, and the six sites widen where it shows up without adding a second
kind of cost. The producer sweep above widens it once more and from the other
end — `$sh.interactive` and every `$sh.options` entry are read back the same
way, and `:bool` inverts a numeral outright.

**`:filter` is the one to watch**, and the reason is already written in the
source: its predicate must answer a `Boolean` rather than anything truthy
precisely because mesh's truthiness is the shell's, so a loose check would make
`:filter(any func(x) { $x })` keep the **zeros**. C1 leaves that intact — the check
stays an exact type test, an int still fails it, and `true` / `false` remain
the spellings a reader is pointed at. The residue is one degree of confusion in
the diagnostic: it must now refuse an int while naming a status, and B2 has
already established that the two compare equal. Worth wording carefully rather
than worth counting against C1.

*Against:* `puts $is_src` prints `0` or `1`, and prints `0` for the true case.
Anyone arriving from any non-shell language reads that backwards, and a bool
stored in a map is now a number whose polarity is inverted. The six sites do
not add to that so much as say where it lands — a setting or a switch reads
back as an inverted number, having been written as a word. This is a real cost
paid on every line that reads a stored predicate. `:bool` is the case that
sharpens it from an aesthetic complaint into a defect: `"1":bool` answering
`status(0)` means the one modifier whose job is to convert a written spelling
hands back the other numeral.

**C2 — delete `Status`.** A command's value is a **bool**; the exit code lives
at `$sh.status` as an int. One condition type again, from the other end.
*Against:* this un-decides the status work — a wrapper cannot forward a code as
a value (`func w() { some-cmd; return $sh.status }` returns the *number*,
successfully), which is the exact defect that made `Status` a type. Listed for
completeness; the argument against it is already written and still holds.

**It also drags §5 along, which C1 does not** — and, worked through, does not
merely restate it. Two earlier drafts of this paragraph got the size wrong in
the same direction: the first called it a rename, the second called it a
three-place restatement. It is neither.

**Start with what `status func` actually declares**, because the name misleads:
not "returns a `Status` value" but **no value channel at all** — which is why
it discards an incidental tail, and why the must-not-fall-off-the-end rule does
not apply to it, so `func f() { x = 1 }` is legal. Rename that default to
`bool func` and it becomes an *ordinary typed func*: a value required on every
path, the tail checked exactly. `func f() { x = 1 }` turns invalid and every
incidental tail in the tree turns into an error — the migration the implicit
default exists to avoid, arriving through a word swap.

**And the concept cannot simply be renamed, because C2 removes what made it
spellable.** Under B, "no value channel" is *surfaceable*: calling a status
func for a value yields a `Status`, so `if deploy { … }` has something to read
and B4's condition takes it. That works only because `Status` is a type. Delete
it and the default has a status channel with no value form, so a condition can
reach it in exactly two ways:

- **derive a bool from the body's success** — which is row three of the
  three-way table below, *"truthiness projection, which B3 has just deleted"*.
  C2 would be reinstating relation 5 to make its own default work.
- **let a condition read the status channel directly**, which is B4's two-case
  condition — a bool *or* a status — the collapse C2 exists to perform. The
  escape hatch costs C2 its entire point.

So the **return-type row is incompatible with C2**, not restated under it. Not
because a spelling is missing but because the default's compatibility — the
majority of functions needing no new word and no edit — is what C2 cannot
preserve without undoing B3.

The command-position rule goes the same way and is the smaller half: it would
require a *bool*-returning callee, so `if deploy { … }` reads a returned `true`
rather than a successful exit — the polarity objection arriving in the
declaration syntax as well as in the values.

**The failure channel is the one place C2 comes out fine**, and it is worth
keeping because it shows the incompatibility above is specific rather than
general. §5's rule reads "`fail 1` — and the `return status(1)` it wraps — is legal from
a func of any declared type", which is how a `str func` reports a miss
without a `string | false` union; C2 deletes the type that sentence is spelled
in. It is definable: **`fail` stops being sugar and becomes primitive** —
control flow that sets `$sh.status` and unwinds *without producing a value*, so
it needs no exemption from the return check rather than being exempted by
carrying a `Status`. The typed-func rule already carves it out ("a value of its
type or an explicit `fail`"), and `if p = find-up("x")` still binds only on
success, reading `$sh.status` where B reads the returned status.

So the failure path is definable — and even it costs what C2 costs everywhere
else. Under B a failure is a **value**: bindable, storable, forwardable. Under
C2 it is control flow only, so the wrapper that wants to pass a failure along
has nothing to pass — the `func w() { some-cmd; return $sh.status }` defect
above, arriving a second time inside the return-type proposal.

**Which leaves C2 and L3 sitting badly together**: one rule restated at a cost
(command position), one definable at a cost (`fail`), and one — the default —
that C2 cannot preserve at all without reinstating the projection B3 deleted.
Taking both means either annotating the majority of functions in the tree or
undoing B3, and neither is a trade this document would recommend.

Between them, **C1 is the coherent one and C2 is not** — coherent being a claim
about the *relations*, which C1 translates without an exception and C2 cannot.
What keeps it listed rather than recommended is the polarity, not the size:
`0` for true survives every count of the surface, and the count only says how
many places a reader meets it. If that cost is not acceptable, B is the answer.

## 5. The declaration says what it returns

*(Written while the proc/func split was still open, and kept as the argument of
record. `DESIGN.md` has since decided it — no second keyword, a `func` may
declare a return type, and one that declares none has no value channel — and
declined the argument-grammar half; see the header, and the markers on
advantage 4 and everything drawn from it.)*

`TODO.md` and `DESIGN.md` treated the proc/func split as a separate open item
leaning "add `proc`, leave `func` alone". Under **L3** it is the same question
as everything above — and the better answer is not two keywords but **one
keyword with a declared return type**, mikelward's proposal:

```mesh
status func deploy(target)  { … }    # runs for effect; the default
        func deploy(target)  { … }    # identical — a bare `func` is `status func`
int     func count(path)     { … }    # returns an int
str     func path-string()   { … }    # returns a str
list    func ancestors(p)    { … }    # returns a list
```

**The bare `func` keeps meaning what it means today**, which is what makes this
cheaper than the split: the majority case — writes bytes, answers a status —
needs no new word and no rename. Only a function that produces *data* gains a
marker, and that is exactly the set whose return type a reader currently has to
reconstruct by reading the body to its last statement.

**The surface is already there.** `DESIGN.md` §"Functions" establishes a
**prefix marker** at the declaration for `wrapper func name(…) { … }` and
argues the position on its own terms. `status func` / `int func` sits in that
slot rather than inventing one, and the two **compose, in a settled order**:
the type is outermost, `status wrapper func` — `DESIGN.md` says so ("the
ordering is settled with it: the type is outermost") and `parser.rs` reads
`int wrapper func f()` as "an int-returning wrapper func". An earlier draft
here wrote the pair the other way round and called the ordering an open
question for that entry; it is neither.

### Why this beats `proc` / `func`

It is a **strict superset** of the split's benefits — everything §5 claimed
before still holds, because "is this callee a status func?" is the same one bit
the split provided — plus four things the split cannot give:

1. **`help` prints the return type.** For a shell, where the thing you most
   want to know about an unfamiliar name is what you get back, this is the
   whole point.
2. **The channel is *checked*, not merely present.** `int func f() { return "x" }`
   is a loud error at the `return`. Under `proc`/`func` the value channel exists
   but says nothing about what is in it, so a func returning a list where every
   caller expects a string is caught by nobody.
3. **Hook slots get specific — as specific as the slot actually is.** The
   effect-only hooks have exactly one type and get the sharp form of this:
   `$sh.preprompt.*`, `$sh.preexec.*`, `$sh.postexec.*`, `$sh.precd` /
   `$sh.postcd.*`, `$sh.signal.<NAME>` and `$sh.exit.*` all declare
   `status func`, not "a func" — so a handler **that declares a type** is
   refused when it is the wrong one, with a message naming the slot and both
   types, instead of running and having its result quietly dropped. The
   qualifier is load-bearing: an *inline un-annotated lambda* declares nothing,
   so there is no type to compare and the check is vacuous rather than skipped.
   For a prompt slot the value layer below still catches it; for an effect
   hook nothing does, and nothing needs to — see the lambda bullet in §7 for
   why that asymmetry is the slot kinds differing rather than a hole.

   **The prompt slot is not one of those, and saying so is the honest form of
   the advantage.** `DESIGN.md` §"Hooks and the prompt" lets a segment produce a
   plain string, a `style(…)` value, a **flat list** of renderables, a **keyed
   sub-map**, or a **structural piece** (`&rule`, `&newline`, `&fill`). So
   `$sh.prompt.dir` cannot declare `str func` without rejecting renderings
   the prompt is specified to accept, and spelling all of them at once is the
   union this section's scope line rules out. It declares **`any func`**: the
   check is *produces a value at all*, not *produces a string*. (`$sh.complete.<cmd>`
   is the other one — "a spec *or* a callable returning candidates" — and lands
   the same way.)

   **The weaker check is still the one worth having**, because it speaks to the
   mistake this document otherwise creates. `DESIGN.md` records it against the
   status decision: `&puts` in a segment slot stops failing, so "a prompt
   segment gets a piece rendering as `0`" — a mistake "the value system no
   longer has a way to catch". A `status func` has no value channel, so an
   `any func` slot refuses it **by name, before the handler runs**.

   **That is the upper of two layers, not the only one**, and saying so is what
   keeps the guarantee true for callables that declare nothing. `DESIGN.md`
   already specifies the floor in the same passage — "what stays is the slot's
   own requirement: a segment that must return a **string** still refuses a
   `Status` on its own terms" — a check on the *value handed back*, which needs
   no declaration and catches everything:

   | Layer | Checks | Applies to | Reports |
   |---|---|---|---|
   | **slot type** | the resolved callee's *declared* type | a named func, an annotated lambda | before the body runs, naming the slot and both types |
   | **slot requirement** | the *value produced* | everything, including an un-annotated lambda | when the segment renders, naming the segment |

   So `$sh.prompt.char = func() { puts ">" }` — an inline effect-only lambda,
   which declares nothing — is not silently rendered as `0`: it fails the
   requirement on the value. What it does not get is the better message. That
   is exactly what "no contract" buys and costs, and it is the same trade the
   un-annotated lambda makes everywhere else in this section.

   It also corrects the claim one paragraph up, which was too generous to the
   slot type: the floor was always there, so what the declared type adds is an
   **earlier and better-named** refusal, not the only refusal. The *shape* of a
   value stays the renderer's to report, which it already does: a bad segment
   is caught at the dispatch boundary, reported above the fresh prompt, and
   dropped.

   **Two rules the exact checking forces**, since "checking is by type, not by
   class" would otherwise answer both wrongly:

   - **A slot declaring `any func` accepts a func of any declared type**, and
     refuses only a `status func`. That is what a top type is for — without it
     a `str func` segment would fail to match the slot written to take
     everything.
   - **An `any func` handler does not satisfy a slot with a specific type.**
     Its returns are unchecked, so it promises nothing the slot can rely on.

   **A composite slot has no single callee.** `$sh.prompt.line1 = [&host-info
   &dir-info]`, and its keyed variant, hold a *list or map of references* —
   not a callable — so there is nothing for "the resolved callee's type" to
   name. The check applies to each reference the composite resolves, at the
   moment it resolves it, which is where it has to happen regardless:

   **The check is at invocation, not at registration**, for the same reason
   advantage 4 is a run-time check: `&name` is a *late-bound name reference*,
   and `DESIGN.md` §"Callables" makes that explicit — it resolves against the
   command namespace when it is called, which is what makes a re-sourced
   handler pick up its redefinition. So registering `&segment` while it names
   a `str func` and redefining `segment` as a `status func` afterwards is a
   sequence no registration-time check can catch. The slot type is checked
   against the resolved callee each time the hook fires, before the handler
   runs. Registration may still check what the name resolves to *then*, but
   only as an early warning — the invocation check is the one that holds.
4. **One declared fact, not two — proposed here and *not* accepted.** The
   argument ran: the split declares the argument grammar *and* the result
   channel, so declare only the result and let **the call grammar fall out of
   it** — command position requires a status func (or an external), everything
   else is `f(…)`. One rule derived rather than two stipulated.

   **`DESIGN.md` took the result half and kept the other half where it was.**
   Argument grammar "stays with the **caller**": `f arg` takes words and yields
   a status, `f(arg)` takes expressions and yields the return value, and a
   return type says nothing about which you may write. The reason given is the
   one this document undervalued — "a function still reads as a command at the
   prompt and as a function in an expression, **which was the affordance the
   declaration split would have spent**".

   So a typed func is *not* refused in command position, and the three
   consequences this document drew from advantage 4 are proposals rather than
   consequences. They are marked where they appear: the `rootdir` "one
   declaration gets one spelling" reading, the lost two-form affordance, and
   the migration cost for a function also called bare. What survives
   untouched is the last sentence — calling a status func for a value still
   works and still yields a `Status`, exactly as `grep(foo)` does today.

### The interlock with B3, which is what keeps it small

A declared return type usually drags a type system behind it — optionals,
unions, parameterization — because the first question anyone asks is "what
about a function that returns a string *or* nothing?"

**B3 already answered that.** With a nonzero status as the only value-level
failure, `str func find-up(marker)` either returns a string or leaves a
failing status; there is no `string | false` to spell, so no union type is
needed, and `if p = find-up("x") { … }` reads the two cases without an optional
type either. Without B3 this section would need both on day one.

So the line can be drawn, and this document draws it: **return types only.** No
parameter types, no parameterization (`list`, never `list<int>`), no unions, no
optionals. Everything past that line is a separate proposal that has to argue
for itself.

**One addition the line forces: a top type, `any`.** Without parameter types
there is no way to say that `func id(x) { return $x }` returns whatever it was
given, and the same holds for any function whose result is a parameter or an
element of a heterogeneous list. Those are legal today and must stay legal, so
they need a spelling:

```mesh
any func id(x)       { return $x }
any func first(xs)   { return $xs[0] }
```

`any func` declares a value channel with **no check at `return`** — the one
thing every other annotation buys. That is the whole cost, and it is the right
one to pay: the alternative is parameterization, which is the type system this
section exists to avoid, and refusing to spell these functions at all would
make working code unwritable rather than merely unchecked.

`any` is a **typed** func in every other respect: it must not fall off the
end, and its slot type has to match where a hook wants one. Only the `return`
check is given up. *(An earlier draft added "it is refused in command
position", carried over from advantage 4 — the decision keeps `f arg` legal
whatever a function declares, `any` included.)*

It should stay rare, and the design makes that visible rather than enforcing
it: `help` prints `any`, so a function that opted out of its own return type
says so at the place a reader looks. A codebase where `any` is common has
answered the "should mesh have parameter types" question empirically, which is
a better way to reopen it than guessing now.

Five consequences worth stating rather than discovering:

- **Checking is by *type*, not by class.** The ten classes of B2 govern
  comparison; an annotation governs production. So an `int func` returning a
  float is an error even though int and float share the number class — which is
  what stops `status func` from quietly accepting an int return.
- **The annotation types the success channel; failing is always available.**
  `fail 1` — and the `return status(1)` it wraps — is legal from a func of any
  declared type, and is *not* checked against the annotation. Without this the
  interlock above would be self-defeating: `str func find-up(marker)` is
  offered as the reason no `string | false` is needed, and if the exact check
  applied to every `return` it would be the one function that cannot report a
  miss. So the annotation reads **"when this succeeds, it produces a
  `str`"** — and `if p = find-up("x") { … }` still binds only on success, so
  no caller ever sees a `Status` in a `str`-typed name. That holds because the
  presence-bind refuses a failing status outright: it takes `else` and binds
  nothing, and it is the *only* value that does — a `false` binds like any other
  answer. It used to bind, which would have made this
  sentence false the moment `DESIGN.md`'s `T | Status(n≠0)` widening let a declared
  type answer one — the binding contract was changed rather than this guarantee
  given up. The price is that the `else` branch cannot read the failing code; a
  caller who wants it assigns first and tests the name.
- **A tail value *is* a return, and is checked as one.** A typed func whose
  body ends in an expression of its declared type has returned it — `return` is
  not required, and `str func f() { "x" }` is exactly `str func f()
  { return "x" }`. Mesh already works this way; the annotation adds the check,
  not the `return`.
- **Ending *without a value* is an error in a typed func**, on every path, for
  the same reason B7 requires a value-producing `if` to have its `else`. The two
  rules are the same rule at different scales. What this catches is a body that
  ends on a bare command, on an assignment, or on an `if` whose missing `else`
  leaves one path empty — never a body that merely ends without the word
  `return`. The failure path is written, not defaulted: a typed func ends in a
  value of its type or in an explicit `fail`, and a body that trails off into
  neither is the bug this rule catches.

  *Deliberately not called "falling off the end"*, which `TODO.md` already uses
  for `func f() { "" }` — the implicit-tail case, which the bullet above makes
  legal. Reusing the phrase here would have it name a rule and its exception at
  once.
- **A `status func` discards an incidental tail value**, and its status is the
  status of its last statement. The two rules above are scoped to typed funcs,
  and this is why: the default type has **no value channel at all**, so there is
  nothing for a tail value to flow into and nothing to type-check it against.
  The tail-value rule inverts rather than merely lapsing — in a typed func the
  tail value is the result, in a `status func` it is discarded.

**That last rule is what "the value channel disappears" actually means**, and
it is worth spelling out because the alternatives are both wrong. `ips()` is the
worked example: its body ends in a `for` loop whose own body ends on a command,
so today it hands callers an aggregate of statuses nobody wrote, purely because
every func has a value channel to put something in. (A loop over a
*value*-producing body collects data someone did mean — see the migration
diagnostic below, which judges a loop by its body rather than by its shape.)

| If a `status func` body ends in a value | Result |
|---|---|
| **discard it; status is the last statement's** *(the rule)* | `ips` returns nothing to name. Ordinary shell semantics, and no annotation churn on the majority case |
| require an explicit status on every path | every effect-only function in the tree grows a `return` it does not need — the migration the implicit default exists to avoid |
| derive a status from the value | truthiness projection, which B3 has just deleted. Reintroducing it here would undo relation 5 |

So the exactness in the first bullet is about a **declared** type, never about a
tail expression in a `status func`. If you want the aggregate, declare
`list func ips()` and the same loop produces it — which is the point: the value
channel now exists only where
someone asked for it by name.

### It is still a run-time check

This does not change with the proposal, and the distinction matters enough to
state before the table. *(The whole block assumes advantage 4's dispatch
refusal, which the decision declined — `f arg` stays legal whatever a function
declares. It is kept because §6 offers the dispatch check as an axis, and
because the run-time-versus-parse-time point below governs the slot check too,
which is live.)*

> **This is still a run-time check, because mesh resolves command names at call
> time and that is not negotiable.** `DESIGN.md`'s [nushell
> comparison](DESIGN.md#elvish--nushell-rich-value-shells) makes call-time
> resolution an explicit selling point — a later redefinition or a hook
> override is visible to existing callers, which is exactly what nushell's
> parse-time `def` resolution costs its users — and hook re-source safety *is*
> late binding. So at the moment `if rootdir { … }` is parsed, nothing knows
> whether `rootdir` will name a status func, a typed func, a builtin or an
> external.
>
> What the declaration changes is **what the check has to know**, not when it
> runs. The refusal keys off the **declared return type of the resolved
> callee**, available at the dispatch site before the body runs. Compare
> Option C, which has to
> reconstruct a four-part boundary state *after* the body has ended — the
> explicit-`return` value, the implicit result captured before `shell.result`
> is restored, whether an operand was written at all, and which of `Produced`'s
> three states the body reached — none of which the value alone can carry.
>
> Placement changes too, and that is what retires the `run_hooks` exception:
> the check belongs at **command-position dispatch**, not inside `call_func`.
> `run_hooks` calls `call_func` directly and so never passes it, instead of
> needing a caller-context distinction invented to exempt it.
>
> A resolver pass can still flag the common case — a name bound to a typed
> `func` declared in the same file with no intervening redefinition — as a
> **lint**, and `DESIGN.md`'s own proc/func entry already argues that pinning
> the kind at the declaration is what makes that class of static check
> tractable at all. It cannot be the whole rule, and this document previously
> said it was.

**Two of the four kinds of callee have no declaration to read**, which the
paragraph above names without settling. A rule that keys off the resolved
callee's declared type owes an answer for the callees that never declared one.

**Externals are free.** A command yields an exit status and nothing else, so an
external *is* a `status func` by nature — command position accepts it, `&name`
against a typed slot is refused, and that is already what advantage 4's "or an
external" carve-out says. Nothing to record.

**Builtins are not.** `builtins.rs` records `(usage, summary)` and no type at
all, and three of them deliberately ship **both spellings** — `pwd · pwd()`,
`gets [--nulls] [VAR] · gets([--nulls])`, and `status` likewise. An earlier
draft read that as two horns, and **only one of them was ever real; the other
was advantage 4's, which the decision declined.** Under advantage 4, calling
`pwd` a `str func` rejects bare `pwd` at the prompt, the spelling people
actually type — gone with the rest of that axis, since the decision keeps
`f arg` legal.

**The horn this draft called "surviving" does not survive either**, and the
tree settles it. A slot only ever reaches a builtin through the **value**
form: `$sh.prompt.dir = &pwd` dispatches at `call_function_value`, whose
`&name` branch resolves the reference at the call and hands it to
`call_named_for_value` (`repl.rs:5269-5275`), which for `pwd` is `eval_pwd`
(`5321-5323`) and yields a string. Bare `pwd` at the prompt is command
position, and the accepted design does not inspect a declared type there at
all. So the slot check never has occasion to consult the status form, and a
builtin recording **one type — its value form** — is both sufficient and
correct. Per-*form* metadata was needed only by the dispatch refusal, which
is to say only by advantage 4.

That makes this the **eighth** site tracing to advantage 4 and the first
where the residue was a claim I had already marked as surviving it — the
sweep for the rule's own words does not find a horn that was re-described as
the *other* horn. "One declaration gets one spelling" remains a rule about
**`func` declarations**; builtins stay a curated exception to it rather than
a counterexample.

The lean is to **record the value-form type in the existing table** — a third
column, one entry per builtin, naming what its `name(…)` spelling produces.
That is where it belongs rather than in a second list beside it, for the reason
the table's own comment already gives: it exists so that a new builtin cannot
arrive dispatchable but unlisted, and a return type is exactly the kind of fact
that would otherwise drift. The `·` in those usage lines goes on encoding the
two spellings informally and the column says nothing about the left-hand one,
because nothing reads it: the accepted design leaves command position alone.

*(An earlier draft asked for a type per **form** — two entries for `pwd`,
`gets` and `status`. That is a strictly larger table answering a question only
advantage 4 asks, and it is dropped rather than kept as an option: recording a
status form that nothing consults would be metadata maintained for a rule the
decision declined.)*

The alternative — **exempt builtins from the dispatch check entirely** — is
one line instead of a column, and it is what the shipped shell does today by
having no check at all. It gives up `&pwd` being checkable against a typed
slot, which is one of the four things §5 is *for*. Recorded, not recommended.

Either way it belongs in the implementation scope, and §7 now carries it.

With that correction, the four items:

| Currently open | Under a declared return type |
|---|---|
| **What status a `return` of a non-bool leaves** — Option C needs a four-part boundary state reconstructed at `call_func`, at a boundary built to discard it, shared with a caller (`run_hooks`) the rule must not apply to | *Under advantage 4, which the decision declined:* a typed func in command position refused at dispatch — the declared type off the resolved callee, checked before the body runs, so no boundary state to reconstruct and no hook exception to carve out. **Under the accepted design** the same item is settled without any dispatch rule: B3 makes every non-status return `0`, and the declaration says whether there is a value channel at all |
| **`if rootdir { … }` is silently always-yes** (fifteen such functions found porting a real config) | *Under advantage 4* `rootdir` is a `str func`, so command position rejects it and points at `if dir = rootdir() { … }` — a loud error on the line that has it rather than a silently-taken branch. **The decision declines that**, keeping `f arg` legal, so what the trap gets under the accepted design is B3's failing status, not a dispatch refusal |
| **`ips()` returns an unnamed for-loop aggregate** nobody wrote, because every func carries a value channel | `ips` is a `status func`, so it has no value channel. There is nothing to name |
| **Hook slots hold both kinds** — `$sh.prompt.dir` returns something renderable, `$sh.postcd.*` runs for effect | Each slot declares the type it takes — `status func` for the effect-only hooks, `any func` for a prompt segment, whose renderings are too many shapes to name one — so a mismatched handler that declares a type is refused when the hook fires, before it runs. An inline un-annotated lambda declares none: a prompt slot still refuses it on the value, an effect hook does not and discards it as it does today |

Costs, stated plainly:

- **A vocabulary of type names becomes claimable in declaration position.**
  `status`, `int`, `str`, `bool`, `list`, `map`, `job`, `regex`, `glob`, `stream`,
  `func`, `any` and `float` have to
  be recognized before `func`, contextually, the way `fork` is recognized only
  before a block — so `func int() { … }` stays a legal definition of a function
  named `int`. That is more surface than one `proc` would have claimed, and it
  is the main thing this option costs that the split does not.
- **Every value-returning function needs annotating — and *only* annotating,
  unless its tail is partial.** (An earlier draft added "or it is also called
  in command position"; that cost belongs to advantage 4, which the decision
  did not take, so a bare call to a typed func stays legal and its callers do
  not move.) Under the implicit default,
  `func f() { return "x" }` becomes an error telling you to write `str func`,
  and the fix is those six characters. Annotating does not drag in a rewrite of
  every implicit-return path either: once `str func` is written, a tail value
  of the declared type *is* a return, so the implicit spelling
  `str func f() { "x" }` needs the same six characters and nothing more.

  **A partial tail costs more than the annotation.**
  `func f(c) { if $c { "x" } }` returns data on one path and the no-`else` `""`
  on the other. Annotating makes that tail value position, where B7 requires
  the `else` and a typed func may not end without a value — so the port writes
  the empty case out (`else { "" }`) or reports it (`fail`). The migration
  diagnostic below finds these; what it cannot do is make them one-line edits.

  **A function called both ways would have cost more under advantage 4** —
  command position requiring a status func turns every bare `f` into a refusal
  at dispatch, so the migration would be the annotation *plus* rewriting those
  callers to `f()`, or a `status func` declaration giving up the value. **The
  decision does not take that**, so the cost is not paid: `f arg` stays legal
  whatever `f` declares. Kept as the price advantage 4 would have carried,
  since §6 offers it as an axis.

  Those call sites are also **less findable than the annotation errors**, since
  dispatch is a run-time check — a bare `f` in a branch nobody took reports the
  next time that path runs. B1's resolver policy is the mitigation and needs no
  new machinery: where the callee is visible in the source, *report* that a
  bare `f` names a typed func, on the same report-don't-refuse terms and for
  the same reason (late binding means the resolver cannot see every case, and
  the ones it can see are worth saying out loud).

  **What the error does not catch is the implicit spelling before it is
  annotated**, and an earlier draft claimed it did. `func f() { "x" }` is a bare
  func, so it is a `status func`, so there is no value channel to check the tail
  against — the tail-value rule is scoped to typed funcs and the rule that
  applies here *discards*. Nothing refuses at the call site either: calling a
  status func for a value stays legal, since the decision keeps `f arg` and
  `f()` both meaning what the caller wrote. So `x = f()` goes from `"x"` to
  `status(0)`, and the explicit case fails loudly on the declaration line
  while this one changes meaning at the call.

  **The accepted design does not leave that unsaid, and an earlier draft here
  claimed it did.** `DESIGN.md` specifies a warning exactly there — "`x = ips()`
  gets a `Status` *and a warning*" — and argues the position at length:
  "**leaning warn at the call, not at the definition** … warning on every
  typeless definition … nags the common case forever in exchange for nothing,
  and a `status func` written to silence a warning is noise rather than
  intent." That check is a run-time one, and it is in this document's own
  header table. So the migration has a mechanism already, and everything below
  is an **addition to it, not the missing fix**.

  **What the addition buys, stated as a proposal.** The call-site warning fires
  when the call runs, so a value-taking call in a branch nobody takes stays
  quiet until it is taken; a **definition-side** diagnostic finds the same
  functions without waiting for a caller. It is scoped to *changed shapes* — a
  bare `func` whose body's last statement **can produce a value on any path** —
  which is the narrow answer to `DESIGN.md`'s objection, since that is not the
  common case: an effect-only func has no value-producing tail and never fires.
  It reports that the value is being discarded and points at declaring a type.
  That covers a value expression — a literal, a variable, an interpolation, an
  arithmetic expression — and, importantly, an **`if` or `match`**:
  `func f() { if $c { "a" } else { "b" } }` returns data today, and reading
  "control flow" as exempt would let the shape most likely to be a real
  function slip through the check meant to find it.

  **It is not strictly better than the accepted one, and the asymmetry cuts
  both ways.** It warns definitions that no caller ever takes a value from,
  which is the nagging `DESIGN.md` objects to, merely narrowed rather than
  eliminated; and it deliberately misses a command-bodied loop (below) that a
  caller *may* be taking a value from, which the call-site warning would
  catch. The two are complements — one keyed on the definition's shape, one on
  the call's — so the case for the lint is coverage, not replacement, and the
  discard rule stays either way, since the three-way table above rules out
  both alternatives to it.

  ***Any* path, not *every* path**, and an earlier draft got this wrong by
  borrowing B7's totality for it. The two are different questions asked of the
  same construct, and only one of them is the diagnostic's: **a caller receives
  data as soon as *some* path produces it**, so a partial tail changes behavior
  exactly as a total one does. `func f(c) { if $c { "x" } }` returns `"x"` or
  the no-`else` `""` today and would become a silent `status(0)` — the shape
  the check exists to find, exempted for having a branch missing.

  `DESIGN.md`'s own prompt example is that shape:
  `func auth-info() { if ssh-id-missing() { style("SSH", fg: yellow) } }`,
  written against the decided no-`else`-yields-`""` rule so that "not
  applicable" contributes nothing. It is a data function by construction, and
  under the previous wording nothing would have flagged it.

  **B7 is what the *fix* has to satisfy, not what the check tests** — which is
  the relationship the earlier draft collapsed. The diagnostic finds a body
  that produces data on some path; annotating it then makes that tail value
  position, where B7 requires the `if` to have its `else` and the typed-func
  rule forbids ending without a value. So `auth-info` migrates to a `str
  func` with the empty case written out — `else { "" }`, or a `fail` if it
  should report instead. That is B3's "the failure path is written, not
  defaulted" arriving as a port, and it makes `auth-info` the second casualty
  of B7's totality rule alongside the `tag = if $root { … }` line §B7 already
  names — one the document had not spotted, in `DESIGN.md`'s worked prompt.

  **A loop is judged by its body, not by being a loop.** `DESIGN.md` is
  explicit that a `for` collects a value per completed pass, so
  `func map(xs) { for x in $xs { $x * 2 } }` builds a list that someone plainly
  meant — and exempting it because it is *shaped* like a loop would repeat, one
  paragraph later, the mistake of exempting `if` because it is shaped like
  control flow. The test recurses instead: a loop whose body can produce a value
  on any path produces data, and is diagnosed. That is also the sharper
  reading of `ips()` — its aggregate is incidental because its **body ends on a
  command**, so what the loop collects is statuses, not because a loop
  collected it. *Incidental* there is a claim about intent, not about whether a
  value exists: it exists, and the table below says what happens to it.

  **The exemptions are two different things, and an earlier draft justified
  both with one false sentence** — that they produce no value. `DESIGN.md`
  says otherwise: "body ends in a command → `Status(n)`, that command's status
  *as a value*". A command produces a value. What matters is whether the
  **caller's** value changes:

  | Tail | Caller gets today | Under a bare `func` | Changed |
  |---|---|---|---|
  | a command invocation | `Status(n)` | that same status | **no** |
  | a loop over a command-bodied body | a **list** of statuses | one status | **yes** |
  | a loop over a value-producing body | a list of values | one status | yes |

  So a command *invocation* is exempt because nothing changes — advantage 4
  keeps `x = f()` yielding a `Status` — and that is not a false negative at
  all. A **command-bodied loop** is a different case: the list collapses, so
  the exemption there is a **deliberate false negative**, and it is stated
  rather than implied.

  **Why take it anyway**: that shape is the one the discard rule exists for.
  `ips()` is it, and diagnosing it would fire on nearly every effect-only `for`
  in the tree — the majority case, and the noise that gets a diagnostic
  switched off. What the exemption knowingly misses is a function that *meant*
  the list, `func statuses(xs) { for x in $xs { run $x } }`, and that function
  has a spelling which keeps working and says so: `list func`. The migration
  for it is the annotation, exactly as for any other data function; what it
  does not get is the compiler pointing at it.
  With the diagnostic the compiler finds the sites; without it, "a one-line
  edit per data-producing function" is true and useless, because finding them
  means reading every `func` in the tree. What the diagnostic does not promise
  is that the edit is always one line — see the migration bullet above for the
  two shapes that cost more, a partial tail and a function also called bare.

  Worth keeping after the migration rather than retiring it: a value written as
  the last statement of a `status func` is dead code whenever it appears, not
  only during the port.
- **A lambda takes the same annotation and no default.** §5 was
  written about declarations and never mentioned lambdas, which is a hole
  rather than a decision: `func(params) { … }` starts with the same keyword, so
  the bare-`func` rule read literally makes `$files:map(func(f) { $f:stem })` a
  `status func` whose tail is discarded — and `:map`, `:filter`, `:each`,
  `:replaceall`'s callback form and the stored thunk `later = func() { … }` all
  break at once. The desugaring makes it worse: `DESIGN.md` defines
  `$files:filter(:exec)` as *being* `$files:filter(func(f) { $f:exec })`, so the
  shorthand would break through a form nobody wrote.

  The annotation spelling carries over unchanged — `int func(x) { $x * 2 }`.
  What an un-annotated lambda gets is **no declared type at all**, which is not
  the same as a permissive one: it is unchecked, so neither the `return` check
  nor the must-not-fall-off-the-end rule applies to it.

  **`any func` is the wrong default here, and reaching for it was the first
  answer.** `any` is a *typed* func in every respect but the `return` check —
  it must not fall off the end — and a body ending on a bare command is exactly
  what that rule catches. So `$xs:each(func(x) { puts $x })` would be an error
  under an `any` default, and the inversion would only have swapped which
  ordinary callback broke: mappers saved, effect-only `:each` callbacks lost.

  **The reason no default is right is that a lambda's mode is set by its
  consumer, not by the lambda.** `:map` calls it for a value, `:each` calls it
  for effect, and the lambda cannot see which — it may be stored in a variable
  and passed on. Both checks exist to make a *declaration's* contract
  enforceable; a lambda has no name, no `help` entry, and a body written at the
  site that consumes it. Leaving it unchecked is what mesh does today, and
  nothing in this section argues it should change.

  Annotating one opts into both checks. **A hook slot does not require the
  annotation**, and the reason is the same one: the slot check binds
  *references*, not literals. `&name` is late-bound, so a named handler can be
  redefined out from under the slot it was registered in — that is the drift
  advantage 3's invocation-time check exists to catch. A lambda written into
  the slot cannot drift, because the slot and the body are the same line, so
  requiring `str func() { "> " }` would demand a contract against a reader who
  is already looking at the body. So an un-annotated lambda in a slot should be
  accepted without a *type* check, exactly as it is everywhere else.

  **The decision went the other way, and this is the one place this document
  disagrees with it.** `DESIGN.md`'s table of accepted checks includes "a
  typeless lambda assigned to a value-taking hook slot", checked at dispatch
  against the slot's declared kind — so `$sh.prompt.char = func() { "> " }` is
  refused, and the fix is to write `str func() { "> " }`, which is exactly what
  `DESIGN.md`'s own worked example now does. The cost is the one this paragraph
  names: an annotation on a lambda whose body is on the same line as the slot
  it fills. The benefit is that it catches the effect-only lambda *before* it
  runs rather than at render, which is the sharper diagnostic. Recorded as a
  disagreement rather than silently conformed, since the case that motivated
  the other reading — a one-line prompt segment — is real and the trade is a
  judgment rather than a fact.

  **In a prompt slot, unchecked by type is not unchecked**, and advantage 3's
  two layers are why: the slot's own requirement still tests the value handed
  back, so `$sh.prompt.char = func() { puts ">" }` is refused when it renders
  rather than passing a `Status` through to the prompt. The annotation buys the
  earlier, better-named refusal — not the only one.

  **In an effect slot there is no second layer, and none is needed.** An event
  hook discards what its handler produces — `run_hooks` calls it and drops the
  result on purpose (`repl.rs`), "so what it runs must not become
  `$sh.status`" — so an inline `$sh.postcd.x = func() { return "x" }` is
  neither refused nor observable: the value goes exactly where a declared
  `status func`'s tail value goes, which is nowhere. That asymmetry is not an
  oversight in the exemption, it is the two slot kinds differing in what a
  wrong handler *costs*. A prompt segment renders its mistake into the prompt,
  so it earns a value-level floor; an effect hook has nothing to render and
  nothing to read back.

  **What that does cost is one claim, which advantage 3 now qualifies**: a slot
  refuses a mismatched handler **that declares a type**. `$sh.postcd.x =
  &data-func` is refused; the inline lambda spelling of the same mistake is
  not, and an explicit `return "x"` inside it passes where the identical
  `return` in a declared `status func` is an error. That is the honest residue
  of having no contract, and the alternative — requiring `status func() { … }`
  on every inline hook lambda — is the keyword-on-every-callback cost this
  section rejected one paragraph up for `:each`, spent here to catch a value
  nobody could have read.

  The storage half of this has since **shipped**: `Callable::Lambda` now carries
  `return_type: Option<ReturnType>` (`vars.rs`), `None` for a lambda that
  declares none — the un-annotated case this section argued for — and the field's
  own comment gives the reason this section gave, that a hook slot receives the
  value rather than the syntax. What is *not* built is the checking: neither
  layer of the slot check exists yet.
- **The one-definition-two-ways affordance goes** for typed funcs, *under
  advantage 4 only* — `co main --amend` at the prompt and `x = co(main, amend:
  true)` in a script could no longer be the same definition unless `co` is a
  status func. **The decision kept the affordance**, naming it as what a split
  would have spent, so this cost is not paid. Stated as the genuine loss it
  would have been: inherited from the split rather than added by the
  annotation — with the counter that two call forms per definition are also
  what makes "what does this return?" unanswerable without reading the body.

**Sub-choice: prefix or suffix — settled, and recorded as history.** The
prefix `status func f()` follows `wrapper func`, puts the most important fact
first, and **is what is built**. The suffix forms this section weighed are
rejected by name: `func f() -> int` "spends punctuation on a form neither Go
nor any shell uses", and `func f(): int` collides with `:int`, which everywhere
else *converts* rather than declares. The argument for them was real — after a
parameter list the position is unambiguous, so a suffix claims **no words at
all** and the contextual-keyword cost in the first bullet disappears — and it
lost to the type moving where a shell reader is least likely to look, with the
`wrapper` precedent on the other side.

One thing stays live and is not the same question: the **bare** postfix
`func f() int`, Go's form, which `DESIGN.md` keeps "as the fallback if the
prefix reads badly in use". A fallback contingent on experience, not a choice
open today. (An earlier draft of this paragraph said "not resolved here" and
offered the arrow as the alternative; both were true when it was written and
neither is now.)

**What is no longer proposed:** the two-keyword `proc` / `func` split, and the
additive "`proc` beside a union `func`" currently leaning in `DESIGN.md`. The
first is subsumed — `status func` *is* `proc`, spelled without a second
keyword — and the second buys none of the four rows above, since none of them
can rely on a bare `func` being value-only.

## 6. Mix and match

The packages are coherent sets, and most axes can be taken on their own. This
is the menu. **Four rows are not free-standing**, and taking them without what
they rest on leaves the document with no answer rather than a different one:

- **The return-type row's *scope line* needs B3 — the declaration itself does
  not.** This bullet used to say the row needed B3 outright, and events refuted
  it: §5 was accepted and built while B3 is still undecided, under today's
  failure model. What B3 buys is narrower and still real. §5's interlock — "no
  `string | false` to spell" — is what keeps the scope line at *return types
  only*; without B3 that duality survives, and `DESIGN.md` lists **the nullable
  encoding** among the questions the decision leaves open ("`false | T` is a
  real and useful duality; how a declaration spells it is unaddressed"). So the
  dependency is: declare freely today, but the promise that no union or optional
  is ever needed is only affordable downstream of B3.
- **The `if` / `match` totality row needs B3.** B7 derives it from "no `false`
  meaning no result"; without that, the question it answers is still open and
  the row is a preference rather than a consequence.
- **The `Flag` row's price needs B1.** Keeping the class costs a table row
  *measured after B1* — before it, the type still drags the operator/`Value::eq`
  split and the `match`-arm seam, which is what made deleting it look like the
  largest simplification available.
- **The whole C column needs B**, which §4 states as its opening line — C is B
  plus a further collapse, not an alternative to it.
- **The return-type row and C2 exclude each other**, which is the one *negative*
  entry in this list and the reason it is worth stating beside the positive
  ones. `status func` declares no value channel; C2 leaves no way to keep that
  without reinstating relation 5 or reopening B4's two-case condition. Picking
  both is not a cheaper combination, it is a contradiction — see §4.

Two more rows are conditional rather than dependent, and say so inline: `~` and
the bare-word-in-a-pattern row both turn on whether list-shape arms survive.

| Axis | Today | B | C |
|---|---|---|---|
| Equality | operator refuses, `Value::eq` total | **one total relation** + static check | same relation; answers a status under C1 |
| Types | 17 | **10 classes** | 9 (C1) |
| Flag | its own type, typed payloads | **a marked string** — distinction kept, payload becomes text and is converted in the body | same as B |
| Styled | its own type | **folded into text** | same as B |
| Glob / regex | two types | **one `pattern`**, dialect kept in its identity | same as B |
| Failure | nonzero status; `false` still projects to `1` | **nonzero status only** | status only (C1) / bool only (C2) |
| Condition | bool, status, command | **bool or status** | one type |
| Order | numeric / lexical / text fall-through | **text, number, instant, duration only; error elsewhere** | same as B |
| `~` | strict subset of arm grammar | **defined as a one-arm `match`**, over the non-binding arm forms if list arms stay | same shape; the arms answer statuses under C1 |
| Bare word in a pattern | literal at top, binder in `[ ]` | **literal everywhere** (if list arms drop) | same as B |
| Value `if` / `match` | lenient, `""` on no match | **must be total** | same as B |
| proc / func | mode at the call site | **the declaration states a return type**; bare `func` = `status func`, bare lambda = unchecked | C1: same as B. **C2: incompatible** — `status func` declares *no value channel*, not a status-valued return, and C2 leaves no way to keep that: a condition would have to derive a bool from the body's success (the truthiness projection B3 deleted) or read the status channel directly (B4's two-case condition, the thing C2 exists to collapse). Command position and `fail` are restatable at a cost; the default is not |

The three that carry most of the benefit, if only three are taken:

1. **B1** (one equality) — deletes the most prose per line of code changed.
2. **B3** (one failure) — deletes a whole relation and settles two open items.
3. **L3 / §5** (the declared return type) — reduces the most expensive open
   item from a reconstructed boundary state to a fact the declaration states
   outright. *(An earlier draft said "a check on the callee's declared type at
   dispatch", which is advantage 4 — declined. The accepted design settles the
   same item without any dispatch rule: B3 makes every non-status return `0`,
   and the declaration says whether there is a value channel at all.)*

That set is self-consistent, and the dependency between its members is the
narrow one above: B3 is what keeps §5's scope line affordable, not what makes
the declaration possible — §5 shipped without it. Dropping B3 leaves L3
standing and reopens the nullable encoding.

## 7. What it costs to build

Rough, for weighing rather than planning.

| Change | Where | Size |
|---|---|---|
| B1 one equality | delete the gate in `eval_binary`; add a resolver **diagnostic** (not a refusal) over `==` / `!=`, `in`, `:has`, list `-` and `match` arms alike — every construct that asks the question, which is not every caller of `Value::eq` | **small** — mostly deletion |
| B2 styled → text | `Value::String` carries optional style; `type_phrase` loses its grouping hack | medium |
| B2 glob + regex → pattern | one variant, two constructors | small |
| B2 strip `Flag` | keep the marked-string representation so stored and spread argv preserve option-ness, and fold `FlagTerminator` into it as the nameless member; delete the payload typing and the arm spellings it feeds | medium, and mostly deletion |
| B3 one failure | `gets()` and friends return a failing status; the `if lhs = rhs` gate follows the condition rule | small |
| B5 order | delete the `as_text` fall-through; refuse the unordered classes | trivial |
| B6 `~` via `match` | route `~` through the arm matcher; if list arms stay, refuse a binding arm on `~`'s right | small |
| B7 totality | require `else` / a total `match` in expression position | small, plus the soft-bind word |
| §5 declared return type | a return annotation on the declaration (contextual keywords before `func` — the prefix is built; the suffix forms are rejected) including the top type `any`, exact checking of the success channel at `return` **and at an implicit tail value** (a failing status passes unchecked; `any` skips the check) — the tail case is the awkward one, since `call_func` today restores the caller's `shell.result` before it classifies how the body ended, so the value is gone by the point the check would run; **the accepted checks**, and where each stands: the parse-time ones (a `return $v` from a func declaring no type; a command tail in a func declaring **any type but `status`** — phrased on the declared word rather than on satisfaction, since `DESIGN.md`'s `T | Status(n≠0)` widening makes a satisfaction test fire for a successful tail and pass a failing one; `return "hi"` against `int`, *literal operands only*), the run-time `x = f()` against a typeless callee, and the dispatch-time typeless-lambda-in-a-value-slot check — of these, `repl.rs` now reads `return_type` on the value path, so **the narrowing itself** is enforced — but none of the accepted *checks* are, including the run-time one: `x = f()` against a typeless callee now binds a `Status` silently, which is exactly what makes its warning worth building rather than something already built (`TODO.md` leaves it unchecked); the parse-time and dispatch-time ones are unbuilt too, and the bare-`return` parse-time check was dropped rather than deferred, being unanswerable from the syntax. **Separately proposed here and not accepted**: an *exact* check on every `return` and implicit tail, which needs typed variables to mean anything, and advantage 4's dispatch refusal; hook slot types checked against the resolved callee at each invocation — `status func` for the effect-only slots, `any func` for `$sh.prompt.*` and `$sh.complete.*`, with an `any` slot accepting any declared type and a composite slot (`[&a &b]`, `[k: &a]`) checked per reference rather than once, and the piece producers `rule` / `newline` / `fill` needing a recorded type the way the builtins do — over a **value-level slot requirement** that needs no declaration and so covers an un-annotated lambda, which `DESIGN.md` already specifies for the prompt and which is the layer the type check improves on rather than replaces — the effect hooks have no such layer and need none, since `run_hooks` discards a handler's result by design; **a return type per builtin**, one entry each — the **value form**, since a **hook slot** reaches a builtin only through `call_named_for_value` and `builtins.rs` records no type at all; recording the *status* form as well was an earlier ask here and is dropped, because the only rule that would have read it is advantage 4's declined dispatch refusal; the **annotated lambda** spelling and the un-annotated lambda staying unchecked — *shipped*, `Callable::Lambda` carries `Option<ReturnType>`; what remains is checking it at a slot; a **resolver report** where a bare `f` names a visibly-typed func, on B1's report-don't-refuse terms, since the dispatch refusal is otherwise only met when that path runs; and a **migration diagnostic** on a bare `func` whose last statement can produce a value on *any* path — a value expression, an `if` / `match` with any value-producing arm (partial included, since a caller gets data as soon as one path yields it), or a loop whose body does the same — an **addition to** the accepted call-site warning rather than the missing fix, finding the same functions without waiting for a caller to run, with one **declared false negative**: a loop over a command-bodied body collapses a list of statuses to one status and is left undiagnosed, since firing there would hit nearly every effect-only `for` in the tree | **large** — the only large one |
| C1 delete `Boolean` | every relation in the C1 table; the **other producers** — `:tty`, the file tests, the `:f` `:d` `:l` `:x` predicates, `:bool`, the switch-parameter binding, and the `$sh.options` / `$sh.interactive` entries; the **six sites that require a `Boolean`** (`re()`, `style()`, a modifier default, `:filter`, a switch passed by name, `$sh.options`); and the two renderers that print `true` / `false` | medium, and **entirely in the tree** — the literals survive, so no call site moves. `:bool` additionally needs a naming decision |
| `Float`, `Instant`, `Duration` | as designed | independent of all of this |

## 8. The open question

Everything above follows from the three levers except the `Flag` question,
which is a judgment call. It is now a **three-way** choice rather than the
binary an earlier draft posed, and B1 has made the stakes much lower than they
first looked:

| Option | Option-ness **through a variable** (`x = --force`, `f $x`) | Payload typing | Costs after B1 |
|---|---|---|---|
| **Delete `Flag`** | lost — `--` and the signature are the mitigations | gone | one class row, for the marked `--` the mitigation needs |
| **Marked string** *(recommended)* | kept | **deleted, and not replaced** — the payload is text, converted in the body with `$n:int` | one class row, one arm literal form |
| **Keep as today** | kept | sniffed from punctuation at the call site | the above, plus four arm spellings, the binder rule, and `--force=true` as a typed variant |

**The column deliberately does not say `f --force` vs `f "--force"`**, which an
earlier draft used and which made the deletion row overstate its own cost. A
*directly written* argument is distinguishable under all three options and
always was — the parser sees a bare `--`-word against a quoted one, and the
signature names the options — so a column reading "kept" three times would
discriminate nothing. What separates the rows is the round trip through
storage, which is the question §2 poses and the only place the type earns its
keep.

The middle row is recommended because the two things the current type bundles
pull in opposite directions under "deduce at the definition": *option-ness* is
call-site information that genuinely cannot be recovered any other way, while
*payload typing* is a type decided by punctuation, which is deduction at the
call site rather than at the definition. Dropping it leaves the payload as
text, with no mechanism replacing it — see the correction above for why that
is a deliberate gap rather than an oversight.

The narrow version of the question, if it needs deciding in one line: **must
option-ness survive a round trip through a variable?** Argument parsing does not
need it — quoting is visible in the source — and the external boundary does not
need it, since a flag renders as its text. Only storage does — `x = --force`,
`args = [--force out.txt]` and `wrapper func` forwarding, all of them
in-shell. `$sh.args` is not among them: it is the external boundary arriving,
and it is text like the rest of that boundary.
