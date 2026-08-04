# mesh, by example

**mesh** is an interactive-first Unix shell: the familiar POSIX spine you already
have in your fingers (`$()`, `&&`/`||`, `~`, pipes, redirection), with the sharp
edges removed and several things made more ergonomic and consistent.
Pipes still carry **bytes** — every external program and coreutil works exactly as
elsewhere — but *inside* the shell you get **real values**: lists, maps, and
type-directed operations, with no word-splitting footguns.

Interactive use sets the priorities, and the same language is what you save to a
file: the features that make a line safe to type — nothing splits, absence is
loud, values are real — are the ones that make a script safe to leave running.

This is a taste, not the spec. Where to go next:

- [`DESIGN.md`](DESIGN.md) — the full design and the rationale behind each choice.
- [`TOUR.md`](TOUR.md) — the same ground at walking pace, in transcripts you can
  paste into a running shell.
- [`REFERENCE.md`](REFERENCE.md) — what is *implemented* today, feature by
  feature, when you need the exact behavior rather than the shape of it.

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
representation for the whole value.

`$PATH` is a **list**, not a colon-string, so the `IFS=:` juggling disappears.
To **prepend** (bash's `PATH="/opt/bin:$PATH"` — new dir wins), build the list with
it first; `:dedup` drops any later duplicate, keeping the first:

<pre>
# bash — prepend /opt/bin
export PATH="/opt/bin:$PATH"

# mesh — spread the old list after the new dir, then dedup (keep-first)
<strong>$env.PATH = [/opt/bin ...$env.PATH]:dedup</strong>
</pre>

To **append** instead (existing entries win), that's exactly what `+=` is:
`$env.PATH += /opt/bin`.

## Modifiers instead of subshell-and-sed gymnastics

A `:`-modifier transforms a value, and maps over a list automatically — so a lot of
`basename`/`dirname`/`cut`/`sed` pipelines become a word:

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
<strong>[a b] = $xs</strong>          # error if $xs isn't exactly two long
<strong>if [a b] = $xs { }</strong>   # soft: a wrong shape just skips the block
</pre>

`gets()` returns `false` at end-of-input (not `""`), so `while gets line { … }`
terminates cleanly, and a blank line is still a real `""`.

## Jobs and the prompt are first-class

Jobs are structured values in `$sh.jobs`, not text you re-parse out of `jobs`
output. The prompt is a map of named, individually-replaceable segments — so a
drop-in external renderer sits *among* your own `[root]` / auth / VCS segments
instead of swallowing them. See [`docs/PROMPT.md`](PROMPT.md) for a real prompt
built this way.

<pre>
<strong>$sh.prompt.dir  = func() { style(if inside-project() { "$(vcs prompt-info)" } else { tilde-pwd() }, fg: blue) }</strong>
<strong>$sh.prompt.auth = func() { if not ssh-id-loaded() { style("SSH", fg: yellow) } }</strong>   # nothing to show → omitted
<strong>$sh.postcd.fetch = func() { vcs auto-fetch &amp; }</strong>                                     # runs only on a real cd
</pre>

---

## The through-line

Everywhere mesh keeps what your fingers know (POSIX syntax, byte pipes, external
programs) and removes what bites (word splitting, `IFS`, `case`-globs-only, silent
empties, `string collect`, `BASH_REMATCH`). Rich values live *inside* the shell;
bytes cross at the process boundary — and you always know which side you're on.
