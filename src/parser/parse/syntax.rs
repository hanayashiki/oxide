//! The actual grammar. Combinators only — no input-stream construction, no
//! state plumbing beyond `push_*` calls, no error post-processing.

use chumsky::pratt::*;
use chumsky::prelude::*;

use crate::lexer::TokenKind;
use crate::parser::ast::*;

use super::extra::{Extra, MapExtraExt, OMapExtra, OValueInput};

/// One Pratt rung per precedence level. The op-parser yields the resolved
/// `BinOp`/`AssignOp`/`UnOp` so the fold closure doesn't need to inspect tokens.
macro_rules! binop_level {
    ($assoc:expr, $($tok:expr => $op:expr),+ $(,)?) => {
        infix(
            $assoc,
            choice(($(just($tok).to($op),)+)),
            |lhs, op: BinOp, rhs, e: &mut OMapExtra<'_, '_, I>| {
                e.push_expr(ExprKind::Binary { op, lhs, rhs })
            },
        )
    };
}
macro_rules! assign_level {
    ($($tok:expr => $op:expr),+ $(,)?) => {
        infix(
            right(1),
            choice(($(just($tok).to($op),)+)),
            |lhs, op: AssignOp, rhs, e: &mut OMapExtra<'_, '_, I>| {
                e.push_expr(ExprKind::Assign { op, lhs, rhs })
            },
        )
    };
}
macro_rules! prefix_level {
    ($prec:expr, $($tok:expr => $op:expr),+ $(,)?) => {
        prefix(
            $prec,
            choice(($(just($tok).to($op),)+)),
            |op: UnOp, rhs, e: &mut OMapExtra<'_, '_, I>| {
                e.push_expr(ExprKind::Unary { op, expr: rhs })
            },
        )
    };
}

pub(super) fn ident_parser<'a, I>() -> impl Parser<'a, I, Ident, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    select! { TokenKind::Ident(name) => name }
        .map_with(|name, e| Ident {
            name,
            span: e.lex_span(),
        })
        .labelled("identifier")
}

/// Build the type parser parameterized by an expression parser. The expr
/// is threaded in only so callers outside `expr_parser` (item-level
/// parsers like `params_parser`, `ret_ty_parser`, `struct_item_parser`)
/// share the same recursive-expr handle. The length slot itself does
/// **not** consume from the expression parser — see
/// `int_lit_length_parser` for the rule.
///
/// Callers outside `expr_parser` construct an `expr_parser` first and
/// pass it in via this entry point so the cast slot (`x as [T; N]`)
/// keeps recursing through the same expression handle.
pub(super) fn type_parser<'a, I, PE>(expr: PE) -> impl Parser<'a, I, TypeId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    // The `expr` parameter is currently unused inside this function — kept
    // in the signature to preserve the caller-site convention that "build
    // an expr first, then a type". Bind to `_` to silence the warning.
    let _ = expr;
    recursive(|ty| {
        let named = ident_parser().map_with(|name, e| e.push_type(TypeKind::Named(name)));

        // `*const T` / `*mut T`. Right-recursive on `ty` for nesting.
        let mutability = choice((
            just(TokenKind::KwConst).to(Mutability::Const),
            just(TokenKind::KwMut).to(Mutability::Mut),
        ));
        let ptr = just(TokenKind::Star)
            .ignore_then(mutability)
            .then(ty.clone())
            .map_with(|(mutability, pointee), e| {
                e.push_type(TypeKind::Ptr {
                    mutability,
                    pointee,
                })
            });

        // `[T; N]` (sized) or `[T]` (unsized). The length slot accepts
        // **only** a bare `IntLit` token — see spec/09_ARRAY.md "Length
        // literal extraction". Anything richer (parens, casts, idents,
        // binary ops) is a parse error here and will surface as a
        // chumsky "expected `]`" diagnostic. `[T]` (no `;`) lowers to
        // `Array { len: None }`.
        let array = ty
            .clone()
            .then(
                just(TokenKind::Semi)
                    .ignore_then(int_lit_length_parser())
                    .or_not(),
            )
            .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
            .map_with(|(elem, len), e| e.push_type(TypeKind::Array { elem, len }));

        choice((ptr, array, named)).labelled("type")
    })
}

/// Length-slot parser used by `[T; N]` and `[init; N]`. v0 only accepts
/// a single `Int(n)` token here — no const-expression evaluator. The
/// captured `n` is wrapped in an `ExprKind::IntLit` so the AST shape
/// (`Option<ExprId>` for type lengths, `ExprId` for repeat-literal
/// lengths) is unchanged. See spec/09_ARRAY.md "Length literal
/// extraction".
fn int_lit_length_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    select! { TokenKind::Int(n) => n }
        .map_with(|n, e| e.push_expr(ExprKind::IntLit(n)))
        .labelled("integer literal")
}

pub(super) fn expr_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    recursive(|expr| {
        // Top-of-expression forms — sit above the Pratt tower so their
        // operands can be full expressions (`return e + 1` parses as
        // `return (e + 1)`). All produce type `!` at typeck.
        let return_form = return_expr_parser(expr.clone());
        let break_form = break_expr_parser(expr.clone());
        let continue_form = continue_expr_parser();

        // Atoms — tried in priority order. `struct_lit` precedes
        // `ident_expr` so the bare-ident fallback only fires when the
        // following tokens don't shape up to a field list.
        let block = block_parser_inner(expr.clone());
        let if_expr = if_parser(expr.clone(), block.clone());
        let while_expr = while_parser(expr.clone(), block.clone());
        let loop_expr = loop_parser(block.clone());
        let for_expr = for_parser(expr.clone(), block.clone());
        let atom = choice((
            int_lit_parser(),
            bool_lit_parser(),
            char_lit_parser(),
            str_lit_parser(),
            null_lit_parser(),
            if_expr,
            while_expr,
            loop_expr,
            for_expr,
            block_expr_parser(block),
            paren_parser(expr.clone()),
            array_lit_parser(expr.clone()),
            struct_lit_parser(expr.clone()),
            ident_expr_parser(),
        ));

        // Postfix tower: call, index, field — left-folded onto `atom`.
        let with_postfix = postfix_parser(atom, expr.clone());

        // Cast slot needs a type parser; build one from the recursive
        // expr handle so array-length slots inside cast types (e.g.
        // `x as [T; 3]`) can recurse through expr correctly.
        let ty_in_expr = type_parser(expr.clone());

        let pratt_expr = with_postfix.pratt((
            prefix_level!(13,
                TokenKind::Minus => UnOp::Neg,
                TokenKind::Bang => UnOp::Not,
                TokenKind::Tilde => UnOp::BitNot,
                // `*expr` — pointer deref. Position-disambiguated from
                // binary `*` (Mul, level 11) by the Pratt builder.
                // See spec/07_POINTER.md "Deref operator".
                TokenKind::Star => UnOp::Deref,
            ),
            // `&expr` / `&mut expr` — same precedence as the other prefix
            // unary ops. `Amp` here is the prefix path; the infix `Amp`
            // (binary BitAnd, level 8) is disambiguated by Pratt position.
            // See spec/10_ADDRESS_OF.md "Token disambiguation".
            prefix(
                13,
                just(TokenKind::Amp).ignore_then(just(TokenKind::KwMut).or_not().map(|m| {
                    if m.is_some() {
                        Mutability::Mut
                    } else {
                        Mutability::Const
                    }
                })),
                |mutability: Mutability, rhs, e: &mut OMapExtra<'_, '_, I>| {
                    e.push_expr(ExprKind::AddrOf {
                        mutability,
                        expr: rhs,
                    })
                },
            ),
            postfix(
                12,
                just(TokenKind::KwAs).ignore_then(ty_in_expr.clone()),
                |lhs, ty, e: &mut OMapExtra<'_, '_, I>| {
                    e.push_expr(ExprKind::Cast { expr: lhs, ty })
                },
            ),
            binop_level!(left(11),
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Rem,
            ),
            binop_level!(left(10),
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
            ),
            binop_level!(left(9),
                TokenKind::Shl => BinOp::Shl,
                TokenKind::Shr => BinOp::Shr,
            ),
            binop_level!(left(8), TokenKind::Amp => BinOp::BitAnd),
            binop_level!(left(7), TokenKind::Caret => BinOp::BitXor),
            binop_level!(left(6), TokenKind::Pipe => BinOp::BitOr),
            binop_level!(left(5),
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Ge => BinOp::Ge,
            ),
            binop_level!(left(4),
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::Ne => BinOp::Ne,
            ),
            binop_level!(left(3), TokenKind::AndAnd => BinOp::And),
            binop_level!(left(2), TokenKind::OrOr => BinOp::Or),
            assign_level!(
                TokenKind::Eq => AssignOp::Eq,
                TokenKind::PlusEq => AssignOp::Add,
                TokenKind::MinusEq => AssignOp::Sub,
                TokenKind::StarEq => AssignOp::Mul,
                TokenKind::SlashEq => AssignOp::Div,
                TokenKind::PercentEq => AssignOp::Rem,
                TokenKind::AmpEq => AssignOp::BitAnd,
                TokenKind::PipeEq => AssignOp::BitOr,
                TokenKind::CaretEq => AssignOp::BitXor,
                TokenKind::ShlEq => AssignOp::Shl,
                TokenKind::ShrEq => AssignOp::Shr,
            ),
        ));

        choice((return_form, break_form, continue_form, pratt_expr)).labelled("expression")
    })
}

// === expr_parser sub-parsers =================================================
//
// Each helper below contributes one piece of the expression atom layer.
// `expr_parser`'s body reads as the structural overview; details live here.
// Leaf literals are extracted too — they're tiny but the symmetry keeps the
// atom alternation in `expr_parser` declarative.

fn int_lit_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    select! { TokenKind::Int(n) => n }.map_with(|n, e| e.push_expr(ExprKind::IntLit(n)))
}

fn bool_lit_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    select! { TokenKind::Bool(b) => b }.map_with(|b, e| e.push_expr(ExprKind::BoolLit(b)))
}

fn char_lit_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    select! { TokenKind::Char(c) => c }.map_with(|c, e| e.push_expr(ExprKind::CharLit(c)))
}

fn str_lit_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    select! { TokenKind::Str(s) => s }.map_with(|s, e| e.push_expr(ExprKind::StrLit(s)))
}

/// `null` — typed null pointer literal. See spec/07_POINTER.md
/// "Null literal".
fn null_lit_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    just(TokenKind::KwNull).map_with(|_, e| e.push_expr(ExprKind::Null))
}

fn ident_expr_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    ident_parser().map_with(|id, e| e.push_expr(ExprKind::Ident(id)))
}

/// `(expr)` — single-expression group. Has chumsky `nested_delimiters`
/// recovery so a malformed inner expression doesn't cascade through the
/// rest of the input; the recovery synthesizes an `ExprKind::Poison`.
fn paren_parser<'a, I, PE>(expr: PE) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    expr.delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        .map_with(|inner, e| e.push_expr(ExprKind::Paren(inner)))
        .recover_with(via_parser(
            nested_delimiters::<I, _, _, _, 2>(
                TokenKind::LParen,
                TokenKind::RParen,
                [
                    (TokenKind::LBrace, TokenKind::RBrace),
                    (TokenKind::LBracket, TokenKind::RBracket),
                ],
                |span: SimpleSpan| span,
            )
            .map_with(|ss: SimpleSpan, e: &mut OMapExtra<'_, '_, I>| {
                e.push_expr_at(ss, ExprKind::Poison)
            }),
        ))
}

/// `{ ... }` as an expression — wraps a block in `ExprKind::Block`.
fn block_expr_parser<'a, I, PB>(block: PB) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PB: Parser<'a, I, BlockId, Extra<'a>> + Clone + 'a,
{
    block.map_with(|bid, e| e.push_expr(ExprKind::Block(bid)))
}

/// `return e?` — wraps an optional operand. Parsed at the top of
/// `expr_parser` so the operand can be any expression.
fn return_expr_parser<'a, I, PE>(expr: PE) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    just(TokenKind::KwReturn)
        .ignore_then(expr.or_not())
        .map_with(|val, e| e.push_expr(ExprKind::Return(val)))
}

/// `Ident { f: v, ... }` — record struct literal. Tried before
/// `ident_expr` in the atom alternation.
///
/// Known deviation from Rust: we allow struct literals in `if`/`while`
/// cond positions (Rust forbids them there to keep the grammar
/// unambiguous). The follow-on cleanup is to thread a "no-struct-lit-
/// at-top" flag through the cond's expression parser; see TBD in
/// spec/08_ADT.md.
fn struct_lit_parser<'a, I, PE>(expr: PE) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    let field = ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(expr)
        .map_with(|(name, value), e| StructLitField {
            name,
            value,
            span: e.lex_span(),
        });
    ident_parser()
        .then(
            field
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(name, fields), e| e.push_expr(ExprKind::StructLit { name, fields }))
}

/// Array literals: `[a, b, c]` (Elems) or `[init; N]` (Repeat).
/// Disambiguation: parse the first expr, peek the next token —
/// `;` → Repeat, `,` or `]` → Elems. Empty `[]` is rejected via a
/// separate rule that emits a custom message (E0107).
///
/// The length slot in the Repeat form accepts only an `IntLit` token —
/// see spec/09_ARRAY.md "Length literal extraction" and
/// `int_lit_length_parser` above. Anything richer is a parse error
/// (typically reported as "expected `]`").
fn array_lit_parser<'a, I, PE>(expr: PE) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    #[derive(Clone)]
    enum ArrayLitTail {
        Repeat(ExprId),
        Elems(Vec<ExprId>),
    }
    let tail = choice((
        just(TokenKind::Semi)
            .ignore_then(int_lit_length_parser())
            .map(ArrayLitTail::Repeat),
        just(TokenKind::Comma)
            .ignore_then(
                expr.clone()
                    .separated_by(just(TokenKind::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .map(ArrayLitTail::Elems),
        // No tail — single-element list: `[a]`.
        empty().to(ArrayLitTail::Elems(Vec::new())),
    ));
    let nonempty = expr
        .then(tail)
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|(first, tail), e| {
            let lit = match tail {
                ArrayLitTail::Repeat(len) => ArrayLit::Repeat { init: first, len },
                ArrayLitTail::Elems(rest) => {
                    let mut elems = Vec::with_capacity(1 + rest.len());
                    elems.push(first);
                    elems.extend(rest);
                    ArrayLit::Elems(elems)
                }
            };
            e.push_expr(ExprKind::ArrayLit(lit))
        });
    // Empty `[]` — grammatically valid; produces `ArrayLit::Elems(vec![])`.
    // The "need context type to infer T" question is semantic, not
    // syntactic; typeck handles it (when arrays land typeck-side; until
    // then, the existing `UnsupportedFeature` ArrayLit arm catches it).
    let empty = just(TokenKind::LBracket)
        .ignore_then(just(TokenKind::RBracket))
        .map_with(|_, e| e.push_expr(ExprKind::ArrayLit(ArrayLit::Elems(Vec::new()))));
    choice((nonempty, empty))
}

/// Postfix tower: `f(args)`, `e[i]`, `e.field`, left-folded onto an atom.
fn postfix_parser<'a, I, PA, PE>(
    atom: PA,
    expr: PE,
) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PA: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    let call_args = expr
        .clone()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen));
    let index = expr
        .clone()
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket));
    let field = just(TokenKind::Dot).ignore_then(ident_parser());

    #[derive(Clone)]
    enum Postfix {
        Call(Vec<ExprId>),
        Index(ExprId),
        Field(Ident),
    }
    let op = call_args
        .map(Postfix::Call)
        .or(index.map(Postfix::Index))
        .or(field.map(Postfix::Field));

    atom.foldl_with(op.repeated(), |callee, op, e| {
        let kind = match op {
            Postfix::Call(args) => ExprKind::Call { callee, args },
            Postfix::Index(idx) => ExprKind::Index {
                base: callee,
                index: idx,
            },
            Postfix::Field(name) => ExprKind::Field { base: callee, name },
        };
        e.push_expr(kind)
    })
}

fn block_parser_inner<'a, I, P>(expr: P) -> impl Parser<'a, I, BlockId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    P: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    recursive(move |_block| {
        let item = block_item_parser(expr.clone());
        item.repeated()
            .collect::<Vec<_>>()
            .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
            .map_with(|raw, e| {
                // Bare `;` items parse to `None` and drop out here.
                let items: Vec<BlockItem> = raw.into_iter().flatten().collect();
                e.push_block(items)
            })
            .recover_with(via_parser(
                nested_delimiters::<I, _, _, _, 2>(
                    TokenKind::LBrace,
                    TokenKind::RBrace,
                    [
                        (TokenKind::LParen, TokenKind::RParen),
                        (TokenKind::LBracket, TokenKind::RBracket),
                    ],
                    |span: SimpleSpan| span,
                )
                .map_with(|ss: SimpleSpan, e: &mut OMapExtra<'_, '_, I>| {
                    e.push_block_at(ss, vec![])
                }),
            ))
            .labelled("block")
    })
}

/// Parse one block item, returning `Option<BlockItem>`:
///
/// - `let_form` (always carries `;`) → `Some(BlockItem { has_semi: true })`
/// - `expr_item` (any expression incl. `if`/`block`, with optional trailing
///   `;`) → `Some(BlockItem { has_semi: <was `;` present?> })`
/// - `bare_semi` (a `;` with no preceding expression) → `None`
///
/// `let` is intentionally only parseable here, never inside `expr_parser`,
/// so `1 + let x = 5` stays a parse error.
fn block_item_parser<'a, I, PE>(
    expr: PE,
) -> impl Parser<'a, I, Option<BlockItem>, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    // Block-item `let` is `let_form_no_semi` followed by a mandatory `;`.
    // The grammar of the let itself is shared with `for_parser`'s init
    // slot — see `let_form_no_semi`.
    let let_form = let_form_no_semi(expr.clone())
        .then_ignore(just(TokenKind::Semi))
        .map(|eid| {
            Some(BlockItem {
                expr: eid,
                has_semi: true,
            })
        });

    // Any expression (incl. `if`/`block` via `expr_parser`'s atom alternation)
    // followed by an optional `;`. The `has_semi` flag captures whether the
    // user wrote one — typeck uses it to decide which item produces the
    // block's value.
    let expr_item = expr
        .clone()
        .then(just(TokenKind::Semi).or_not())
        .map(|(eid, semi)| {
            Some(BlockItem {
                expr: eid,
                has_semi: semi.is_some(),
            })
        });

    // Bare `;` — empty statement, produces no AST node. Rust permits
    // `{ ;; let x = 1; ;; x }` and we mirror that.
    let bare_semi = just(TokenKind::Semi).map(|_| None);

    choice((let_form, expr_item, bare_semi)).labelled("block item")
}

/// Parses `let [mut] name (: ty)? (= init)?` — **no trailing `;`**. The
/// grammar shared between block-position let-statements (which append
/// a `;` at the call site) and `for_parser`'s init slot (where `;` is
/// the for-header separator, consumed by `for_parser` itself).
///
/// Not promoted to a sibling of `return_form` / `break_form` /
/// `continue_form` in `expr_parser`'s top-level `choice` because that
/// would make `let x = let y = 5` and `return let x = 5` parse, which
/// Rust rejects. See spec/13_LOOPS.md "Why `let` stays out of
/// `expr_parser`".
fn let_form_no_semi<'a, I, PE>(expr: PE) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    let ty = type_parser(expr.clone());
    just(TokenKind::KwLet)
        .ignore_then(just(TokenKind::KwMut).or_not().map(|m| m.is_some()))
        .then(ident_parser())
        .then(just(TokenKind::Colon).ignore_then(ty).or_not())
        .then(just(TokenKind::Eq).ignore_then(expr).or_not())
        .map_with(|(((mutable, name), ty), init), e| {
            e.push_expr(ExprKind::Let {
                mutable,
                name,
                ty,
                init,
            })
        })
}

fn if_parser<'a, I, PE, PB>(expr: PE, block: PB) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
    PB: Parser<'a, I, BlockId, Extra<'a>> + Clone + 'a,
{
    recursive(move |if_expr| {
        let else_arm = just(TokenKind::KwElse).ignore_then(choice((
            block.clone().map(ElseArm::Block),
            if_expr.map(ElseArm::If),
        )));
        just(TokenKind::KwIf)
            .ignore_then(expr.clone())
            .then(block.clone())
            .then(else_arm.or_not())
            .map_with(|((cond, then_block), else_arm), e| {
                e.push_expr(ExprKind::If {
                    cond,
                    then_block,
                    else_arm,
                })
            })
    })
}

/// `while cond block`. Cond struct-literal ambiguity (`while Foo { } { }`
/// parses the first `{}` as a struct literal) is the same TBD as
/// `if`'s — see `struct_lit_parser` and spec/08_ADT.md.
fn while_parser<'a, I, PE, PB>(expr: PE, block: PB) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
    PB: Parser<'a, I, BlockId, Extra<'a>> + Clone + 'a,
{
    just(TokenKind::KwWhile)
        .ignore_then(expr)
        .then(block)
        .map_with(|(cond, body), e| e.push_expr(ExprKind::While { cond, body }))
}

/// `loop block`. The expression's value type is decided at typeck per
/// the `break expr?` operands inside the body — see spec/13_LOOPS.md.
fn loop_parser<'a, I, PB>(block: PB) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PB: Parser<'a, I, BlockId, Extra<'a>> + Clone + 'a,
{
    just(TokenKind::KwLoop)
        .ignore_then(block)
        .map_with(|body, e| e.push_expr(ExprKind::Loop { body }))
}

/// `for ( init? ; cond? ; update? ) block` — C-style. Each header slot
/// independently optional; `for (;;) { ... }` is the infinite-loop
/// spelling. Parens around the header are mandatory — they delimit
/// header from body unambiguously, fixing the update→body ambiguity
/// the parenless form has (see spec/13_LOOPS.md "Why parens around
/// the `for` header"). `init` may be a `let`-form (parsed via
/// `let_form_no_semi`) or any other expression; the `let` is tried
/// first because the keyword disambiguates.
fn for_parser<'a, I, PE, PB>(expr: PE, block: PB) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
    PB: Parser<'a, I, BlockId, Extra<'a>> + Clone + 'a,
{
    let init = choice((let_form_no_semi(expr.clone()), expr.clone()));

    let header = init
        .or_not()
        .then_ignore(just(TokenKind::Semi))
        .then(expr.clone().or_not())
        .then_ignore(just(TokenKind::Semi))
        .then(expr.or_not())
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen));

    just(TokenKind::KwFor)
        .ignore_then(header)
        .then(block)
        .map_with(|(((init, cond), update), body), e| {
            e.push_expr(ExprKind::For {
                init,
                cond,
                update,
                body,
            })
        })
}

/// `break expr?` — wraps an optional operand. Operand parsing is greedy
/// (consumes any expression that follows the keyword), same as
/// `return`. Use a `;` to terminate explicitly.
fn break_expr_parser<'a, I, PE>(expr: PE) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
{
    just(TokenKind::KwBreak)
        .ignore_then(expr.or_not())
        .map_with(|val, e| e.push_expr(ExprKind::Break { expr: val }))
}

/// `continue` — bare keyword, no operand.
fn continue_expr_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    just(TokenKind::KwContinue).map_with(|_, e| e.push_expr(ExprKind::Continue))
}

fn param_parser<'a, I, PT>(ty: PT) -> impl Parser<'a, I, Param, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PT: Parser<'a, I, TypeId, Extra<'a>> + Clone + 'a,
{
    just(TokenKind::KwMut)
        .or_not()
        .map(|m| m.is_some())
        .then(ident_parser())
        .then_ignore(just(TokenKind::Colon))
        .then(ty)
        .map_with(|((mutable, name), ty), e| Param {
            mutable,
            name,
            ty,
            span: e.lex_span(),
        })
}

fn params_parser<'a, I, PT>(ty: PT) -> impl Parser<'a, I, Vec<Param>, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PT: Parser<'a, I, TypeId, Extra<'a>> + Clone + 'a,
{
    param_parser(ty)
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
}

fn ret_ty_parser<'a, I, PT>(ty: PT) -> impl Parser<'a, I, Option<TypeId>, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PT: Parser<'a, I, TypeId, Extra<'a>> + Clone + 'a,
{
    just(TokenKind::Arrow).ignore_then(ty).or_not()
}

/// Parse a fn signature plus either `{ block }` or `;`. The grammar is
/// the same in both top-level and `extern "C"`-block positions; each call
/// site validates the body's presence/absence and emits a clear error if
/// the wrong shape was used.
fn fn_decl_parser<'a, I>() -> impl Parser<'a, I, FnDecl, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    let expr = expr_parser();
    let ty = type_parser(expr.clone());
    let block = block_parser_inner(expr.clone());
    let body = choice((block.map(Some), just(TokenKind::Semi).to(None)));

    just(TokenKind::KwFn)
        .ignore_then(ident_parser())
        .then(params_parser(ty.clone()))
        .then(ret_ty_parser(ty))
        .then(body)
        .map(|(((name, params), ret_ty), body)| FnDecl {
            name,
            params,
            ret_ty,
            body,
        })
}

fn fn_item_parser<'a, I>() -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    fn_decl_parser()
        .validate(|fn_decl, e: &mut OMapExtra<'_, '_, I>, emitter| {
            if fn_decl.body.is_none() {
                emitter.emit(Rich::custom(
                    e.span(),
                    format!(
                        "bodyless `fn {}` must be inside an `extern \"C\" {{ ... }}` block",
                        fn_decl.name.name
                    ),
                ));
            }
            fn_decl
        })
        .map_with(|fn_decl, e| e.push_item(ItemKind::Fn(fn_decl)))
        .labelled("function")
}

/// `extern "C" { fn name(args) -> ret; ... }`. Only `"C"` is a valid
/// ABI string in v0. Each child fn is validated to be bodyless.
fn extern_block_parser<'a, I>() -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    let abi = any().try_map(|tok: TokenKind, span: SimpleSpan| match tok {
        TokenKind::Str(s) if s == "C" => Ok(s),
        TokenKind::Str(s) => Err(Rich::custom(
            span,
            format!("only \"C\" ABI is supported, got \"{s}\""),
        )),
        other => Err(Rich::custom(
            span,
            format!("expected ABI string \"C\", got {other:?}"),
        )),
    });

    let items = fn_decl_parser()
        .validate(|mut fn_decl, e: &mut OMapExtra<'_, '_, I>, emitter| {
            if fn_decl.body.is_some() {
                emitter.emit(Rich::custom(
                    e.span(),
                    format!(
                        "extern \"C\" fn `{}` must not have a body",
                        fn_decl.name.name
                    ),
                ));
                fn_decl.body = None;
            }
            fn_decl
        })
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace));

    just(TokenKind::KwExtern)
        .ignore_then(abi)
        .then(items)
        .map_with(|(abi, items), e| {
            let span = e.lex_span();
            e.push_item(ItemKind::ExternBlock(ExternBlock { abi, items, span }))
        })
        .labelled("extern block")
}

/// `import "<path>";` — splat-import statement. Top-level only.
/// The path is taken raw; resolution is the loader's job. See
/// spec/14_MODULES.md.
fn import_item_parser<'a, I>() -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    let path = any().try_map(|tok: TokenKind, span: SimpleSpan| match tok {
        TokenKind::Str(s) => Ok(s),
        other => Err(Rich::custom(
            span,
            format!("expected import path string literal, got {other:?}"),
        )),
    });

    just(TokenKind::KwImport)
        .ignore_then(path)
        .then_ignore(just(TokenKind::Semi))
        .map_with(|path, e| {
            let span = e.lex_span();
            e.push_item(ItemKind::Import(ImportItem { path, span }))
        })
        .labelled("import")
}

/// `struct Name { f: T, ... }` — record struct declaration. Empty field
/// list (`struct Foo {}`) is accepted; tuple/unit struct forms (`Foo(...)`,
/// `Foo;`) are not.
fn struct_item_parser<'a, I>() -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    let expr = expr_parser();
    let ty = type_parser(expr);
    let field_decl = ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(ty)
        .map_with(|(name, ty), e| FieldDecl {
            name,
            ty,
            span: e.lex_span(),
        });

    just(TokenKind::KwStruct)
        .ignore_then(ident_parser())
        .then(
            field_decl
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(name, fields), e| {
            let span = e.lex_span();
            e.push_item(ItemKind::Struct(StructDecl { name, fields, span }))
        })
        .labelled("struct declaration")
}

pub(super) fn module_parser<'a, I>() -> impl Parser<'a, I, Vec<ItemId>, Extra<'a>>
where
    I: OValueInput<'a>,
{
    let item = choice((
        import_item_parser(),
        extern_block_parser(),
        struct_item_parser(),
        fn_item_parser(),
    ));
    item.repeated().collect::<Vec<_>>().then_ignore(end())
}
