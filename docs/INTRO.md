# mesh, by example

**mesh** is an interactive-first Unix shell: the familiar POSIX spine you already
have in your fingers (`$()`, `&&`/`||`, `~`, pipes, redirection), with the sharp
edges removed and several things made more ergonomic and consistent.
Pipes still carry **bytes** — every external program and coreutil works exactly as
elsewhere — but *inside* the shell you get **real values**: lists, maps, and
type-directed operations, with no word-splitting footguns. That is what the first
two sections below are about — table stakes, but the ground everything else
stands on. What sits on top of it is the **postfix call chain** —
`$path:base:stem:upper`, the third section — the piece of mesh with the least
prior art in any other shell.

Interactive use sets the priorities, and the same language is what you save to a
file: the features that make a line safe to type — nothing splits, absence is
loud, values are real — are the ones that make a script safe to leave running.

This is a taste, not the spec. Where to go next:

- [`DESIGN.md`](DESIGN.md) — the full design and the rationale behind each choice.
- [`TOUR.md`](TOUR.md) — the same ground at walking pace, in transcripts you can
  paste into a running shell.
- [`REFERENCE.md`](REFERENCE.md) — what is *implemented* today, feature by
  feature, when you need the exact behavior rather than the shape of it.
- [`COMPARISON.md`](COMPARISON.md) — mesh set against bash, zsh, fish, elvish,
  and nushell, including what it gives up.
- [`UPSTREAM.md`](UPSTREAM.md) — which of these ideas another shell could adopt
  without a clean break, and which are grammar and therefore cannot travel.

In the examples, the mesh you'd type is in **bold**; the `# bash` lines are the
old way, shown for contrast.

---

## Values don't split behind your back

Assign a value with `=` and read it back with `$name`:

<pre>
<strong>photo='My Photo.jpg'</strong>
<strong>mv $photo album/</strong>          # one argument — "My Photo.jpg", space and all
</pre>

A value is always exactly one value. The space in `$photo` can't split it into two
arguments, and an unquoted `$photo` is never re-matched against filenames — so
there's no quoting to remember and nothing splits behind your back.

Maps are ordered, string-keyed values rather than flattened command words. The
same literal supports defaults followed by overrides, with later values winning
without disturbing key order:

<pre>
<strong>defaults = [host: localhost, port: 8080]</strong>
<strong>config = [...$defaults, port: 9090]</strong>
<strong>puts $config.host $config.port</strong>
localhost 9090
</pre>

Use `$config.key` for identifier keys and `${config[$name]}` for a computed key.
`:keys`, `:values`, and `:len` inspect a map without inventing a lossy string
representation for the whole value. (`subject:name` is a call applied to the
value on its left — the third section below is about that shape; take the ones
used here on their names for now.)

`$env.PATH` is a **list**, not a colon-string, so the `IFS=:` juggling
disappears.
To **prepend** (bash's `PATH="/opt/bin:$PATH"` — new dir wins), say so; `:dedup`
drops any later duplicate, keeping the first:

<pre>
# bash — prepend /opt/bin
export PATH="/opt/bin:$PATH"

# mesh — add at the front, then dedup (keep-first)
<strong>$env.PATH = $env.PATH:prepend(/opt/bin):dedup</strong>
</pre>

`:prepend` and `:append` return a new list rather than writing one, which is why
they chain like that — and why the result has to be assigned back;
`[/opt/bin ...$env.PATH]:dedup` builds the same list the long way. To **append**
instead (existing entries win), `$env.PATH = $env.PATH:append(/opt/bin)`, or the
mutating `$env.PATH += /opt/bin`. Each adds one entry; `:extend` takes a list and
adds its elements.

## Split + destructure replaces `read` / `cut` / `IFS`

Splitting a line into fields is *split then destructure* — no monolithic `read`,
no `IFS` juggling:

<pre>
# bash
IFS=: read -r user pass uid gid home shell &lt;&lt;&lt;"$line"

# mesh
<strong>[user pass uid gid home shell] = $line:split(":")</strong>
<strong>[_ _ uid] = $line:split(":")</strong>        # _ discards fields you don't want
</pre>

Regex captures come back as a list, so there's no `[[ =~ ]]`-then-`$BASH_REMATCH`
dance:

<pre>
# bash
[[ $s =~ (.*)\ (.*) ]] &amp;&amp; one=${BASH_REMATCH[1]} two=${BASH_REMATCH[2]}

# mesh — bind the groups directly; or test-and-bind in one line
<strong>[one two] = $s:match(/(.*) (.*)/)</strong>
<strong>if [key val] = $line:match(/(\w+): (.*)/) { ... }</strong>
</pre>

## `:` is a postfix call, and calls chain

The `:dedup` and `:split` above are not special forms — the colon is a call, and
that one rule is what the rest of this section is.

A bash parameter expansion can only operate on a *variable name*. `${p##*/}` is a
basename and `${f%.*}` strips the last extension, but `${${p##*/}%.*}` is a
`bad substitution` — there's nowhere to put the first result — so a two-step
transform costs two statements and a variable you never wanted:

<pre>
# bash
file=${path##*/}
stem=${file%.*}

# mesh
<strong>stem = $path:base:stem</strong>
</pre>

`subject:name` applies `name` to `subject`, and the result is the subject of
whatever comes next. That is the whole rule, so the chain has no depth limit and
never needs a temporary. The vocabulary is **words** rather than zsh's `:h` /
`:t` / `:r` letters, and every value modifier maps over a list on its own — which
turns a lot of `basename` / `dirname` / `cut` / `sed` pipelines into a word:

<pre>
# bash
name=$(basename "$f" .tar.gz)
dir=$(dirname "$f")

# mesh
<strong>name=$f:bare</strong>      # every extension; :stem the last only, :base:stripend('.tar.gz') just that suffix
<strong>dir=$f:dir</strong>
</pre>

<pre>
# "the executable files in this dir, deduped" — bash needs a loop + test -x
mesh:  <strong>$files:filter(:exec)</strong>

# join a list back into a colon-string (a whole shell function, in the config this
# is ported from, collapses to one modifier)
<strong>$env.PATH:join(":")</strong>
</pre>

It is a **call**, not a fixed table of operators, so the vocabulary is yours to
add to. The subject sits left of the colon in the declaration, exactly where the
call site puts it:

<pre>
<strong>str func _s:shout()  { return "$_s!" }</strong>        # $x:shout
<strong>str func _s:wrap(_c) { return "$_c$_s$_c" }</strong>   # $x:wrap("*")
<strong>str func ..._xs:oxford(_conj) { … }</strong>       # a ... subject takes the whole list at once
</pre>

A plain subject receives one element, so `$xs:shout` on `[a b]` gives
`[a! b!]`; a `...` subject receives the list whole. The built-ins split the same
way — `:stem` maps, `:len` and `:join` consume the collection. And the chain is
an ordinary word, so it works
in argument position — where a shell actually spends its day:

<pre>
<strong>cp $f:base $dest/</strong>
<strong>if $f:ext == gz { … }</strong>
<strong>for d in $env.PATH:dedup { … }</strong>
</pre>

That last part is the unusual one. zsh has the operator but a closed set of
cryptic letters; YSH has the general chain (`x => f()`) but only inside an
expression; fish and nushell have the vocabulary but reach it through a pipeline.
[`COMPARISON.md`](COMPARISON.md#transforming-a-value) is the full survey,
including what mesh could still borrow.

## `match` and `~` replace `case` and `[[ … ]]`

<pre>
# bash
case "$f" in
  *.bak) mv "$f" "${f%.bak}" ;;
  *)     mv "$f" "$f.bak" ;;
esac

# mesh
<strong>match $f {
  *.bak =&gt; { mv $f $f:stem }
  _     =&gt; { mv $f "$f.bak" }
}</strong>
</pre>

`~` is the one-line boolean twin (`$f ~ *.txt`, `$s ~ /re/`) — one regex story, no
separate `=~`, and it's unanchored like grep (anchor with `^…$`).

## Loops iterate a real list, in the current scope

Every shell has loops. What differs is what you get to iterate, and bash makes
you pick which breakage you can live with: pipe into `while read` and the body
runs in a subshell, so whatever it set is gone by `done`; or reach for
`for x in $(cmd)` and every line is re-split on `IFS` and glob-expanded, so one
filename with a space becomes two iterations.

mesh has one shape and neither problem. A capture is a **string** until you say
how to split it — `:lines` says it — and the loop runs right where you wrote it.
So a line is one value, spaces and all, nothing re-globs, and what the body sets
is still set afterwards:

<pre>
# bash — pick your poison
n=0; seq 3 | while read x; do n=$((n+1)); done; echo "$n"   # 0: the subshell ate it
for f in $(ls); do echo "$f"; done                          # "My Photo.jpg" arrives as two

# mesh
<strong>n=0</strong>
<strong>for line in $(seq 3):lines { n += 1 }</strong>
<strong>puts $n</strong>                                    # 3
</pre>

The split is never guessed for you, and never silently skipped either: looping
over something that isn't a list is refused, and the error names `:lines`.

Here's a real one — "list this machine's IPs" — from a hand-rolled config,
in mesh:

<pre>
<strong>func ips() {
  for line in $(ip -o a sh up primary scope global):lines {
    [_ iface afam addr ...rest] = $line:words
    puts $iface $addr  if $afam ~ inet*
  }
}</strong>
</pre>

## Absence is loud — unless you say it's expected

mesh never hands you a silent empty string where you asked for something that
isn't there. Asking for a missing element is a bug and says so; when absence is
*expected*, you opt into a soft form:

<pre>
<strong>$xs[99]</strong>              # error — names the index; a missing element is a mistake
<strong>$xs:get(99, "-")</strong>      # "-" — the total accessor, when absence is normal
<strong>$config.timeout</strong>       # error — "no `timeout` in this map", not ""
<strong>$env.EDITOR</strong>           # error — "$env.EDITOR: not set", not ""
<strong>$env:get(EDITOR, vi)</strong>  # vi — the same opt-in, one shape for every lookup
<strong>[a b] = $xs</strong>          # error if $xs isn't exactly two long
<strong>if [a b] = $xs { }</strong>   # soft: a wrong shape just skips the block
</pre>

An unset variable is the same: `$nope` is an error naming the name, never an
empty word that quietly changes what a command sees.

Reading input is the same story. `gets line` binds one whole line and reports
**false** at end-of-input rather than `""`, so `while gets line { … }` ends
cleanly and a blank line is still a real `""` you can act on.

## Jobs and the prompt are first-class

Jobs are structured values in `$sh.jobs`, not text you re-parse out of `jobs`
output. The prompt is a map of named, individually-replaceable segments — so a
drop-in external renderer sits *among* your own `[root]` / auth / VCS segments
instead of swallowing them. See [`docs/PROMPT.md`](PROMPT.md) for a real prompt
built this way.

<pre>
<strong>$sh.prompt.dir  = any func() { style(if inside-project() { "$(vcs prompt-info)" } else { tilde-pwd() }, fg: blue) }</strong>
<strong>$sh.prompt.auth = any func() { if not ssh-id-loaded() { style("SSH", fg: yellow) } }</strong>   # nothing to show → omitted
<strong>$sh.postcd.fetch = func() { vcs auto-fetch &amp; }</strong>                                     # runs only on a real cd
</pre>

---

## The through-line

Everywhere mesh keeps what your fingers know (POSIX syntax, byte pipes, external
programs) and removes what bites (word splitting, `IFS`, `case`-globs-only, silent
empties, `string collect`, `BASH_REMATCH`). Rich values live *inside* the shell;
bytes cross at the process boundary — and you always know which side you're on.
