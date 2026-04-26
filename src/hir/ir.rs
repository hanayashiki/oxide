//! HIR — name-resolved IR. AST identifiers are resolved into typed-index
//! handles (`LocalId`, `FnId`); types are kept syntactic (`HirTy::Named`)
//! since real type derivation is typeck's job.

use index_vec::IndexVec;

use crate::lexer::Span;
use crate::parser::ast::{AssignOp, BinOp, Mutability, UnOp};

index_vec::define_index_type! { pub struct FnId        = u32; }
index_vec::define_index_type! { pub struct LocalId     = u32; }
index_vec::define_index_type! { pub struct HExprId     = u32; }
index_vec::define_index_type! { pub struct HBlockId    = u32; }
index_vec::define_index_type! { pub struct HAdtId      = u32; }
index_vec::define_index_type! { pub struct VariantIdx  = u32; }
index_vec::define_index_type! { pub struct FieldIdx    = u32; }

#[derive(Clone, Debug)]
pub struct HirModule {
    pub fns: IndexVec<FnId, HirFn>,
    pub adts: IndexVec<HAdtId, HirAdt>,
    pub locals: IndexVec<LocalId, HirLocal>,
    pub exprs: IndexVec<HExprId, HirExpr>,
    pub blocks: IndexVec<HBlockId, HirBlock>,
    pub root_fns: Vec<FnId>,
    pub root_adts: Vec<HAdtId>,
    pub span: Span,
}

/// Algebraic data type definition. v0 is record-struct only; the
/// variants-list shape is the rustc-style umbrella so enums and unions
/// fit by adding variants/AdtKind without reshaping.
#[derive(Clone, Debug)]
pub struct HirAdt {
    pub name: String,
    pub kind: AdtKind,
    pub variants: IndexVec<VariantIdx, HirVariant>,
    pub span: Span,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AdtKind {
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

#[derive(Clone, Debug)]
pub struct HirFn {
    pub name: String,
    pub params: Vec<LocalId>,
    /// `None` when source omits `-> T` — typeck defaults to unit.
    pub ret_ty: Option<HirTy>,
    /// `Some(_)` for defined fns; `None` for foreign fns declared in an
    /// `extern "C"` block. The two correlate today (`body.is_none()` iff
    /// `is_extern`), but they're distinct fields so future cases that
    /// have no body for non-extern reasons (trait methods, etc.) don't
    /// require a refactor.
    pub body: Option<HBlockId>,
    /// `true` if this fn was declared inside an `extern "C"` block —
    /// linker resolves the symbol against an external object file.
    pub is_extern: bool,
    pub span: Span,
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
    /// `Unary { Deref, .. }` and `Index { .. }` will gain producer/projection
    /// arms when their feature specs land (07_POINTER §5 for deref;
    /// future array spec for index). Today they're not place.
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
    /// Resolved use of a let-binding or function parameter.
    Local(LocalId),
    /// Resolved use of a module-level function.
    Fn(FnId),
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
    /// `Name { f: expr, ... }` — record struct literal. The type-name has
    /// been resolved (`adt`) but field names stay as strings; typeck
    /// validates the field set and types each value expression against
    /// the declared field type.
    StructLit {
        adt: HAdtId,
        fields: Vec<HirStructLitField>,
    },
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
    Adt(HAdtId),
    /// `*const T` / `*mut T`. Pointee is `Box`ed for recursion.
    Ptr {
        mutability: Mutability,
        pointee: Box<HirTy>, // FIXME: should intern HirTy.
    },
    /// Recovery placeholder for malformed type positions.
    Error,
}

#[derive(Clone, Debug)]
pub enum HirError {
    /// Value-namespace lookup failed.
    UnresolvedName { name: String, span: Span },
    /// Two `fn`s in one module share a name.
    DuplicateFn {
        name: String,
        first: Span,
        dup: Span,
    },
    /// `'\xHH'` whose value exceeds `u8::MAX`, or a multibyte char literal.
    CharOutOfRange { ch: char, span: Span },
    /// Two ADTs (struct/enum/union) in one module share a name.
    DuplicateAdt {
        name: String,
        first: Span,
        dup: Span,
    },
    /// Two fields in one ADT share a name.
    DuplicateField {
        adt: String,
        name: String,
        first: Span,
        dup: Span,
    },
    /// Type-namespace lookup failed in a struct-literal position.
    UnresolvedAdt { name: String, span: Span },
    /// Left-hand side of `=` (or compound assign) is not a place
    /// expression. See spec/08_ADT.md "Place expressions and `is_place`".
    InvalidAssignTarget { span: Span },
}

impl HirError {
    pub fn span(&self) -> &Span {
        match self {
            Self::UnresolvedName { span, .. }
            | Self::CharOutOfRange { span, .. }
            | Self::UnresolvedAdt { span, .. }
            | Self::InvalidAssignTarget { span } => span,
            Self::DuplicateFn { dup, .. }
            | Self::DuplicateAdt { dup, .. }
            | Self::DuplicateField { dup, .. } => dup,
        }
    }
}
