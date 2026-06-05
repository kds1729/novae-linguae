//! Surface syntax for the Nova Lingua expression sub-languages.
//!
//! This module implements the concrete grammar defined in
//! `spec/surface-syntax.md`: a shared lexer plus per-sub-language parsers
//! (surface string -> JSON AST) and pretty-printers (JSON AST -> canonical
//! surface string). The JSON AST it produces / consumes is the one defined by
//! the sibling JSON Schemas in `spec/` (e.g. `type-expression.schema.json`),
//! which are authoritative for AST shape.
//!
//! v0.1 status: the **type** sub-language is implemented first as the
//! self-contained proof of the round-trip contract. Predicate, value, and body
//! follow, reusing [`lexer`].
//!
//! ## Where this diverges from `spec/surface-syntax.md`
//!
//! The prose in `surface-syntax.md` predates the committed schemas in a few
//! places; where they conflict, the **schema wins** (the AST must validate and
//! interoperate with the existing validator). Concretely, for type expressions:
//!
//! * **`fn`** uses the schema's `{kind:"fn", params:[..], result}` (uncurried,
//!   multi-arg) shape, not the `{param, ret}` shape shown in the spec's mapping
//!   table — the latter would not validate against `type-expression.schema.json`.
//!   A surface arrow chain `a -> b -> c` parses to the flat
//!   `{params:[a,b], result:c}`; the canonical form never nests a `fn` in
//!   `result` position (any result-position arrow is absorbed into `params`).
//! * **Constructors** `List`, `Maybe`, `Result`, `Map`, `Set` are schema
//!   `builtin`s (PascalCase), so `List a` is
//!   `{kind:"apply", ctor:{builtin "List"}, args:[..]}`. The spec mentions
//!   `Option`/`IO`; the schema has neither (`Maybe` replaces `Option`), so those
//!   names are not recognised here.
//!
//! ## Round-trip contract
//!
//! Per `spec/surface-syntax.md`:
//! 1. `parse(unparse(ast)) == ast` for every **canonical** AST, and
//! 2. `unparse(parse(s))` is the canonical surface string for `s`.
//!
//! `unparse` always emits canonical form (e.g. record fields sorted by name);
//! `parse` is a faithful reader that preserves source order. The two compose to
//! the identity on canonical ASTs, which is exactly the subset the validator's
//! canonicalization produces.

mod body;
mod lexer;
mod predicate;
mod types;
mod values;

pub use body::{parse_body, unparse_body};
pub use lexer::SurfaceError;
pub use predicate::{parse_predicate, unparse_predicate};
pub use types::{parse_type, unparse_type};
pub use values::{parse_value, unparse_value};
