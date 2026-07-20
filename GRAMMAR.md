# Grammar (implemented subset)

The grammar mesh **currently parses**, grown one task at a time — deliberately a
*subset* of the full language in [`DESIGN.md`](DESIGN.md), just enough for the
features built so far. Where this doc and `DESIGN.md` differ, `DESIGN.md` is the
eventual target and this file is the current reality. Decisions made ahead of the
full design are noted inline and are open to revision.

Notation is EBNF-ish: `*` = zero or more, `?` = optional, `|` = alternative,
`"x"` = literal.

## Task 1 — external commands + `exit`

```
line    = ws? words? ws? newline
words   = word (ws word)*
word    = nonspace+                 # M0: no quoting, escapes, or expansion yet
ws      = whitespace+               # Unicode whitespace (see lexer)
```

A non-empty `line` is a **command**: the first `word` names it. A builtin name
runs in-process; any other name is launched as an external program with the
remaining words as arguments.

```
exit [ N ]      # leave the shell. N is masked to 0-255 (default 0);
                # a surplus operand is reported and the shell keeps running.
```

Input must be valid UTF-8; a malformed line is rejected loudly. (Lossless
handling of non-UTF-8 command bytes is deferred to the real lexer.)

### Not yet parsed
Quoting, escapes, `$` variables and interpolation, globs, `~`, pipes `|`,
redirection `>` `<`, sequencing `;` `&&` `||`, `{ }` blocks, `func`. Each arrives
with the task that needs it, and this file grows to match.
