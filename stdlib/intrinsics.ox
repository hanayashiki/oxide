// intrinsics.ox — bundled compiler intrinsics.
//
// Imported via `import "intrinsics.ox";`. Each entry is body-less and
// recognized by the compiler via a two-gate scanner check (file path ==
// "intrinsics.ox" AND name in the recognized-intrinsics allowlist; see
// `src/hir/lower/scanner.rs::name_to_intrinsic`).
//
// Intrinsics never appear as LLVM symbols — codegen synthesizes the IR
// inline for each call site. Calling these from outside oxide source
// (e.g., declaring them in a user file) is rejected by the HIR scanner
// because the file gate fails.
//
// Naming convention: every function we author in our own bundled
// stdlib carries the `ox_` prefix to distinguish from C-binding files
// (`stdio.ox`, `stdlib.ox`, `string.ox`) whose names must match linker
// symbols. See spec/17_LAYOUT.md §Naming convention.
//
// See spec/17_LAYOUT.md for the full intrinsic specification.

// Bit-copy reinterpret. Matches `core::mem::transmute` semantics:
// the only validity gate is `size_of(Src) == size_of(Dst)` (checked
// per-instance after monomorphization; mismatch fires E0276).
// Aggregates and pointer mutability changes are accepted.
fn ox_transmute<Src, Dst>(x: Src) -> Dst;

// Compile-time-resolved size of `T` in bytes, returned as a runtime
// `usize`. Lowers to a single `i64` constant at the call site. Cannot
// flow into type-level positions (e.g., array lengths) because oxide
// has no const-expr.
fn ox_size_of<T>() -> usize;
