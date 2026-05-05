# Oxide-in-Oxide: Bootstrap Plan

A self-hosting Oxide compiler, written in Oxide, living at
`example-projects/oxide/`. The Rust-hosted compiler in `src/` is
**stage 0**. This project produces **stage 1**, an Oxide program that
accepts a subset of Oxide (call it "Tier-1") rich enough to include
stage 1's own source. Running stage 1 on its own source produces
**stage 2**, and we declare success when `stage 2 == stage 1` (the
classic bootstrap fixed point).

## Why this is plausible

Today's Oxide gives us:

- Generic structs and generic fns ⇒ `Vec<T>`, arenas, hand-rolled
  hash tables (with a fixed-key hash, since there are no traits).
- `&`/`&mut`, `*const T`/`*mut T`, `ox_transmute<Src, Dst>`,
  `ox_size_of<T>` ⇒ all the typed-pointer plumbing arenas need.
- `for`, `while`, `loop`, `break`, `continue` ⇒ imperative compiler
  loops read like C/Rust.
- Modules with splat imports ⇒ multi-file project.
- `extern "C"` + variadics ⇒ libc bindings already shipped in
  `stdlib/{stdio,stdlib,string,mem}.ox`.

The two language features the host compiler leans on heavily that
Oxide does *not* yet have are **enum-with-payload** and **closures /
fn-pointers**. Both have ergonomic workarounds:

- **Enum payloads → tagged structs.** Each `*Kind` enum becomes a
  struct with a `tag: u32` discriminant plus per-variant payload
  fields. Where payloads vary widely in size, the variant struct is
  allocated on the side via `ox_alloc<VariantStruct>()` and the
  parent stores a `*mut u8` + tag (manual tagged union). Either is a
  mechanical translation, not a design decision.
- **Closures / `dyn Trait` → explicit dispatch.** No `Tapper` trait;
  the driver calls each phase directly.

The remaining absence — `match` — folds into `if tag == TAG_FOO { … }
else if tag == TAG_BAR { … }` chains. Tedious but boring.

Programmers wrote PCC, GCC stage 0, OCaml's `ocamlc`, and TCC in
nothing richer than C. Oxide is strictly more expressive than C in
the dimensions that matter for compilers (generics, slice-typed
pointers, mutability tracking). We are not undermanned for this.

## Subset accepted by stage 1 (Tier-1)

Stage 1 must accept everything its own source uses. Working from the
walkthrough's feature list:

**In Tier-1.**

- Primitives: `i8`–`i64`, `u8`–`u64`, `isize`, `usize`, `bool`,
  unit-by-omission.
- Pointers: `*const T`, `*mut T`, null literal, `&`/`&mut`,
  auto-deref on field access.
- Arrays: `[T; N]`, `*const [T]`, `*mut [T]`, `*const [T; N]`,
  indexing.
- Structs (record + generic record). No tuple/unit structs.
- Generic fns + turbofish + inference.
- `extern "C" { … }` with variadics (`...`).
- `as` casts (the spec/12 set).
- `if` / `else` as expression, `while`, `for`, `loop`, `break`,
  `continue`.
- `import "…";` (splat) with stdlib hardcode + relative resolution.
- Mutability checking (`let` / `let mut`, write through `*mut`).
- `ox_transmute`, `ox_size_of` intrinsics — recognized by file-gate +
  name allowlist exactly as in stage 0.

**Out of Tier-1 (stage 1 rejects with a parse/typeck error).**

- Anything stage 0 itself doesn't yet implement (floats, enum
  payloads, closures, `match`, traits, `unsafe`, `const fn`).
- Layout rarities not used by our own source: `repr(packed)`,
  alignment > 16, etc.

If stage 1 finds it needs a feature its own source doesn't use, we
prune the source rather than expand Tier-1. Keep the diagonal tight.

## Output

Stage 1 emits **textual LLVM IR** to a `.ll` file. Linking shells
out to `clang` (same path stage 0 uses today via `builder/link.rs`).

This sidesteps needing LLVM-C FFI bindings in Oxide. We can revisit
once everything else works and `.bc`/`.o` direct emission becomes
the bottleneck.

## Architecture

Mirror `src/` so the mental map is one-to-one:

```text
example-projects/oxide/
├── SPEC.md                  ← this file
├── main.ox                  ← driver: parse args → phases → IR
├── lexer.ox                 ← source bytes → token stream
├── parser.ox                ← tokens → AST
├── ast.ox                   ← AST node structs + tag constants
├── hir.ox                   ← AST → HIR; name resolution, scopes
├── typeck.ox                ← inference, mutability, generics
├── codegen.ox               ← HIR + typeck → LLVM IR text
├── util/
│   ├── vec.ox               ← Vec<T>
│   ├── strbuf.ox            ← growable byte buffer (≈ String)
│   ├── strmap.ox            ← StringMap<V> (linear-probed hash)
│   ├── arena.ox             ← Arena<T> (id-based, generic)
│   └── io.ox                ← buffered stdout/stderr writer
├── tests/
│   └── *.ox                 ← end-to-end snapshot programs
└── README.md                ← build + run instructions
```

`Vec<T>` and `Arena<T>` are direct enablers; without them every
container is a separate hand-roll. They are `M1`'s sole deliverable.

Tag-based "enum" structs (e.g. `TokenKind`, `ExprKind`) live in
their respective files, not a shared `tags.ox` — the discriminants
are private to each kind.

## Milestones

Each milestone is independently committable and produces something
demoable.

- **M1 — Utility crate.** `vec.ox`, `strbuf.ox`, `strmap.ox`,
  `arena.ox`, `io.ox`. Tests = round-trip snapshots run on the
  *stage 0* compiler.
- **M2 — Lexer.** Every `TokenKind` from `src/lexer/token.rs`.
  Acceptance = lex `example-projects/oxide/lexer.ox` itself with
  zero unknown tokens.
- **M3 — Parser → AST.** Every surface form in spec/03 + spec/08 +
  spec/13 + spec/14 + spec/16 + spec/17. Acceptance = parse the
  whole `example-projects/oxide/` tree.
- **M4 — HIR lowering + scope/name resolution.** Two-namespace
  scopes, splat imports, generics, ADT references. Acceptance =
  no `UnresolvedName` on our own source.
- **M5 — Typeck.** Bidirectional inference over generics; mutability
  rules from spec/11; `as` rules from spec/12; layout intrinsics
  from spec/17. Acceptance = our own source typechecks.
- **M6 — Mono + codegen.** Monomorphize generic instances; emit IR
  for everything in Tier-1. Acceptance = end-to-end on a corpus of
  small programs (`puts`, `fib`, `layout_intrinsics`,
  `socket-server`).
- **M7 — Driver.** Argument parsing, file I/O, `clang` invocation,
  `--emit ir|exe`. Acceptance = `oxide-stage1 hello.ox` produces a
  running binary.
- **M8 — Bootstrap.** Stage 1 compiles its own source. Stage 2
  compiles its own source. `diff stage1.ll stage2.ll == ∅`.

Each milestone gets a sub-spec (`SPEC.M1.md`, `SPEC.M2.md`, …) that
mirrors the host's spec/* style: requirements → design → acceptance
→ errors. We iterate the sub-spec with the human first, then
implement.

## Testing

Per project memory: snapshot-only, Jest-style auto-bless on first
run. Two layers:

1. **Stage-0-driven unit snapshots.** Each Oxide module gets a
   companion `tests/<module>_snapshot.ox` test program that exercises
   it end-to-end and prints a stable representation. Run with the
   host `oxide` binary; output captured to `.snap`.
2. **Stage-1 vs stage-0 differential.** Once M7 lands, every program
   in `example-projects/` (excluding ours) compiles under both stage
   0 and stage 1 and the produced executables behave identically on
   a fixed corpus of inputs.

Snapshot files live under `example-projects/oxide/tests/snapshots/`.

## Open questions (for human review before M1)

The blocking call right now is M1's container shapes — every
later milestone consumes them. Specifically:

1. **Arena ID type.** `Arena<T>` either hands out `usize` indices
   (simple, type-erased) or a generic `Id<T>`-shaped newtype
   (type-safe, requires a phantom-marker pattern that may not be
   expressible in Oxide today — we have no zero-sized phantoms).
   Default proposal: `usize` indices, with per-arena typed wrapper
   structs (`FnId`, `HExprId`) that hold a `usize` field. Mirrors
   what stage 0's `define_index_type!` macro produces, minus the
   strong typing.
2. **Hashing in `StringMap<V>`.** No traits ⇒ the hash function is
   hard-coded to `fnv1a` over byte slices. Keys are owned `StrBuf`
   (compiler symbol tables only ever key by source name).
3. **Ownership / drop.** Oxide has no `Drop`. Containers leak by
   default; we run with the assumption that the compiler process is
   short-lived and the OS reclaims at exit (same trade as stage 0's
   arenas in practice).

Anything controversial there gets resolved on the SPEC.M1.md review;
otherwise M1 starts on those defaults.
