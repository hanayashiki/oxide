# Modules

## Requirements

Today every Oxide program is a single file. To call a libc symbol the
user copy-pastes an `extern "C" { fn printf(...); }` block into every
program that needs it. There is no way to factor declarations into a
shared file; there is no path toward a stdlib.

This spec adds the smallest module system that lets one file pull in
top-level items from another:

```rust
import "stdio.ox";
fn main() -> i32 { printf("hi\n"); 0 }
```

The mental model is C-flavored: there are no namespaces, no name
mangling, and no `pub`/visibility. An import textually-equivalently
splats the imported file's top-level names into the importer's
scope, and the linker (logically — we still emit one LLVM module
in this round) sees a flat global symbol pool.

The work spans the parser (one new item kind), a new **loader**
layer between parsing and HIR lowering, and a small change to HIR
lowering's entry point. AST/HIR shapes for everything *inside* a
file (fns, types, exprs) are unchanged. `extern "C"` parsing and
the `HirFn { is_extern: true }` codepath stay exactly as they are
today — modules are the layer that *delivers* extern decls into
files that didn't write them.

## Design Overview

- **Splat imports.** `import "p";` makes every top-level item of
  `p` directly visible by its source name in the importing file.
  No `p.printf`, no aliasing, no per-item filtering.
- **Source-level non-transitive.** Visibility does not chain. If
  `c.ox` imports `a.ox`, and `a.ox` imports `b.ox`, then `c.ox`
  does *not* see `b.ox`'s names. Each file imports what it uses.
- **No mangling.** Linker symbols equal source names — for both
  extern decls (`printf` stays `printf`, as today) and Oxide-defined
  fns (`fn foo` becomes the symbol `foo`). Two files defining the
  same top-level name is a compile error, not a silent override.
- **One LLVM module aggregates all reachable files.** No object
  files, no system-linker invocation in this round. Codegen output
  remains "LLVM IR text" exactly as today.
- **Driver discovers from a root.** `oxide root.ox` walks `import`
  edges transitively, parses every reachable file once, and feeds
  the whole set to lowering.
- **Path resolution**: hardcoded stdlib name first, otherwise
  resolved relative to the importing file. Cycles are allowed —
  the loader dedups by canonical absolute path.

## Subset-of-Rust constraint

Rust modules carry visibility, namespacing, mangling, and a
`mod`/`use` distinction. We strip all of that. The import surface
here parses-and-means-the-same in Rust only in the trivial sense
that every accepted Oxide program uses one fully-qualified path
form (`import "..."`) that doesn't collide with any Rust syntax.
We are not pretending to track Rust here. The reasoning is the
same as the rest of the language: stay C-shaped at the symbol
boundary, defer everything else.

## Acceptance

```rust
// Stdlib (compiler-bundled) `stdio.ox`:
extern "C" { fn printf(fmt: *const u8, ...) -> i32; }
```

```rust
// main.ox — uses the stdlib import.
import "stdio.ox";

fn main() -> i32 {
    printf("hello\n");
    0
}
// ✓ resolves stdio.ox to the bundled file; `printf` visible in main.ox.
```

```rust
// main.ox + ./util.ox — relative path import.
import "./util.ox";

fn main() -> i32 { add_one(41) }
```

```rust
// util.ox
fn add_one(x: i32) -> i32 { x + 1 }
// ✓ relative paths resolve from the importing file's directory.
```

```rust
// Diamond: main imports a and b; both import stdio.
import "stdio.ox";
import "./a.ox";
import "./b.ox";

fn main() -> i32 { a_print(); b_print(); 0 }
// ✓ stdio.ox loaded once; `printf` declared once in the merged module.
```

```rust
// main.ox — missing import.
fn main() -> i32 { printf("oops\n"); 0 }
// ✗ E_UNRESOLVED_NAME on `printf` — no `import "stdio.ox";` in this file.
```

```rust
// main.ox imports two files that both define `fn helper`.
import "./a.ox";
import "./b.ox";
fn main() -> i32 { 0 }
// ✗ E_DUPLICATE_GLOBAL_SYMBOL — `helper` defined in both a.ox and b.ox.
```

```rust
// main.ox — missing file.
import "./does_not_exist.ox";
// ✗ E_IMPORT_FILE_NOT_FOUND — path does not resolve.
```

## Position in the pipeline

```text
Sources ─▶ tokens (per file) ─▶ AST (per file)
        ─▶ loader (file graph) ─▶ HirProgram ─▶ typeck ─▶ codegen
                                       ▲                     │
                                       └─────────────────────┘
                                          (single LLVM module)
```

The loader is a new layer that sits between the parser and HIR
lowering. Each individual file goes through lex → parse normally;
the loader orchestrates which files get parsed and hands lowering
an ordered list of loaded files (each tagged with a `FileId`).

**Spans carry `FileId`.** A `Span` is now
`Span { file: FileId, start, end, lsp_start, lsp_end }`. The lexer
takes the `FileId` for the file it is tokenizing; spans flow
through tokens, AST, and HIR carrying their file naturally. Without
this, byte-offset-only spans would be ambiguous in a multi-file
world — diagnostics need the file tag to point into the right
source buffer.

## AST changes

Add one new variant to `ItemKind` (`src/parser/ast.rs:33`):

```rust
pub enum ItemKind {
    Fn(FnDecl),
    ExternBlock(ExternBlock),
    Struct(StructDecl),
    /// `import "<path>";` — a request to splat another file's top-level
    /// items into this file's scope. Consumed at HIR lowering; emits no
    /// HIR node. See spec/14_MODULES.md.
    Import(ImportItem),
}

pub struct ImportItem {
    /// Path string as it appears in source, before resolution. The
    /// loader is responsible for resolving this against the importing
    /// file's directory and the stdlib hardcode table.
    pub path: String,
    pub span: Span,
}
```

### Grammar

```text
ImportItem ::= "import" StringLiteral ";"
```

`import` is a new keyword (`KwImport`), added next to `KwExtern` in
the lexer. The parser places `import_item_parser()` adjacent to
`extern_block_parser()` in `src/parser/parse/syntax.rs`, and
`item_parser()` adds it to the alternation.

`import` items are top-level only. The grammar permits them anywhere
at the top level; convention is to place them at the top of the file,
but the parser does not enforce ordering.

## Loader (new layer)

A new module `src/loader/mod.rs` provides:

```rust
pub struct LoadedFile {
    pub path: PathBuf,        // canonical absolute path
    pub source: String,       // raw source (for diagnostic spans)
    pub ast: ast::Module,
}

pub fn load_program(root: &Path) -> (Vec<LoadedFile>, Vec<LoadError>);
```

### Resolution rule

For an `ImportItem { path }` appearing in a file at canonical path `F`:

1. **Stdlib hardcode.** If `path` matches a name in the bundled
   stdlib table (initially: `"stdio.ox"`), resolve to the
   compiler-bundled file's canonical path.
2. **Relative.** Otherwise, resolve `path` against `F.parent()` and
   canonicalize.
3. **Not found.** If neither rule produces an existing readable
   file, emit `E_IMPORT_FILE_NOT_FOUND` against the `import`
   span and skip the edge (loader continues so the user gets a
   complete error report in one pass).

The loader does not interpret the path syntactically (no special-
casing of `./`, `/`, etc.) — it hands the raw string to the OS
filesystem APIs after the optional stdlib match. This keeps the
spec implementable without a path-shape mini-grammar.

### Traversal

DFS from `root`. For each newly-visited file:

1. Read the source.
2. Lex + parse; collect parse errors with the file's path attached.
3. Iterate top-level `ItemKind::Import` items; for each:
   - Resolve to a canonical path.
   - If already in the visited set, skip (handles diamonds and cycles).
   - Otherwise, mark visited and recurse.

The visited set is keyed by canonical absolute path, so two
different relative spellings that point at the same file are
collapsed.

### Cycles

`a.ox` importing `b.ox` while `b.ox` imports `a.ox` is **allowed**.
Source-level visibility is non-transitive, so the cycle is
semantically harmless: `a.ox`'s scope sees `b.ox`'s top-level
items but not its own (transitively-via-`b`) items. The loader's
visited-set dedup terminates the traversal.

The spec does not require cycle warnings.

### Output ordering

`Vec<LoadedFile>` is returned in DFS post-order, with the root
file last. Lowering uses this order to assign `FileId`s and to
iterate files in passes 1, 3, and 4.

## HIR lowering changes

The current entry point lowers a single `ast::Module` into a single
`HirModule`. Replace that with a top-level `HirProgram` that owns
program-wide arenas and a per-file structural view:

```rust
pub struct HirProgram {
    /// Globally-unique arenas. Every item across every file lives here;
    /// IDs are unique program-wide by construction.
    pub fns:    IndexVec<FnId, HirFn>,
    pub adts:   IndexVec<HAdtId, HirAdt>,
    pub locals: IndexVec<LocalId, HirLocal>,
    pub exprs:  IndexVec<HExprId, HirExpr>,
    pub blocks: IndexVec<HBlockId, HirBlock>,

    /// One `HirModule` per loaded file, indexed by `FileId`.
    pub modules: IndexVec<FileId, HirModule>,
    /// The root file the driver was invoked on.
    pub root: FileId,
}

pub struct HirModule {
    pub file: FileId,
    /// All fns whose bodies live in this file (including extern decls).
    pub fns:  Vec<FnId>,
    /// All ADTs declared in this file.
    pub adts: Vec<HAdtId>,
    /// Top-level fns / ADTs of this file (= every fn / ADT, today; the
    /// distinction is kept so future nested item shapes drop in cleanly).
    pub root_fns:  Vec<FnId>,
    pub root_adts: Vec<HAdtId>,
    pub span: Span,
}
```

Both `HirFn` and `HirAdt` gain a `file: FileId` field, so any item
can be traced back to its origin without a side-table.

Cross-file references inside `HirExprKind` (call sites, named-type
references, etc.) carry **bare** `FnId` / `HAdtId` — no wrapper
type, no `(ModuleId, _)` pair. The arenas are program-wide, so an
ID is unambiguous on its own; the file ownership is bucket
metadata in `HirModule`, not part of the ID.

The lowering entry point becomes:

```rust
pub fn lower_program(sess: &Session, files: Vec<LoadedFile>)
    -> (HirProgram, Vec<HirError>);
```

Conceptually: the loader hands lowering an ordered list of loaded
files; lowering returns the `HirProgram` plus accumulated
diagnostics.

### Name resolution — two namespaces

Oxide has two namespaces, matching Rust:

- **Types** — structs (and future type aliases / traits).
- **Values** — fns (and future constants / statics).

`struct Foo {}` and `fn Foo() {}` legally coexist in the same file:
the struct lives in the types namespace, the fn in the values
namespace. Use-site resolution picks the namespace from the
syntactic position:

| Use-site | Namespace |
|---|---|
| `HirTy::Named("Foo")` (any type position) | Types |
| Call-position `Foo(...)` / value-position `Foo` | Values |

A single `Scope` shape carries both:

```rust
pub struct Scope {
    pub types:  HashMap<String, HAdtId>,
    pub values: HashMap<String, FnId>,
}
```

The same `Scope` shape is reused in three roles during lowering:

- **`local_defs[F]`** — file `F`'s own top-level defs (output of
  pass 1).
- **`program_directory`** — the union of all `local_defs[*]`, used
  for cross-file collision detection in pass 2; not retained
  afterwards.
- **`file_scopes[F]`** — `F`'s name-resolution scope: `F`'s own
  defs ∪ defs of every file `F` directly imports (pass 3 output,
  consumed by pass 4).

### Four-pass lowering

1. **Pass 1 — Assign items to modules.** Per file, scan top-level
   items. For each `Fn`, `ExternBlock` child fn, or `Struct`,
   allocate a global `FnId` / `HAdtId` from the program's arenas
   (signatures only — no bodies yet). Push the new ID into
   `HirModule[F]`'s `fns` / `adts` / `root_fns` / `root_adts`.
   Populate `local_defs[F]: Scope` per namespace.
2. **Pass 2 — Check uniqueness.** Fold every `local_defs[F]` into
   `program_directory: Scope` per namespace. On collision in either
   namespace emit `DuplicateGlobalSymbol { name, first, duplicate,
   namespace }` and **keep the first** entry; do not abort. After
   pass 2, `program_directory` is discarded.
3. **Pass 3 — Build per-file scope.** For each file `F`, compute
   `file_scopes[F]: Scope` per namespace as
   `local_defs[F] ∪ ⋃ local_defs[G] for G in F.imports_direct`.
   Within-file collisions in either namespace (a file's own def
   shadowed by an imported name) emit a diagnostic and keep the
   local entry.
4. **Pass 4 — Lower bodies.** Per file, lower expression and
   statement bodies. A use-site name queries `file_scopes[F]` in
   the namespace dictated by syntax. The lookup yields a bare
   `FnId` / `HAdtId` — globally unique — recorded directly in HIR.

This is the entire enforcement of "non-transitive": `file_scopes[F]`
only widens by `F`'s direct imports, never by an import's imports.

### Diagnostics and recovery

When pass 2 finds a duplicate name in either namespace of
`program_directory`, it emits `DuplicateGlobalSymbol { name, first,
duplicate, namespace }` and **keeps the first entry**. Compilation
does not abort: subsequent name-resolution lookups for that name
resolve to the first definition, so the program can continue to
type-check and report further errors. The same rule applies to
within-file collisions in pass 3 (a file's own def shadowed by an
imported name in the same namespace).

Note: a single extern fn declaration shared by N importers is **not**
a duplicate — the file containing the `extern "C" { fn printf }`
block is loaded once (loader dedup), so `printf` lands in the
program's `fns` arena exactly once even if every other file
imports it.

### `Import` items emit no HIR

After contributing to `file_scopes`, `ItemKind::Import` is dropped.
Lowering produces no HIR node for it, and codegen never sees it.

## Typeck

No new typeck rules. Typeck consumes `HirProgram` and runs as
today; cross-file references are already plain `FnId` / `HAdtId`
handles into the program-wide arenas, so typeck does not see
multi-file structure beyond iterating `program.modules` for
top-level item discovery.

The only typeck-adjacent error introduced by this spec
(`DuplicateGlobalSymbol`) is fired during lowering's pass 2,
before typeck runs.

## Codegen

Codegen consumes `HirProgram`: it iterates the program-wide `fns`
arena and emits one LLVM module containing every fn. It already
emits `declare` for `HirFn { is_extern: true, body: None }` and
`define` for fns with bodies. User-defined fns already use external
linkage with their source name as the symbol — which is exactly
what "no mangling" requires.

The spec asserts as an invariant: every Oxide top-level fn — extern
or defined — produces a global LLVM symbol equal to its source
name. Codegen must not introduce per-file prefixes, hashing, or
any other mangling.

## Worked examples

### Stdlib + a single user file

`stdio.ox` (compiler-bundled):

```rust
extern "C" { fn printf(fmt: *const u8, ...) -> i32; }
```

`main.ox` (user):

```rust
import "stdio.ox";

fn main() -> i32 {
    printf("hi\n");
    0
}
```

Loader output (DFS post-order): `[stdio.ox, main.ox]`, with
`FileId(0) = stdio.ox`, `FileId(1) = main.ox`.

Lowered HIR (sketch):

```text
HirProgram {
    fns: [
        FnId(0): HirFn { name: "printf", file: FileId(0), is_extern: true,  body: None,    ... },
        FnId(1): HirFn { name: "main",   file: FileId(1), is_extern: false, body: Some(_), ... },
    ],
    modules: [
        FileId(0) → HirModule { file: FileId(0), fns: [FnId(0)], root_fns: [FnId(0)], ... },
        FileId(1) → HirModule { file: FileId(1), fns: [FnId(1)], root_fns: [FnId(1)], ... },
    ],
    root: FileId(1),
}
```

`file_scopes[main.ox].values` contains both `printf` (via the
`import`) and `main` (locally defined); `printf` resolves to
`FnId(0)`.

LLVM IR (sketch):

```text
declare i32 @printf(ptr, ...)

define i32 @main() {
  ; ... call @printf, return 0
}
```

### Diamond

```text
main.ox  ─┬─▶ a.ox ─▶ stdio.ox
          └─▶ b.ox ─▶ stdio.ox
```

Loader visits in DFS order; second arrival at `stdio.ox` is dropped
by the visited set. `printf` lands in the program's `fns` arena
once. `file_scopes[main.ox]` includes `a.ox`'s and `b.ox`'s items
but **not** `printf` — `main.ox` has no direct
`import "stdio.ox";`.

To call `printf` from `main.ox` directly, `main.ox` must add its
own `import "stdio.ox";`. The fact that `a.ox` and `b.ox` already
brought it into the program-wide symbol pool does not help
`main.ox`'s name resolution.

### Two files, colliding name

```rust
// a.ox
fn helper() -> i32 { 1 }
```

```rust
// b.ox
fn helper() -> i32 { 2 }
```

```rust
// main.ox
import "./a.ox";
import "./b.ox";
fn main() -> i32 { 0 }
```

Pass 2 folds `local_defs[a.ox]` into `program_directory`, then
attempts to fold in `local_defs[b.ox]`'s `helper` and emits
`DuplicateGlobalSymbol { name: "helper", first: <a.ox span>,
duplicate: <b.ox span>, namespace: Namespace::Values }`.
`a.ox`'s `helper` is kept; subsequent name resolution sees it.

`main.ox` itself does not even use `helper`; the error fires
purely because both files were pulled into the program. This is
the cost of "no mangling" and is intentional — under C linkage,
two definitions of `helper` would be a link-time multiple-definition
error too.

## Out of scope

- **Per-item / selective imports** (`import "stdio.ox" { printf };`).
- **Aliasing** (`import "stdio.ox" as io;`). Requires namespacing
  machinery we explicitly rejected.
- **Visibility / `pub`.** Everything top-level in an imported file
  is visible to the importer; there is no private form.
- **Multiple LLVM modules / object files / linker invocation.** The
  user-facing model already permits this evolution (the symbol
  rules are what the linker would need); deferred until a real
  driver binary lands.
- **Configurable stdlib path.** The stdlib name table is hardcoded;
  no env var, no `--stdlib-path` flag.
- **Project-root resolution** (`import "std/stdio.ox";`). Useful
  once non-stdlib multi-directory projects appear; not needed yet.
- **Cycle warnings or lints.** Cycles are allowed; we don't lint
  them.
- **Re-exports.** A file cannot say "everyone who imports me also
  sees what I imported." Non-transitive is non-transitive.
- **Conditional / platform-gated imports.** No `#[cfg]` analogue.
- **Inline modules** (Rust's `mod foo { ... }`). One module = one file.

## Errors

```rust
pub enum LoadError {
    /// `import "<path>";` resolved to no readable file.
    /// E0270.
    ImportFileNotFound { raw_path: String, span: Span },

    /// The imported file parsed with errors. The wrapped
    /// `ParseError`s carry their own spans pointing into the
    /// imported file's source.
    /// E0271.
    ImportParseFailed { path: PathBuf, errors: Vec<ParseError>, span: Span },
}

pub enum HirError {
    ...
    /// Two top-level items in the same namespace have the same name.
    /// Fired by lowering pass 2 (cross-file collision in
    /// `program_directory`) and pass 3 (within-file collision in
    /// `file_scopes[F]`). See spec/14_MODULES.md "Diagnostics and
    /// recovery". E0272.
    DuplicateGlobalSymbol {
        name: String,
        first: Span,
        duplicate: Span,
        namespace: Namespace,
    },
}

/// Which name namespace a collision happened in. Reported in
/// `DuplicateGlobalSymbol` so the diagnostic can say "fn `Foo`"
/// versus "struct `Foo`" precisely.
pub enum Namespace { Types, Values }
```

`UnresolvedName` already exists in HIR lowering and now also fires
when a file uses a name it forgot to import.

### Errors summary

| Code  | Variant                  | Layer    | Trigger                                                                                  |
|-------|--------------------------|----------|------------------------------------------------------------------------------------------|
| E0270 | `ImportFileNotFound`     | Loader   | `import "p";` where `p` resolves to no file                                              |
| E0271 | `ImportParseFailed`      | Loader   | Imported file parsed with errors                                                         |
| E0272 | `DuplicateGlobalSymbol`  | Lowering | Same name defined twice in the same namespace (pass 2 cross-file, or pass 3 within-file) |
| (existing E for unresolved) | `UnresolvedName` | Lowering | Use-site name not in the file's `file_scopes[F]` for the syntactic namespace      |

(Numbers tentative — pick the next free code at implementation time.)

## Note on existing code

- `extern "C"` parsing (`src/parser/parse/syntax.rs:806`) and
  `HirFn { is_extern: true }` (`src/hir/ir.rs:63`) are unchanged.
  This spec is purely the layer that decides *which extern blocks
  are visible to which file*.
- The current single-file driver `oxide-codegen-example.rs`
  changes its CLI from `-f <one-file>` to `<root-file>` and calls
  the loader instead of parsing one file directly. This is a
  driver-shaping change, not a language-design change, and the
  spec does not pin its exact CLI surface beyond "takes a root
  file path."
- The parser-spec hint at `spec/03_PARSER.md:49` ("Natural
  cross-module handles `(ModuleId, ExprId)` when modules land")
  is **superseded** by the global-arena approach. There is no
  "renumber-on-merge" step: arenas are program-wide from the
  start, so every `FnId` / `HAdtId` is unique by construction.
  Cross-module handles aren't needed because the IDs already
  carry no ambiguity; `HirModule` records file ownership as
  bucket metadata, not as part of the ID.
