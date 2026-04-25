// AST with ID-based arenas. No `Stmt` type: a block is a list of expressions
// (`items`) plus an optional tail expression. `let`, `if`, `{block}`, and
// `return` are all expression kinds. The grammar restricts `let` to
// block-item position; everything else is unrestricted.

use crate::lexer::Span;
use index_vec::IndexVec;

index_vec::define_index_type! { pub struct ItemId  = u32; }
index_vec::define_index_type! { pub struct ExprId  = u32; }
index_vec::define_index_type! { pub struct BlockId = u32; }
index_vec::define_index_type! { pub struct TypeId  = u32; }

#[derive(Clone, Debug)]
pub struct Module {
    pub items: IndexVec<ItemId, Item>,
    pub exprs: IndexVec<ExprId, Expr>,
    pub blocks: IndexVec<BlockId, Block>,
    pub types: IndexVec<TypeId, Type>,
    pub root_items: Vec<ItemId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    /// Defined function. Body is required by the grammar at item position
    /// (`body` is always `Some` here in well-formed AST).
    Fn(FnDecl),
    /// `extern "C" { ... }` — group of foreign function declarations.
    /// Each child `FnDecl` has `body: None` (no block in the grammar).
    ExternBlock(ExternBlock),
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret_ty: Option<TypeId>,
    /// `Some(_)` for defined fns; `None` for fns inside an `extern` block.
    pub body: Option<BlockId>,
}

#[derive(Clone, Debug)]
pub struct ExternBlock {
    /// ABI string, e.g. `"C"`. Only `"C"` is accepted at parse time in v0.
    pub abi: String,
    pub items: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Ident,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    /// Items are evaluated in source order. The block's *value* comes from
    /// the last item if it carries `has_semi == false`; otherwise the block
    /// has type `()`. Mid-block items with `has_semi == false` are
    /// validated by typeck (must coerce to `()` or `!`); the parser stays
    /// uniform.
    pub items: Vec<BlockItem>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct BlockItem {
    pub expr: ExprId,
    /// `true` iff the source had a `;` after this expression (or the
    /// expression's grammar always carries one — `let …;`).
    pub has_semi: bool,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    IntLit(u64),
    BoolLit(bool),
    CharLit(char),
    StrLit(String),
    Ident(Ident),
    Paren(ExprId),
    Unary {
        op: UnOp,
        expr: ExprId,
    },
    Binary {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Assign {
        op: AssignOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
    },
    Index {
        base: ExprId,
        index: ExprId,
    },
    Field {
        base: ExprId,
        name: Ident,
    },
    Cast {
        expr: ExprId,
        ty: TypeId,
    },
    If {
        cond: ExprId,
        then_block: BlockId,
        else_arm: Option<ElseArm>,
    },
    Block(BlockId),
    /// `return e?` — type `!`. Always parses as an expression so it can
    /// appear in any expression position (`let b: i32 = return 1;`).
    Return(Option<ExprId>),
    /// `let [mut] name [: ty] [= init]` — type `()`. The grammar restricts
    /// this to block-item position; the AST does not.
    Let {
        mutable: bool,
        name: Ident,
        ty: Option<TypeId>,
        init: Option<ExprId>,
    },
    Poison,
}

#[derive(Clone, Debug)]
pub enum ElseArm {
    Block(BlockId),
    If(ExprId),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AssignOp {
    Eq,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Clone, Debug)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    Named(Ident),
    /// `*const T` or `*mut T`. Pointee is recursive — `*const *mut u8`
    /// nests another `Ptr` inside.
    Ptr { mutability: Mutability, pointee: TypeId },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Mutability {
    Const,
    Mut,
}

impl Mutability {
    pub fn as_str(self) -> &'static str {
        match self {
            Mutability::Const => "const",
            Mutability::Mut => "mut",
        }
    }
}
