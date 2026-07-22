# Parser contract

The clean-break parser target for M3. Unlike [`GRAMMAR.md`](GRAMMAR.md), which
records the subset accepted by the current incremental lexer, this file fixes
the structure the replacement parser must produce. Productions marked *later*
reserve their precedence and shape; the first parser does not have to evaluate
them.

The contract deliberately separates three concerns:

1. the lexer preserves spelling, quoting, adjacency, and source spans;
2. the parser turns tokens into syntax without expanding variables or globs;
3. evaluation resolves values, performs expansion, and runs commands.

Whitespace separates command words but is otherwise trivia. A newline is a
statement terminator unless the parser is incomplete. Comments and heredoc
bodies are lexical input, not grammar punctuation.

## Lexical contract

The lexer emits tokens with byte spans. Longest match wins for punctuation:
`...`, `<<<`, `..=`, `..`, `<<`, `>>`, `&&`, and `||` are each one token. Redirections are
tokens even without surrounding whitespace. Value operators require surrounding
whitespace when the design calls for it; word operators such as `and`, `or`, and
`in` also require word boundaries. The token stream retains whitespace boundaries
so the parser can distinguish an operator from punctuation inside a bare word.

Each command word retains its adjacent quoted and unquoted pieces. Thus
`"$dir"/'sub'/$file` is one word, while whitespace starts another. Quoted or
escaped punctuation remains word content. The parser never reconstructs quote
context from a flattened string.

Newlines remain tokens inside a buffered input unit. They are ignored inside
`(...)`, `[...]`, and after a trailing binary connector; inside `{...}` they
separate statements. A bare backslash-newline is removed by the lexer.

## Statement and command grammar

The notation is EBNF-ish: `*` means zero or more, `+` one or more, `?` optional,
and `|` separates alternatives. `NL` is a significant newline and `EOS` is end
of input.

```ebnf
source          = terminator* statement-list? terminator* EOS ;
statement-list  = and-or (list-sep and-or)* list-sep? ;
list-sep        = terminator+ | background ;
terminator      = ";" | NL ;
background      = "&" terminator* ;

and-or          = executable (("&&" | "||") executable)* ;
executable      = pipeline | compound-statement ;
pipeline        = pipe-stage (("|" | "|&") pipe-stage)* ;
pipe-stage      = simple-command postfix-guard? | value-call ;

simple-command  = command-item+ ;
command-item    = command-word | redirection ;
redirection     = redirect-op command-word | heredoc-redirect ;
redirect-op     = "<" | ">" | ">>" | fd-redirect ;       # fd forms: later
here-string-redirect
                = "<<<" value-expression ;                 # later
heredoc-redirect
                = "<<" heredoc-delimiter HEREDOC_BODY ;
heredoc-delimiter
                = command-word ;

compound-statement
                = assignment
                | function-definition
                | if-expression
                | for-expression
                | match-expression
                | control-statement
                | expression-statement ;

assignment      = binding "+=" value-expression
                | binding "=" assignment-value ;
assignment-value
                = value-expression | background-job ;
background-job  = pipeline "&" ;
binding         = name | pattern ;                          # pattern: later
control-statement
                = ("return" | "break" | "continue") value-expression?
                  postfix-guard? ;
postfix-guard   = ("if" | "unless") value-expression ;
expression-statement
                = value-expression postfix-guard? ;

block           = "{" terminator* statement-list? terminator* "}" ;
function-definition
                = "func" name parameter-list block ;
lambda-expression
                = "func" parameter-list block ;
parameter-list  = "(" (parameter (","? parameter)*)? ")" ;
parameter       = name ;                                    # richer forms: later
if-expression   = "if" condition block
                  ("else" (if-expression | block))? ;
condition       = conditional-assignment | value-expression | pipeline ;
conditional-assignment
                = binding "=" value-expression ;
for-expression  = "for" pattern "in" value-expression block ;
match-expression
                = "match" value-expression "{" match-arm* "}" ;
match-arm       = match-pattern ("|" match-pattern)* match-guard? block
                  terminator* ;
match-guard     = "if" value-expression ;
match-pattern   = "_" | list-pattern | value-pattern ;
value-pattern   = value-expression | "*" ;
pattern         = name | "_" | list-pattern ;
list-pattern    = "[" NL* (list-pattern-item ","? NL*)* "]" ;
list-pattern-item
                = name | "_" | "..." name ;
```

A `value-pattern` uses the value-expression grammar below, so it includes exact
values, ranges, bare globs, and regex literals. The standalone `*` spelling is
listed separately because it is accepted as a catch-all glob even though `*`
cannot begin an ordinary value expression. In a `list-pattern`, names bind by
position, `_` discards an element, and at most one `...` rest binding is
permitted; list-pattern items are not recursively patterns.

`command-word` is the lexer's adjacency-preserving word token, including
interpolations and expression atoms allowed in command position. Keywords are
recognized only where the grammar expects them; an ordinary command may still
receive `if`, `unless`, `for`, or `match` as an argument. In command position,
an unquoted `if` or `unless` starts a postfix guard only when the remaining
tokens form a complete value expression. Quoting the word or leaving no viable
guard expression keeps it as a command argument.

`HEREDOC_BODY` is an opaque, span-carrying lexical token containing the lines
after the command-line newline through the matching delimiter. The lexer queues
each `<<` delimiter on the command line, reads queued bodies in source order,
and associates each body token with its `heredoc-redirect`; a quoted delimiter
also records that the body is raw. The parser does not interpret body contents.

An assignment, definition, or value expression is not inferred by expanding a
word. The parser selects it from unquoted syntax. In particular, a bare word on
an assignment RHS remains a string (`x = greet`), while attached parentheses
select a value call (`x = greet()`).

A syntactically recognizable value expression is valid as a statement, not
only a value call. This lets a block return a scalar, variable, collection,
capture, or operator expression as its final value. A command-shaped bare word
remains a command, preserving the shell-oriented default. In a condition,
`binding = value-expression` is a distinct test-and-bind node rather than an
ordinary assignment statement.

An `=` assignment may take a trailing-`&` command pipeline as its RHS. In
`j = make -j8 &`, the ampersand belongs to `background-job`, so evaluation
launches the pipeline and binds its job handle to `j`; it does not background an
assignment node. `+=` remains a value-only operation.

## Value-expression grammar

Expressions use the following precedence, from lowest to highest. Binary tiers
associate left except comparison, which is non-associative; postfix operations
associate left. Assignment and command `&&` / `||` are statement grammar and do
not appear in this table.

| Precedence | Forms | Associativity |
| --- | --- | --- |
| 1 | `or` | left |
| 2 | `and` | left |
| 3 | `not` | prefix |
| 4 | `==`, `!=`, `<`, `<=`, `>`, `>=`, `~`, `!~`, `in` | none |
| 5 | `..`, `..=` | none |
| 6 | `+`, `-` | left, later |
| 7 | `*`, `/`, `%` | left, later |
| 8 | prefix `-`, expression spread `...` | prefix |
| 9 | call, member/index access, `:` modifier | left postfix |
| 10 | primary values and adjacent word pieces | n/a |

```ebnf
value-expression = or-expression ;
or-expression    = and-expression ("or" and-expression)* ;
and-expression   = not-expression ("and" not-expression)* ;
not-expression   = "not" not-expression | comparison ;
comparison       = range-expression (compare-op range-expression)? ;
compare-op       = "==" | "!=" | "<" | "<=" | ">" | ">=" | "~" | "!~" | "in" ;
range-expression = additive (".." additive?)?
                 | ".." additive?
                 | additive? "..=" additive ;
additive         = multiplicative (("+" | "-") multiplicative)* ;
multiplicative   = prefix (("*" | "/" | "%") prefix)* ;
prefix           = ("-" | "...") prefix | postfix ;
postfix          = primary postfix-part* ;
postfix-part     = call-arguments | member-access | index-access | modifier ;
call-arguments   = "(" argument-list? ")" ;
member-access    = "." name ;
index-access     = "[" (value-expression | range-expression) "]" ;
modifier         = ":" name call-arguments? ;

primary          = scalar | variable | list | map | capture | lambda-expression
                 | value-call | "(" value-expression ")"
                 | if-expression | for-expression | match-expression ;
value-call       = name call-arguments ;
capture          = "$(" terminator* statement-list? terminator* ")" ;
list             = "[" list-items? "]" ;
list-items       = list-item (list-separator? list-item)* ;
list-item        = value-expression | "..." value-expression ;
list-separator   = "," | NL ;
map              = "[" ":" "]" | "[" map-items "]" ;
map-items        = map-item ("," map-item)* ","? ;
map-item         = value-expression ":" value-expression
                 | "..." value-expression ;
argument-list    = argument ("," argument)* ","? ;
argument         = value-expression
                 | name ":" value-expression
                 | "..." value-expression ;
```

`capture` runs its nested statement list in command-substitution mode and
produces captured output for the following postfix chain, so `$(cmd):raw` is a
modifier applied to the capture node. `lambda-expression` is the named
definition form without a name and produces a callable value; it reuses the
same parameter and block grammar as `function-definition`.

`[...]` is a map if it contains a `key: value` pair or uses the empty-map form
`[:]`; otherwise it is a list. Once pair syntax selects a map, every entry must
be a pair and entries are comma-separated. Mixed list and map entries are a
syntax error. Empty `[]` is a list. A colon followed by a value is a map pair;
a recognized modifier name in a postfix chain is a modifier. Quote a literal
colon when that distinction would otherwise select syntax. The parser records
adjacency at the primary tier, but evaluation decides whether adjacent pieces
form a scalar word or distribute over a list.

Ranges are deliberately above comparisons and below arithmetic/postfix access:
`$xs[1 + 1..$n - 1]` has arithmetic endpoints, and `$x in 1..=10` compares `$x`
with one range. Chained comparisons such as `a < b < c` are errors; use `and`.

## Attachment decisions

These decisions remove ambiguities before AST implementation:

- **Backgrounding wraps the complete preceding `and-or`.** In
  `a | b && c & d`, the background node contains `a | b && c`; `d` is the next
  list item. A trailing `&` is valid. This preserves `&` as both a background
  marker and a list separator.
- **Pipelines bind more tightly than `&&` / `||`.** Both conditional operators
  have equal precedence and associate left.
- **Redirection attaches to the nearest simple command.** In `a | b >out`, the
  redirect belongs to `b`; in `a >out | b`, it belongs to `a`. Redirections may
  interleave with command words and the last redirect for a descriptor wins at
  evaluation time.
- **A postfix guard attaches to one simple command, control statement, or value
  expression**, before any `;`, newline, `&`, pipeline, or conditional-list
  operator. It is not a suffix on an entire pipeline. Both `if` and `unless`
  use this attachment; `unless` negates the guard condition.
- **A newline terminates at the shallow statement level.** Delimiter nesting or
  a trailing `|`, `|&`, `&&`, `||`, comma, or binary value operator makes it
  trivia instead. A newline after a complete operand terminates; continuing such
  an expression requires parentheses or backslash-newline.
- **Postfix access and modifiers form one chain.** `$x.a[0]:get(k):len` parses
  from left to right. Argument-free modifiers have no empty parentheses;
  `:first()` is rejected when `first` is defined as argument-free.

## Completeness and errors

The parser reports one of three outcomes: a complete syntax tree, incomplete
input, or an error. Input is incomplete only when adding tokens could complete
the current production, including an unclosed quote or delimiter, a missing
heredoc delimiter, or a trailing connector/operator. At file EOF the same state
is an error. An unexpected closer, a repeated separator, or an operator that
cannot continue the current production is immediately an error.

No expansion occurs during recovery. Error synchronization resumes after a
top-level newline or `;`, or after the closing delimiter of the current block,
so malformed function and control-flow bodies cannot leak statements into the
surrounding scope.

## Compatibility boundary

The first parser must preserve all valid forms documented in `GRAMMAR.md` unless
this contract explicitly rejects one. It may initially build placeholder AST
nodes for reserved future forms and diagnose them as unsupported. The executor,
job-control implementation, value store, and expansion rules are consumers of
the AST rather than part of this replacement.
