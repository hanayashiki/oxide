//! HIR — name-resolved IR. AST identifiers are resolved into typed-index
//! handles (`LocalId`, `FnId`); types are kept syntactic (`HirTy::Named`)
//! since real type derivation is typeck's job.

use index_vec::IndexVec;

use crate::lexer::Span;
use crate::parser::ast::{AssignOp, BinOp, UnOp};

index_vec::define_index_type! { pub struct FnId     = u32; }
index_vec::define_index_type! { pub struct LocalId  = u32; }
index_vec::define_index_type! { pub struct HExprId  = u32; }
index_vec::define_index_type! { pub struct HBlockId = u32; }

#[derive(Clone, Debug)]
pub struct HirModule {
    pub fns: IndexVec<FnId, HirFn>,
    pub locals: IndexVec<LocalId, HirLocal>,
    pub exprs: IndexVec<HExprId, HirExpr>,
    pub blocks: IndexVec<HBlockId, HirBlock>,
    pub root_fns: Vec<FnId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct HirFn {
    pub name: String,
    pub params: Vec<LocalId>,
    /// `None` when source omits `-> T` — typeck defaults to unit.
    pub ret_ty: Option<HirTy>,
    pub body: HBlockId,
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
    /// Expressions evaluated in order; values discarded.
    pub items: Vec<HExprId>,
    /// Optional value-producing expression at the end of the block.
    pub tail: Option<HExprId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub span: Span,
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
    /// Field access — name unresolved in v0 (no struct support yet).
    Field {
        base: HExprId,
        name: String,
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
pub struct HirTy {
    pub kind: HirTyKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HirTyKind {
    /// A type-position name as written in source (e.g. "i32", "MyStruct").
    /// Typeck does the primitive-name lookup and resolves to its
    /// internal `Ty`.
    Named(String),
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
}

impl HirError {
    pub fn span(&self) -> &Span {
        match self {
            Self::UnresolvedName { span, .. } | Self::CharOutOfRange { span, .. } => span,
            Self::DuplicateFn { dup, .. } => dup,
        }
    }
}
