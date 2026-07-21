# mesh, by example

**mesh** is an interactive-first Unix shell: the familiar POSIX spine you already
have in your fingers (`$()`, `&&`/`||`, `~`, pipes, redirection), with the sharp
edges removed and several things made more ergonomic and consistent.
Pipes still carry **bytes** — every external program and coreutil works exactly as
elsewhere — but *inside* the shell you get **real values**: lists, maps, and
type-directed operations, with no word-splitting footguns.

This is a taste, not the spec — see [`DESIGN.md`](../DESIGN.md) for the full design
and the rationale behind each choice.

---

## Values don't split behind your back

Assign a value with `=` and read it back with `$name`:

```
file = 'My Report.pdf'
rm $file          # one argument — "My Report.pdf", space and all
```

A value is always exactly one value. The space in `$file` can't split it into two
arguments, and an unquoted `$file` is never re-matched against filenames — so
there's no quoting to remember and nothing splits behind your back.

`$PATH` is a **list**, not a colon-string, so the `IFS=:` juggling disappears.
To **prepend** (bash's `PATH="/opt/bin:$PATH"` — new dir wins), build the list with
it first; `:dedup` drops any later duplicate, keeping the first:

```
# bash — prepend /opt/bin
export PATH="/opt/bin:$PATH"

# mesh — spread the old list after the new dir, then dedup (keep-first)
$env.PATH = [/opt/bin ...$env.PATH]:dedup
```

To **append** instead (existing entries win), that's exactly what `+=` is:
`$env.PATH += /opt/bin`.

## Modifiers instead of subshell-and-sed gymnastics

A `:`-modifier transforms a value, and maps over a list automatically — so a lot of
`basename`/`dirname`/`cut`/`sed` pipelines become a word:

```
# bash
stem=$(basename "$f" .tar.gz)
dir=$(dirname "$f")

# mesh
stem = $f:stem
dir  = $f:dir
```

```
# "the executable files in this dir, deduped" — bash needs a loop + test -x
mesh:  $files:filter(:exec)

# join a list back into a colon-string (a whole shell function, in the config this
# is ported from, collapses to one modifier)
$env.PATH:join(":")
```

## Split + destructure replaces `read` / `cut` / `IFS`

Splitting a line into fields is *split then destructure* — no monolithic `read`,
no `IFS` juggling:

```
# bash
IFS=: read -r user pass uid gid home shell <<<"$line"

# mesh
[user pass uid gid home shell] = $line:split(":")
[_ _ uid] = $line:split(":")        # _ discards fields you don't want
```

Regex captures come back as a list, so there's no `[[ =~ ]]`-then-`$BASH_REMATCH`
dance:

```
# bash
[[ $s =~ (.*)\ (.*) ]] && one=${BASH_REMATCH[1]} two=${BASH_REMATCH[2]}

# mesh — bind the groups directly; or test-and-bind in one line
[one two] = $s:match(/(.*) (.*)/)
if [key val] = $line:match(/(\w+): (.*)/) { ... }
```

## `match` and `~` replace `case` and `[[ … ]]`

```
# bash
case "$f" in
  *.bak) mv "$f" "${f%.bak}" ;;
  *)     mv "$f" "$f.bak" ;;
esac

# mesh
match $f {
  *.bak { mv $f $f:stem }
  _     { mv $f "$f.bak" }
}
```

`~` is the one-line boolean twin (`$f ~ *.txt`, `$s ~ /re/`) — one regex story, no
separate `=~`, and it's unanchored like grep (anchor with `^…$`).

## Loops keep their variables

Piping into `while read` in bash runs the loop in a subshell, so your counter
silently resets to zero. In mesh you iterate a captured **list** in the current
scope:

```
# bash: prints 0 — the loop ran in a subshell
n=0; seq 3 | while read x; do n=$((n+1)); done; echo "$n"

# mesh: n survives
n = 0
for line in $(seq 3) { n += 1 }
puts $n
```

Here's a real one — "list this machine's IPs" — from a hand-rolled config,
in mesh:

```
func ips() {
  for line in $(ip -o a sh up primary scope global) {
    [_ iface afam addr ...rest] = $line:words
    puts $iface $addr  if $afam ~ inet*
  }
}
```

## Absence is loud — unless you say it's expected

mesh never hands you a silent empty string where you asked for something that
isn't there. Asking for a missing element is a bug and says so; when absence is
*expected*, you opt into a soft form:

```
$xs[99]              # error — names the index; a missing element is a mistake
$xs:get(99, "-")      # "-" — the total accessor, when absence is normal
[a b] = $xs          # error if $xs isn't exactly two long
if [a b] = $xs { }   # soft: a wrong shape just skips the block
```

`gets()` returns `false` at end-of-input (not `""`), so `while gets line { … }`
terminates cleanly, and a blank line is still a real `""`.

## Jobs and the prompt are first-class

Jobs are structured values in `$sh.jobs`, not text you re-parse out of `jobs`
output. The prompt is a map of named, individually-replaceable segments — so a
drop-in external renderer sits *among* your own `[root]` / auth / VCS segments
instead of swallowing them. See [`docs/PROMPT.md`](PROMPT.md) for a real prompt
built this way.

```
$sh.prompt.dir  = func() { style(if inside-project() { "$(vcs prompt-info)" } else { tilde-pwd() }, fg: blue) }
$sh.prompt.auth = func() { if not ssh-id-loaded() { style("SSH", fg: yellow) } }   # nothing to show → omitted
$sh.postcd.fetch = func() { vcs auto-fetch & }                                     # runs only on a real cd
```

---

## The through-line

Everywhere mesh keeps what your fingers know (POSIX syntax, byte pipes, external
programs) and removes what bites (word splitting, `IFS`, `case`-globs-only, silent
empties, `string collect`, `BASH_REMATCH`). Rich values live *inside* the shell;
bytes cross at the process boundary — and you always know which side you're on.
