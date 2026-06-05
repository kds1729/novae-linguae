# Surface Syntax for Nova Lingua

This document defines the concrete surface syntax for the four Nova Lingua expression sub-languages: **type**, **predicate**, **value**, and **body** expressions. It is normative for the parser and pretty-printer; the JSON schemas in this directory define the AST that results.

## Purpose and scope

v0.1 records store type, predicate, and body fields as free-form strings. v0.2 made those structured ASTs mandatory, but humans still need a readable way to write and read them. This document defines:

1. A concrete grammar for each sub-language
2. The bidirectional mapping between surface strings and JSON ASTs
3. A canonical pretty-print form (AST → surface string, used for display and round-trip testing)
4. How the parser is exposed via `nl-validator`

**Not in scope:** execution semantics, type-checking, or evaluation. The surface syntax is purely about notation.

## Where the parser lives

Four new subcommand pairs are added to `nl-validator` (one per sub-language):

| Parse direction | Subcommand | Input | Output |
|---|---|---|---|
| string → AST | `parse-type` | surface string (stdin or `--input`) | type-expression JSON on stdout |
| AST → string | `unparse-type` | type-expression JSON file | surface string on stdout |
| string → AST | `parse-predicate` | surface string | predicate-expression JSON |
| AST → string | `unparse-predicate` | predicate-expression JSON file | surface string |
| string → AST | `parse-value` | surface string | value-expression JSON |
| AST → string | `unparse-value` | value-expression JSON file | surface string |
| string → AST | `parse-body` | surface string | body-expression JSON |
| AST → string | `unparse-body` | body-expression JSON file | surface string |

All subcommands exit 0 on success, non-zero on parse or I/O error. Parse errors go to stderr with a byte offset and a description.

Input for the `parse-*` commands is a raw surface string, **not** a JSON file. Pass it as a CLI argument or pipe it:

```bash
# Both forms are equivalent
nl-validator parse-type "forall a b. (a -> b) -> List a -> List b"
echo "forall a b. (a -> b) -> List a -> List b" | nl-validator parse-type

# Round-trip: parse then pretty-print back
nl-validator parse-type "forall a b. (a -> b) -> List a -> List b" \
    | nl-validator unparse-type /dev/stdin
```

The `unparse-*` commands accept a JSON file path or `-` for stdin:

```bash
nl-validator unparse-type spec/examples/type-map.json
nl-validator parse-type "List Int" | nl-validator unparse-type -
```

The implementation lives in `tooling/validator/src/surface/` with one module per sub-language and a shared lexer.

## Lexer (shared across all sub-languages)

Token kinds:

| Kind | Pattern | Notes |
|------|---------|-------|
| `Ident` | `[a-z][a-z0-9_']*` | lowercase — variable name, op name, field name |
| `Tag` | `[A-Z][A-Za-z0-9_]*` | uppercase — variant tag, type constructor |
| `ContentAddr` | `(fn\|expr\|type\|proof\|msg)_[0-9a-f]{64}` | content address |
| `Int` | `-?[0-9]+` | integer literal |
| `Float` | `-?[0-9]+\.[0-9]+` | float literal |
| `Str` | `"…"` with `\"` escaping | string literal |
| `Bytes` | `b"[0-9a-fA-F]*"` | hex-encoded bytes |
| `Arrow` | `->` | function / lambda arrow |
| `FatArrow` | `=>` | case arm arrow (body expressions only) |
| `Dot` | `.` | forall separator, field access |
| `Comma` | `,` | separator |
| `Colon` | `:` | type annotation |
| `Eq` | `=` | let binding, record field |
| `Backslash` | `\` | lambda |
| `Pipe` | `\|` | sum variant separator |
| `Lparen` | `(` | |
| `Rparen` | `)` | |
| `Lbrace` | `{` | |
| `Rbrace` | `}` | |
| `Lbracket` | `[` | |
| `Rbracket` | `]` | |
| `Semi` | `;` | case arm terminator |
| `Underscore` | `_` | wildcard pattern |

Whitespace and `--`-to-end-of-line comments are skipped between tokens.

## 1. Type expressions

**Schema:** `type-expression.schema.json` — nine kinds: `var`, `ref`, `builtin`, `forall`, `fn`, `apply`, `tuple`, `record`, `sum`.

### Grammar

```
type      ::= "forall" ident+ "." type          -- forall (binder)
            | fn_type

fn_type   ::= app_type ("->" fn_type)?          -- right-associative

app_type  ::= atom_type+                        -- left-associative juxtaposition

atom_type ::= ident                             -- var (lowercase) or builtin (see below)
            | tag                               -- builtin type constructor shorthand (List, etc.)
            | content_addr                      -- ref
            | "(" type ")"                      -- grouping
            | "(" type "," type ("," type)* ")" -- tuple (2+ elements)
            | "{" fields "}"                    -- record
            | "[" variants "]"                  -- sum

fields    ::= field ("," field)*
field     ::= ident ":" type

variants  ::= variant ("|" variant)*
variant   ::= tag ("(" type ")")?
```

### Builtin names

Lowercase builtins (parsed as `ident`, resolved to `builtin` kind):

`int`, `nat`, `float`, `bool`, `string`, `bytes`, `unit`, `never`

Uppercase shorthands for common constructors — parsed as `tag`, resolved to an `apply` node with the named constructor:

`List`, `Option`, `Result`, `Map`, `Set`, `IO`

Any other `Tag` token in a type position is a type variable that happens to start uppercase — currently an error (type variables are lowercase per the schema). Reserved for future higher-kinded work.

### Precedence summary (lowest → highest)

1. `forall` binder
2. `->` (right-associative)
3. Juxtaposition / application (left-associative) — `List a` binds tighter than `->`
4. Atoms (builtins, vars, refs, parenthesised, compound literals)

### AST mapping examples

| Surface | JSON AST |
|---------|----------|
| `int` | `{"kind":"builtin","name":"int"}` |
| `a` | `{"kind":"var","name":"a"}` |
| `List a` | `{"kind":"apply","ctor":{"kind":"builtin","name":"List"},"args":[{"kind":"var","name":"a"}]}` |
| `a -> b` | `{"kind":"fn","param":{"kind":"var","name":"a"},"ret":{"kind":"var","name":"b"}}` |
| `(a -> b) -> List a -> List b` | fn with fn-param, right-associative |
| `forall a b. (a -> b) -> List a -> List b` | `{"kind":"forall","vars":["a","b"],"body":{…fn…}}` |
| `(int, bool)` | `{"kind":"tuple","elems":[…int…,…bool…]}` |
| `{name: string, age: nat}` | `{"kind":"record","fields":[…]}` |
| `[Some(a) \| None]` | `{"kind":"sum","variants":[{"tag":"Some","payload":{…}},{"tag":"None"}]}` |

### Canonical pretty-print rules

- Builtins: lowercase name
- `forall`: `forall <vars joined by space>. <body>`
- `fn`: `<param> -> <ret>`; parenthesise `param` if it is itself a `fn` type: `(a -> b) -> c`
- `apply`: ctor followed by space-separated args; parenthesise each arg that is `fn` or `apply`
- `tuple`: `(<elem>, <elem>, …)`
- `record`: `{<name>: <type>, …}` sorted by field name
- `sum`: `[<Tag>(<payload>) | <Tag> | …]` in declaration order
- `ref`: emit content address literally

---

## 2. Predicate expressions

**Schema:** `predicate-expression.schema.json` — five kinds: `var`, `lit`, `app`, `forall`, `exists`.

### Grammar

Infix operators are syntactic sugar for `app` nodes. Precedence follows conventional arithmetic/logic rules.

```
pred     ::= "forall" ident+ "." pred           -- forall
           | "exists" ident+ "." pred           -- exists
           | or_pred

or_pred  ::= and_pred ("||" and_pred)*          -- app {op:"or"}
and_pred ::= eq_pred  ("&&" eq_pred)*           -- app {op:"and"}
eq_pred  ::= cmp_pred (("==" | "!=") cmp_pred)? -- app {op:"eq"/"neq"}
cmp_pred ::= add_pred (("<" | "<=" | ">" | ">=") add_pred)? -- app {op:"lt"/"lte"/"gt"/"gte"}
add_pred ::= mul_pred (("+" | "-") mul_pred)*   -- app {op:"add"/"sub"}
mul_pred ::= unary    (("*" | "/" | "%") unary)* -- app {op:"mul"/"div"/"mod"}
unary    ::= "!" unary                           -- app {op:"not"}
           | "-" unary                           -- app {op:"neg"}
           | call_pred

call_pred ::= atom_pred ("(" (pred ("," pred)*)? ")")?  -- app if args present, else atom

atom_pred ::= ident                              -- var
            | content_addr ("(" (pred ("," pred)*)? ")")? -- app with ref op
            | int_literal                        -- lit {value: N}
            | float_literal                      -- lit {value: F}
            | str_literal                        -- lit {value: S}
            | "true" | "false"                   -- lit {value: bool}
            | "(" pred ")"
```

### Infix → op name mapping

| Infix | `op` in AST |
|-------|------------|
| `==` | `eq` |
| `!=` | `neq` |
| `<` | `lt` |
| `<=` | `lte` |
| `>` | `gt` |
| `>=` | `gte` |
| `&&` | `and` |
| `\|\|` | `or` |
| `+` | `add` |
| `-` (binary) | `sub` |
| `*` | `mul` |
| `/` | `div` |
| `%` | `mod` |
| `!` (prefix) | `not` |
| `-` (prefix) | `neg` |

Function call syntax `f(a, b)` maps to `{"kind":"app","op":"f","args":[…a…,…b…]}`. A bare name `f` with no call parens is `{"kind":"var","name":"f"}`.

### AST mapping examples

| Surface | JSON AST |
|---------|----------|
| `x` | `{"kind":"var","name":"x"}` |
| `42` | `{"kind":"lit","value":42}` |
| `length(xs) == 0` | `{"kind":"app","op":"eq","args":[{"kind":"app","op":"length","args":[…xs…]},{"kind":"lit","value":0}]}` |
| `map(id, xs) == xs` | app eq with app map and var xs |
| `forall xs. length(map(f, xs)) == length(xs)` | forall wrapping eq |
| `p && q` | `{"kind":"app","op":"and","args":[…p…,…q…]}` |

### Canonical pretty-print rules

- Infix ops print as infix with conventional spacing: `a == b`, `p && q`
- Function calls: `f(a, b, c)`
- Quantifiers: `forall x y. <body>`, `exists n. <body>`
- Parenthesise sub-expressions when required by precedence
- `lit` values print as JSON literals: numbers unquoted, strings quoted

---

## 3. Value expressions

**Schema:** `value-expression.schema.json` — twelve kinds: `bool`, `int`, `nat`, `float`, `string`, `bytes`, `unit`, `list`, `tuple`, `record`, `variant`, `fn_ref`.

### Grammar

Value syntax is JSON-like with a few extensions.

```
value   ::= "true" | "false"                    -- bool
          | int_literal                          -- nat (≥ 0) or int (< 0)
          | float_literal                        -- float
          | str_literal                          -- string
          | bytes_literal                        -- bytes: b"deadbeef"
          | "()"                                 -- unit
          | "[" (value ("," value)*)? "]"       -- list
          | "(" value "," value ("," value)* ")" -- tuple (2+ elements required)
          | "{" (field ("," field)*)? "}"       -- record
          | tag ("(" value ")")?                -- variant
          | content_addr                         -- fn_ref
```

**`int` vs `nat` disambiguation:** a non-negative integer literal parses to `nat`; a negative integer literal (`-N`) parses to `int`. To force `int` for a non-negative value use `int(N)` explicit syntax. This is the only case where a function-like syntax appears in value expressions.

```
value ::= … | "int" "(" int_literal ")"         -- explicit int (for non-negative values)
```

### AST mapping examples

| Surface | JSON AST |
|---------|----------|
| `true` | `{"kind":"bool","value":true}` |
| `42` | `{"kind":"nat","value":42}` |
| `-7` | `{"kind":"int","value":-7}` |
| `int(0)` | `{"kind":"int","value":0}` |
| `3.14` | `{"kind":"float","value":3.14}` |
| `"hello"` | `{"kind":"string","value":"hello"}` |
| `b"deadbeef"` | `{"kind":"bytes","value":"3q2+7w=="}` (base64) |
| `()` | `{"kind":"unit"}` |
| `[1, 2, 3]` | `{"kind":"list","elems":[…nat 1…,…nat 2…,…nat 3…]}` |
| `(true, 42)` | `{"kind":"tuple","elems":[…bool…,…nat…]}` |
| `{x = 1, y = 2}` | `{"kind":"record","fields":[{"name":"x","value":…},{"name":"y","value":…}]}` |
| `Some(42)` | `{"kind":"variant","tag":"Some","payload":…nat 42…}` |
| `None` | `{"kind":"variant","tag":"None"}` |
| `fn_fd20f1…` | `{"kind":"fn_ref","target":"fn_fd20f1…"}` |

### Canonical pretty-print rules

- `nat`/`int`: decimal number; negative values prefixed with `-`
- `float`: always include decimal point (`1.0`, not `1`)
- `bytes`: `b"<lowercase hex>"` (decode base64, re-encode as hex)
- `list`: `[<elem>, …]`; `[]` for empty
- `tuple`: `(<elem>, <elem>, …)`
- `record`: `{<name> = <value>, …}` sorted by field name
- `variant`: `<Tag>(<payload>)` or bare `<Tag>` if no payload
- `unit`: `()`

---

## 4. Body expressions

**Schema:** `body-expression.schema.json` — seven expression kinds (`var`, `lit`, `app`, `let`, `lambda`, `case`, `field`) and four pattern kinds (`wildcard`, `bind`, `variant`, `lit`).

### Grammar

```
expr    ::= let_expr
          | lambda_expr
          | case_expr
          | infix_expr

let_expr    ::= "let" ident "=" expr "in" expr

lambda_expr ::= "\" param+ "->" expr

case_expr   ::= "case" expr "of" "{" arm (";" arm)* ";"? "}"

arm         ::= pattern "=>" expr

param       ::= ident                            -- untyped param
              | "(" ident ":" type ")"          -- typed param (type per §1)

pattern     ::= "_"                              -- wildcard
              | ident                            -- bind
              | tag "(" pattern ")"             -- variant with payload
              | tag                              -- variant without payload
              | value                            -- lit (delegate to §3)

infix_expr  ::= app_expr (infix_op app_expr)*   -- same op table as predicates (§2)

app_expr    ::= field_expr+                     -- left-associative application

field_expr  ::= atom_expr ("." ident)*          -- field access (postfix, left-assoc)

atom_expr   ::= ident                            -- var
              | value                            -- lit (delegate to §3 for non-ident atoms)
              | "(" expr ")"
```

Infix operators in body expressions have the same precedence table as predicate expressions (§2) and map to `app` nodes with the same `op` names.

### Ambiguity: `ident` as var vs value

A lowercase `ident` in expression position is always `var`. To embed a `nat`/`int`/`float`/`string`/`bool` literal in a body expression, write the literal directly (`42`, `"hello"`, `true`) — these are not valid `ident` tokens. A `Tag` in expression position is an error unless it appears as the subject of a case pattern (where it's a variant constructor) — bare tag references are not supported in v0.1 body expressions.

### AST mapping examples

| Surface | JSON AST |
|---------|----------|
| `x` | `{"kind":"var","name":"x"}` |
| `42` | `{"kind":"lit","value":{"kind":"nat","value":42}}` |
| `f x y` | `{"kind":"app","fn":{"kind":"app","fn":…f…,"args":[…x…]},"args":[…y…]}` |
| `\x -> add(x, x)` | `{"kind":"lambda","params":[{"name":"x"}],"body":…app add…}` |
| `\(x: int) -> x` | lambda with typed param |
| `let y = 0 in y` | `{"kind":"let","name":"y","value":…nat 0…,"body":…var y…}` |
| `rec.name` | `{"kind":"field","record":…var rec…,"name":"name"}` |
| `case n of { 0 => true; _ => false }` | `{"kind":"case","scrutinee":…,"arms":[…]}` |
| `case opt of { Some(x) => x; None => 0 }` | case with variant patterns |

### Canonical pretty-print rules

- `var`: bare name
- `lit`: delegate to §3 pretty-print for the embedded value
- `app`: `<fn> <arg>` (space-separated, left-assoc); parenthesise if arg is itself an app or infix
- infix ops print as infix: `add(x, y)` → `x + y` when op is in the infix table
- `lambda`: `\<params joined by space> -> <body>`; typed params use `(<name>: <type>)` form
- `let`: `let <name> = <value> in <body>`
- `case`: `case <scrutinee> of { <arm>; … }` with each arm as `<pattern> => <expr>`
- `field`: `<record>.<name>` (parenthesise record if needed)
- Patterns: `_`, bare name (bind), `<Tag>(<pattern>)` or `<Tag>`, literal value

---

## Round-trip requirement

A conforming implementation must satisfy:

1. **Parse is a left-inverse of unparse:** `parse(unparse(ast)) == ast` for all valid ASTs.
2. **Unparse produces canonical form:** `unparse(parse(s))` is the canonical string for the input (may differ from `s` in whitespace, operator notation, or field ordering, but must parse back to the same AST).

These properties are tested by the conformance suite: for each sub-language, the manifest includes `(surface_string, ast_json)` pairs that the parse and unparse functions must reproduce exactly.

## Implementation notes

**Parser approach:** hand-written recursive descent in Rust. No parser-generator dependency. One module per sub-language sharing a common `Lexer` type. The lexer is a simple byte-walking iterator producing `(TokenKind, &str, usize)` triples (kind, text, byte offset).

**Error messages** include the byte offset and a short description: `parse error at byte 12: expected '->' or end of type, found ','`.

**New crate feature:** the parser lives in `tooling/validator/src/surface/` as a module of `nl_validator`, gated behind a `surface` feature flag in `Cargo.toml` so the core validator binary remains lean. The `nl-validator` binary enables the feature unconditionally.

**Conformance vectors:** a new section in `spec/conformance/manifest.json` lists `(sub_language, surface_string, expected_ast_path)` triples. The `parse-*` subcommands are tested by `cargo test --test conformance` alongside the existing hash/sign/schema vectors.
