# M3 — Parser → AST

Token stream → `Module` (the root AST node), errors collected into a
parallel `Vec<ParseError>`. Hand-written recursive descent for items
and types; Pratt parser for expressions.

## Architecture

```text
example-projects/oxide/
├── ast.ox        ← AST node types (Item, Expr, Block, Type) + tag
│                    constants + Module shape
├── parser.ox     ← Parser struct, parse_program() entry point,
│                    parse_item/expr/block/type helpers
└── tests/m3_*.ox
```

`ast.ox` is consumed by every later stage (HIR lower walks the AST,
typeck/codegen never touch the AST directly but reference shapes
defined here). Splitting the data shapes from the parser logic
mirrors stage 0's `src/parser/ast.rs` vs `src/parser/parse/`.

## AST shape

Tagged structs with shared payload fields, just like `Token`. Lists
inside nodes (`Vec<ParamId>`, `Vec<ExprId>`) are stored as
`(off, len)` slices into per-element flat arenas owned by `Module`.

```rust
struct Module {
    // Per-node arenas
    items:   Vec<Item>,
    exprs:   Vec<Expr>,
    blocks:  Vec<Block>,
    types:   Vec<Type>,

    // Flat side arrays referenced by `(off, len)` slices on nodes
    params:        Vec<Param>,            // for FnDecl
    fields:        Vec<FieldDecl>,        // for StructDecl
    block_items:   Vec<BlockItem>,        // for Block
    sl_fields:     Vec<StructLitField>,   // for StructLit
    expr_ids:      Vec<usize>,            // for Call.args, ArrayLit.Elems
    type_ids:      Vec<usize>,            // for turbofish args, named-type type-args
    idents:        Vec<Ident>,            // for fn/struct generic_params

    // String backing (shared with the Lexer's pool — passed in by
    // value, since the lexer's pool is consumed once)
    pool:          StrBuf,

    // Top-level item ids (a slice of Module.items)
    root_items:    Vec<usize>,

    // Diagnostics
    errors:        Vec<ParseError>,
    span_start:    usize,
    span_end:      usize,
}

struct Ident {
    name_off: usize,           // pool offset
    name_len: usize,
    span_start: usize,
    span_end: usize,
}

struct Param {
    mutable: bool,
    name:    Ident,
    ty:      usize,            // TypeId
    span_start: usize,
    span_end:   usize,
}

struct FieldDecl {
    name:    Ident,
    ty:      usize,            // TypeId
    span_start: usize,
    span_end:   usize,
}

struct BlockItem {
    expr:     usize,           // ExprId
    has_semi: bool,
}

struct StructLitField {
    name:  Ident,
    value: usize,              // ExprId
    span_start: usize,
    span_end:   usize,
}
```

`Item`, `Expr`, `Block`, `Type` are each a tagged struct. The common
shape:

```rust
struct Item {
    kind: u8,                  // ITEM_FN | ITEM_STRUCT | ITEM_EXTERN | ITEM_IMPORT

    // Fn: name, generic_params (idents slice), params slice, ret_ty,
    //     body, is_variadic
    name:      Ident,
    gp_off:    usize,          // idents slice off
    gp_len:    usize,
    p_off:     usize,          // params slice off
    p_len:     usize,
    is_variadic: bool,
    ret_ty:    usize,          // TypeId or USIZE_MAX = "no annotation"
    body:      usize,          // BlockId or USIZE_MAX = "extern decl"

    // Struct: name, generic_params, fields slice
    f_off:     usize,
    f_len:     usize,

    // ExternBlock: abi (interned), nested items slice (into expr_ids
    //              repurposed; fits since item ids are also usize)
    abi:       Ident,          // store as Ident even though it's a
                               // string literal — re-uses the name slot
    ei_off:    usize,
    ei_len:    usize,

    // Import: path (interned)
    // (Reuses `name` field for the path string.)

    span_start: usize,
    span_end:   usize,
}
```

Yes, this is wasteful. Each `Item` slot is ~120 bytes. For a
compiler input file with O(100) top-level items, that's 12KB —
trivial. The simpler "one big tagged struct" wins over the
mechanical complexity of per-kind arenas.

Same pattern for `Expr`, `Block`, `Type`. See `ast.ox` for the
authoritative field list.

### Sentinel values

`USIZE_MAX` (= `0` cast back via wrapping) is the "absent" sentinel
for optional `usize` slots (`ret_ty`, `body`, optional `init`). We
define it as a function-shaped constant since Oxide has no `const`
items:

```rust
fn ID_NONE() -> usize { 0xFFFFFFFFFFFFFFFF }
```

Callers always go through `ID_NONE()` rather than literal magic
numbers.

## Item-kind tags

```rust
fn ITEM_FN()           -> u8 { 0 }
fn ITEM_STRUCT()       -> u8 { 1 }
fn ITEM_EXTERN_BLOCK() -> u8 { 2 }
fn ITEM_IMPORT()       -> u8 { 3 }
```

## Expr-kind tags

Mirroring `ExprKind` in stage 0:

```rust
fn EX_INT_LIT()    -> u8 { 0 }
fn EX_BOOL_LIT()   -> u8 { 1 }
fn EX_CHAR_LIT()   -> u8 { 2 }
fn EX_STR_LIT()    -> u8 { 3 }
fn EX_NULL()       -> u8 { 4 }
fn EX_IDENT()      -> u8 { 5 }
fn EX_PAREN()      -> u8 { 6 }
fn EX_UNARY()      -> u8 { 7 }
fn EX_BINARY()     -> u8 { 8 }
fn EX_ASSIGN()     -> u8 { 9 }
fn EX_CALL()       -> u8 { 10 }
fn EX_INDEX()      -> u8 { 11 }
fn EX_FIELD()      -> u8 { 12 }
fn EX_STRUCT_LIT() -> u8 { 13 }
fn EX_ARRAY_LIT()  -> u8 { 14 }    // Elems form
fn EX_ARRAY_RPT()  -> u8 { 15 }    // Repeat form
fn EX_ADDR_OF()    -> u8 { 16 }
fn EX_CAST()       -> u8 { 17 }
fn EX_IF()         -> u8 { 18 }
fn EX_WHILE()      -> u8 { 19 }
fn EX_LOOP()       -> u8 { 20 }
fn EX_FOR()        -> u8 { 21 }
fn EX_BREAK()      -> u8 { 22 }
fn EX_CONTINUE()   -> u8 { 23 }
fn EX_BLOCK()      -> u8 { 24 }
fn EX_RETURN()     -> u8 { 25 }
fn EX_LET()        -> u8 { 26 }
fn EX_POISON()     -> u8 { 27 }
```

## Type-kind tags

```rust
fn TY_NAMED() -> u8 { 0 }
fn TY_PTR()   -> u8 { 1 }
fn TY_ARRAY() -> u8 { 2 }
```

## Operator codes

`UnOp` / `BinOp` / `AssignOp` are small enums; we encode them as
`u8` constants. See `ast.ox`.

## Parser shape

```rust
struct Parser {
    tokens:   Vec<Token>,    // owned, transferred from Lexer
    pool:     StrBuf,        // ditto
    pos:      usize,         // index into tokens
    module:   Module,        // accumulating
}

fn parse_program(lx: Lexer) -> Module;
```

The parser consumes the lexer wholesale. Tokens move into
`parser.tokens`; the lexer's `pool` becomes `module.pool`. The
returned `Module` carries everything downstream stages need; the
lexer's storage is released.

## Grammar coverage (Tier-1)

The parser must accept everything Tier-1 includes:

- **Items**: `fn`, `struct`, `extern "C" { ... }`, `import "...";`
- **Types**: `Named`, `*const T` / `*mut T`, `[T; N]`, `[T]`, generic
  args (`Foo<T, U>`).
- **Expressions**: literals (int, bool, char, str, null), idents,
  paren, unary (`-`, `!`, `~`, `*`, `&`, `&mut`), binary (full
  precedence table), assign (`=`, `+=`, etc.), call (with optional
  turbofish), index, field, struct literal, array literal (elems +
  repeat), cast (`as`), `if`/`else`, `while`, `loop`, `for`,
  `break`/`continue`, `block`, `return`, `let`.
- **Pratt precedences** as in spec/03_PARSER.md (level 1 = `||`,
  level 2 = `&&`, ..., level 13 = unary prefix). Cribbed from the
  host's `pratt_*` table.

## Error handling

Each parse error is appended to `module.errors`. We poison the
current expression (push `EX_POISON`) and try to recover at the next
synchronization point — typically the next `;`, `}`, or `)`.

We *do* continue past errors; `M4` (HIR) refuses to lower a module
with errors but the user gets all of them in one go.

```rust
struct ParseError {
    kind:       u8,         // PE_*
    span_start: usize,
    span_end:   usize,
    // optional context strings (kept short — most diagnostics rely
    // on the kind tag + span; rich rendering is M9 territory).
    extra_off:  usize,
    extra_len:  usize,
}
```

Error tags reflect the host's surface (E0101 / E0102 etc.):

```rust
fn PE_UNEXPECTED_TOKEN()   -> u8 { 0 }   // E0101
fn PE_UNEXPECTED_EOF()     -> u8 { 1 }   // E0102
fn PE_DUPLICATE_VARIADIC() -> u8 { 2 }
fn PE_LEX_ERROR()          -> u8 { 3 }   // a TK_ERROR token reached the parser
```

(The full error list lives in `parser.ox`.)

## Acceptance

1. **Round-trip on lexer.ox.** Parse `lexer.ox` itself; assert zero
   `module.errors`.
2. **Snapshot dump.** A pretty-printer (`pretty_print(&module)`)
   emits an indented S-expression view of every node. The snapshot
   harness compares the dump for a fixed corpus of source.
3. **Coverage.** The corpus includes at least one program per
   ItemKind, ExprKind, and TypeKind variant.

## Scope discipline

This milestone produces **only AST**. Name resolution, generic-param
scoping, and module-graph loading all defer to M4. Things that
typecheck-incorrectly (`fn f<T: U>` — bound not allowed) parse
*structurally* if the grammar admits the shape; the rejection lands
later. Anything the host explicitly rejects at parse time (e.g.
non-`IntLit` length in `[T; N]`) we reject too.

## Out of scope

- Pretty-printing back to source (we only emit a debug dump).
- Lossless syntax tree (no whitespace/comment preservation; spans
  only).
- LSP-style position reporting.
- Multi-file loading (M4).
- Recovery beyond the simple sync-to-`;|}|)` strategy.
