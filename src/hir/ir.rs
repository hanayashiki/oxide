//! HIR — name-resolved IR. AST identifiers are resolved into typed-index
//! handles (`LocalId`, `FnId`); types are kept syntactic (`HirTy::Named`)
//! since real type derivation is typeck's job.

use index_vec::IndexVec;

use crate::parser::ast::{AssignOp, BinOp, Mutability, UnOp};
use crate::reporter::{FileId, Span};

index_vec::define_index_type! { pub struct FnId        = u32; }
index_vec::define_index_type! { pub struct LocalId     = u32; }
index_vec::define_index_type! { pub struct HExprId     = u32; }
index_vec::define_index_type! { pub struct HBlockId    = u32; }
index_vec::define_index_type! { pub struct HAdtId      = u32; }
index_vec::define_index_type! { pub struct VariantIdx  = u32; }
index_vec::define_index_type! { pub struct FieldIdx    = u32; }
index_vec::define_index_type! { pub struct HTyParamId   = u32; }
index_vec::define_index_type! { pub struct ConstId     = u32; }

/// Top-level HIR. Owns globally-unique arenas — every `FnId` /
/// `HAdtId` / `LocalId` / `HExprId` / `HBlockId` is unique program-
/// wide. Per-file structure lives in `modules`; each `HirModule`
/// records which IDs belong to that file but doesn't store the items
/// itself. See spec/14_MODULES.md.
#[derive(Clone, Debug)]
pub struct HirProgram {
    pub fns: IndexVec<FnId, HirFn>,
    pub adts: IndexVec<HAdtId, HirAdt>,
    /// All `const` items in the program. Allocated at scanner prescan;
    /// referenced from use sites via `HirExprKind::Const(ConstId)`. See
    /// spec/18_CONST.md.
    pub consts: IndexVec<ConstId, HirConstItem>,
    pub locals: IndexVec<LocalId, HirLocal>,
    pub exprs: IndexVec<HExprId, HirExpr>,
    pub blocks: IndexVec<HBlockId, HirBlock>,
    /// All type parameters declared by all fns in the program, keyed by
    /// `HTyParamId`. Each entry's `owner` and `idx_in_owner` together
    /// place it within its declaring fn's `generic_params` list.
    /// See spec/16_GENERIC.md §HIR.
    pub ty_params: IndexVec<HTyParamId, TyParamInfo>,

    /// One `HirModule` per loaded file, indexed by `FileId`.
    pub modules: IndexVec<FileId, HirModule>,
    /// The root file the driver was invoked on.
    pub root: FileId,
}

impl HirProgram {
    /// Convenience: top-level fns of the root module, the iteration
    /// order callers historically used. Same shape as the old
    /// `HirModule.root_fns`.
    pub fn root_fns(&self) -> &[FnId] {
        &self.modules[self.root].root_fns
    }

    /// Convenience: top-level ADTs of the root module.
    pub fn root_adts(&self) -> &[HAdtId] {
        &self.modules[self.root].root_adts
    }

    /// Convenience: top-level consts of the root module.
    pub fn root_consts(&self) -> &[ConstId] {
        &self.modules[self.root].root_consts
    }
}

/// Per-file HIR metadata. Records which globally-allocated IDs belong
/// to this file; the items themselves live in the `HirProgram`'s
/// arenas.
#[derive(Clone, Debug)]
pub struct HirModule {
    pub file: FileId,
    /// Every `FnId` whose definition lives in this file (including
    /// extern-block children). Useful for per-file walks.
    pub fns: Vec<FnId>,
    /// Every `HAdtId` declared in this file.
    pub adts: Vec<HAdtId>,
    /// Every `ConstId` declared in this file.
    pub consts: Vec<ConstId>,
    /// Top-level fns in source order. Today this equals `fns`; the
    /// distinction is reserved for nested item shapes.
    pub root_fns: Vec<FnId>,
    /// Top-level ADTs in source order.
    pub root_adts: Vec<HAdtId>,
    /// Top-level consts in source order. v0: equals `consts` since
    /// const items are top-level only. See spec/18_CONST.md.
    pub root_consts: Vec<ConstId>,
    pub span: Span,
}

/// Algebraic data type definition. v0 is record-struct only; the
/// variants-list shape is the rustc-style umbrella so enums and unions
/// fit by adding variants/AdtKind without reshaping.
#[derive(Clone, Debug, Default)]
pub struct HirAdt {
    pub name: String,
    pub kind: AdtKind,
    /// Type parameters in declaration order. Empty for non-generic
    /// structs *and* for `struct G<>` (matches Rust). Each `HTyParamId`
    /// is a global index into `HirProgram.ty_params` with `owner:
    /// TyParamOwner::Adt(_)`. See spec/16_GENERIC.md §HIR (extension).
    pub generic_params: Vec<HTyParamId>,
    pub variants: IndexVec<VariantIdx, HirVariant>,
    /// Source span — origin file is `span.file`.
    pub span: Span,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum AdtKind {
    #[default]
    Struct,
    // Enum, Union — future
}

#[derive(Clone, Debug)]
pub struct HirVariant {
    /// `None` for the implicit unnamed variant of a struct.
    pub name: Option<String>,
    pub fields: IndexVec<FieldIdx, HirField>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct HirField {
    pub name: String,
    pub ty: HirTy,
    pub span: Span,
}

/// Which item declared a type parameter — fn or ADT. The substitution
/// machinery (`substitute_ty`, `resolve_fully`, mangler) doesn't dispatch
/// on owner kind; it just looks up `pid` in a `HashMap<ParamId, TyId>`.
/// The owner tag is consulted only at diagnostic-rendering sites.
/// See spec/16_GENERIC.md §HIR (extension).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TyParamOwner {
    Fn(FnId),
    Adt(HAdtId),
}

/// Metadata for one type parameter declared by some fn or ADT.
/// Self-contained: `TyKind::Param(tpid)` downstream needs only the
/// `HTyParamId` to identify ownership, since `owner` is recoverable
/// here. See spec/16_GENERIC.md §HIR.
#[derive(Clone, Debug)]
pub struct TyParamInfo {
    /// Item that declared this type param.
    pub owner: TyParamOwner,
    /// Position within `owner`'s `generic_params` list (0-indexed).
    pub idx_in_owner: u32,
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, Default)]
pub struct HirFn {
    pub name: String,
    /// Type parameters in declaration order. Empty for non-generic
    /// fns *and* for fns declared with empty brackets `<>` (matches
    /// Rust). Each `HTyParamId` is a global index into
    /// `HirProgram.ty_params`. See spec/16_GENERIC.md §HIR.
    pub generic_params: Vec<HTyParamId>,
    pub params: Vec<LocalId>,
    /// `None` when source omits `-> T` — typeck defaults to unit.
    pub ret_ty: Option<HirTy>,
    /// `Some(_)` for defined fns; `None` for foreign fns declared in an
    /// `extern "C"` block, **or** for compiler-recognized intrinsics
    /// declared bodyless in `stdlib/intrinsics.ox` (see `intrinsic`).
    pub body: Option<HBlockId>,
    /// `true` if this fn was declared inside an `extern "C"` block —
    /// linker resolves the symbol against an external object file.
    pub is_extern: bool,
    /// `true` if the fn signature ends in `, ...` — a C-ABI variadic.
    /// The parser enforces `is_variadic ⇒ is_extern`.
    pub is_variadic: bool,
    /// `Some(_)` for compiler-recognized intrinsics. Set by the HIR
    /// scanner when both gates pass: (a) the fn is declared in
    /// `stdlib/intrinsics.ox`, (b) its name matches an entry in the
    /// `name_to_intrinsic` allowlist. Read by the HIR scanner only to
    /// decide whether to emit `BodylessFnOutsideExtern` (E0209) for a
    /// body-less fn at module scope. Mono uses it to decide which
    /// `InstanceOperation` to stamp; codegen reads from the stamped
    /// operation, not from this field. See spec/17_LAYOUT.md §Intrinsic
    /// recognition.
    pub intrinsic: Option<Intrinsic>,
    /// Source span — origin file is `span.file`.
    pub span: Span,
}

/// Compiler-recognized intrinsic kind. The variant names are the
/// Rust-side enum (no `ox_` prefix); the Oxide-source name lives in
/// the `HirFn.name` field. Mapping from name → variant is
/// `name_to_intrinsic` in the scanner — that function is the single
/// source of truth for the allowlist; do not introduce a parallel
/// `INTRINSIC_NAMES` constant. See spec/17_LAYOUT.md §Intrinsic
/// recognition.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Intrinsic {
    /// `fn ox_transmute<Src, Dst>(x: Src) -> Dst` — bit-copy reinterpret.
    Transmute,
    /// `fn ox_size_of<T>() -> usize` — runtime constant size in bytes.
    SizeOf,
}

#[derive(Clone, Debug)]
pub struct HirLocal {
    pub name: String,
    pub mutable: bool,
    /// `None` ⇒ no annotation in source; typeck creates an inference var.
    pub ty: Option<HirTy>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct HirBlock {
    /// Items in source order. The block's *value* comes from the last item
    /// if `has_semi == false`; otherwise the block has type `()`. Mirror
    /// of `ast::Block`. Mid-block items with `has_semi == false` are
    /// validated by typeck (must coerce to `()` or `!`).
    pub items: Vec<HBlockItem>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct HBlockItem {
    pub expr: HExprId,
    pub has_semi: bool,
}

/// A `const` item: name, type annotation, literal value. Per
/// spec/18_CONST.md, the RHS is one of `IntLit`/`BoolLit`/`CharLit`/
/// `StrLit`; the parser pins this and `HirConstValue` mirrors it.
/// Annotation is mandatory (no inference); typeck verifies that the
/// literal kind matches.
#[derive(Clone, Debug)]
pub struct HirConstItem {
    pub name: String,
    pub ty: HirTy,
    pub value: HirConstValue,
    pub span: Span,
}

/// Literal RHS of a const item. Variants line up 1:1 with the four
/// allowed `ExprKind`/`HirExprKind` literal kinds. `Char(u8)` matches
/// `HirExprKind::CharLit(u8)` — chars are bytes in v0; out-of-range
/// chars never reach here because the lowering path for a const RHS
/// runs after the same `lower_char_lit` rejection used for body
/// expressions.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum HirConstValue {
    Int(u64),
    Bool(bool),
    Char(u8),
    Str(String),
}

#[derive(Clone, Debug)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub span: Span,
    /// Whether this expression refers to a memory location ("place" in
    /// rustc terminology, "lvalue" in C). Place-ness is purely syntactic —
    /// derived from `kind` and the place-ness of children — so we cache
    /// it at construction time in `lower` rather than re-deriving it on
    /// each lookup.
    ///
    /// Rules (see spec/08_ADT.md "Place expressions"):
    ///   - `Local(_)` → place.
    ///   - `Field { base, .. }` → place iff `base` is place.
    ///   - `Unresolved(_) | Poison` → place (suppress cascading errors;
    ///     diagnostics already filed at HIR/typeck for the underlying issue).
    ///   - everything else → not place.
    ///
    /// `Unary { Deref, .. }` is a place per 07_POINTER §HIR; `Index` gains
    /// it when array spec lands.
    pub is_place: bool,
}

#[derive(Clone, Debug)]
pub enum HirExprKind {
    /// Integer literal — typed by typeck (default `i32`).
    IntLit(u64),
    BoolLit(bool),
    /// Char literals are bytes (`u8`). C-style; matches LLVM `i8`.
    CharLit(u8),
    /// String literal data carried through. Typeck rejects in v0 since
    /// strings need pointers/arrays.
    StrLit(String),
    /// `null` — typed null pointer literal. Typeck assigns
    /// `*mut α` (fresh inference var). See spec/07_POINTER.md
    /// "Null literal".
    Null,
    /// Resolved use of a let-binding or function parameter.
    Local(LocalId),
    /// Resolved use of a module-level function.
    Fn(FnId),
    /// Resolved use of a module-level `const` item. The value lives in
    /// `HirProgram.consts[cid].value`. Not a place — codegen
    /// materializes the literal inline at every use site. See
    /// spec/18_CONST.md.
    Const(ConstId),
    /// Name lookup failed; preserved as a string for diagnostics.
    Unresolved(String),
    Unary {
        op: UnOp,
        expr: HExprId,
    },
    Binary {
        op: BinOp,
        lhs: HExprId,
        rhs: HExprId,
    },
    Assign {
        op: AssignOp,
        target: HExprId,
        rhs: HExprId,
    },
    Call {
        callee: HExprId,
        args: Vec<HExprId>,
        /// Turbofish type arguments (`name::<T, U>(args)`). Empty for
        /// the common `name(args)` case *and* for `name::<>(args)`
        /// (both collapse here, matching the AST behavior).
        /// Resolution (turbofish vs. inferred) is typeck's job —
        /// see spec/16_GENERIC.md §Typeck rules.
        type_args: Vec<HirTy>,
    },
    Index {
        base: HExprId,
        index: HExprId,
    },
    /// Field access — `name` is unresolved at HIR time; typeck looks it up
    /// once `base`'s type is inferred.
    Field {
        base: HExprId,
        name: String,
    },
    /// `Name { f: expr, ... }` or `Name::<T> { f: expr, ... }` — record
    /// struct literal. The type-name has been resolved (`adt`) but field
    /// names stay as strings; typeck validates the field set and types
    /// each value expression against the declared field type.
    ///
    /// `type_args` is the optional turbofish args. Empty for the inferred
    /// form `Name { ... }` and for the empty-turbofish `Name::<> { ... }`
    /// (both collapse). Resolution (turbofish vs. inferred) is typeck's
    /// job — see spec/16_GENERIC.md §Typeck rules (extension).
    StructLit {
        adt: HAdtId,
        type_args: Vec<HirTy>,
        fields: Vec<HirStructLitField>,
    },
    /// `&expr` / `&mut expr`. Operand validated to be a place at lower
    /// time (errors as `AddrOfNonPlace` if not). Result is `*const T` /
    /// `*mut T` per the operator's mutability. See spec/10_ADDRESS_OF.md.
    AddrOf {
        mutability: Mutability,
        expr: HExprId,
    },
    /// `[a, b, c]` (Elems) or `[init; N]` (Repeat). N has been
    /// extracted to a `HirConst` at HIR-lower time. The parser rejects
    /// non-`IntLit` shapes in the length slot, so the extraction is
    /// total. See spec/09_ARRAY.md.
    ArrayLit(HirArrayLit),
    Cast {
        expr: HExprId,
        ty: HirTy,
    },
    If {
        cond: HExprId,
        then_block: HBlockId,
        else_arm: Option<HElseArm>,
    },
    Block(HBlockId),
    /// `return e?` — type `!`. Operand was already lowered; this is the
    /// expression node.
    Return(Option<HExprId>),
    /// Unified loop. Covers all three surface forms (`while` / `loop` /
    /// C-style `for`). Each header slot is populated only when the
    /// surface form supplied it; see spec/13_LOOPS.md "Design overview".
    /// `cond.is_some()` and `has_break` together drive the structural
    /// typing rule (`()` if cond is some, `!` if no break, fresh-infer
    /// otherwise). `source` is diagnostic / pretty-print only.
    Loop {
        init: Option<HExprId>,
        cond: Option<HExprId>,
        update: Option<HExprId>,
        body: HBlockId,
        has_break: bool,
        source: LoopSource,
    },
    /// `break expr?` — type `!`. Operand carries the value the
    /// enclosing loop expression evaluates to. HIR-lower validates we're
    /// inside a loop (emits `BreakOutsideLoop` otherwise); typeck
    /// coerces `expr`'s type into the innermost loop's result-type slot.
    Break {
        expr: Option<HExprId>,
    },
    /// `continue` — type `!`. No operand in v0 (no labels). HIR-lower
    /// validates we're inside a loop.
    Continue,
    /// `let` binding. The `local` was already pushed to the locals arena
    /// and the current block's scope when lowering. This expression's own
    /// type is `()`.
    Let {
        local: LocalId,
        init: Option<HExprId>,
    },
    /// Recovery placeholder — used for AST `Poison` and char-out-of-range.
    Poison,
}

/// Records which surface keyword produced a `HirExprKind::Loop`. Used
/// only by HIR pretty-print and any diagnostic that wants to flavour
/// its wording — does **not** drive the typing rule (see spec/13_LOOPS.md
/// "Typing rule is structural, not source-driven").
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoopSource {
    While,
    Loop,
    For,
}

#[derive(Clone, Debug)]
pub enum HElseArm {
    Block(HBlockId),
    If(HExprId),
}

#[derive(Clone, Debug)]
pub struct HirStructLitField {
    pub name: String,
    pub value: HExprId,
    pub span: Span,
}

/// Array literal — element list or repeat-with-length form.
#[derive(Clone, Debug)]
pub enum HirArrayLit {
    /// `[a, b, c]` — element list. Length is `elems.len()`. Empty `[]`
    /// reaches HIR as `Elems(vec![])`; typeck is responsible for the
    /// element-type inference question (it needs a context type).
    Elems(Vec<HExprId>),
    /// `[init; N]` — `init` repeated N times. `N` has been extracted from
    /// the AST length expression to a `HirConst::Lit(u64)`. Non-`IntLit`
    /// shapes are rejected at parse time, so `HirConst::Error` is
    /// unreachable in v0; the variant survives for forward-compatibility
    /// with a future ICE evaluator.
    Repeat { init: HExprId, len: HirConst },
}

/// Type-level constant value, extracted from a length-position AST
/// expression at HIR-lower time. v0 only carries `Lit(u64)` (from a bare
/// `IntLit` token) or `Error`. Future const-generics work adds more
/// variants without changing the `Lit`/`Error` cases.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum HirConst {
    Lit(u64),
    Error,
}

#[derive(Clone, Debug)]
pub struct HirTy {
    pub kind: HirTyKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HirTyKind {
    /// A type-position name that didn't resolve to a user-defined ADT in
    /// the type namespace. Typeck does the primitive-name lookup and
    /// either resolves it to a primitive `Ty` or emits an unknown-type
    /// error.
    Named(String),
    /// Resolved use of a user-defined ADT (struct/enum/union).
    /// Args carry the type-arguments supplied at this use site:
    /// `Adt(haid, [])` for non-generic ADTs and bare names of generic
    /// ADTs whose args are inferred elsewhere (typeck handles that),
    /// `Adt(haid, [t1, t2])` for `Foo<t1, t2>`. **Arity invariant**:
    /// `args.len() == hir.adts[haid].generic_params.len()` always —
    /// see spec/16_GENERIC.md §Typeck rules (extension).
    Adt(HAdtId, Vec<HirTy>),
    /// A reference to a generic type parameter declared by the
    /// enclosing fn. Resolved at HIR-lower time when the source name
    /// matches one of the fn's `generic_params`. Typeck (Phase C) maps
    /// this to `TyKind::Param(tpid)`. See spec/16_GENERIC.md §HIR.
    Param(HTyParamId),
    /// `*const T` / `*mut T`. Pointee is `Box`ed for recursion.
    Ptr {
        mutability: Mutability,
        pointee: Box<HirTy>, // FIXME: should intern HirTy.
    },
    /// `[T; N]` (sized — `len: Some(_)`) or `[T]` (unsized — `len: None`).
    /// The unified shape mirrors the `[T] ≡ [T; ∞]` mental model directly:
    /// the `Option` discriminates length-known vs length-unknown without
    /// introducing a separate kind. `Array(_, None)` is rejected as a
    /// value type at typeck (E0261 `UnsizedArrayAsValue`) and is only
    /// valid behind a pointer (`*const [T]` / `*mut [T]`).
    Array(Box<HirTy>, Option<HirConst>),
    /// `[extern "C"]? fn(T1, T2[, ...]) -> R`. Param names from AST are
    /// dropped at lower per spec/19_FN_PTR.md §7.1; only the param
    /// types reach typeck interning. `ret_ty: None` means the source
    /// omitted `-> R`, which lowers to `()` at typeck.
    Fn {
        is_extern_c: bool,
        params: Vec<HirTy>,
        is_variadic: bool,
        ret_ty: Option<Box<HirTy>>,
    },
    /// Recovery placeholder for malformed type positions.
    Error,
}

/// Which name namespace a collision happened in. See
/// spec/14_MODULES.md "Name resolution — two namespaces".
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Namespace {
    Types,
    Values,
}
