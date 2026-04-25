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

pub(super) fn type_parser<'a, I>() -> impl Parser<'a, I, TypeId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    recursive(|ty| {
        let named = ident_parser().map_with(|name, e| e.push_type(TypeKind::Named(name)));

        // `*const T` / `*mut T`. Right-recursive on `ty` for nesting.
        let mutability = choice((
            just(TokenKind::KwConst).to(Mutability::Const),
            just(TokenKind::KwMut).to(Mutability::Mut),
        ));
        let ptr = just(TokenKind::Star)
            .ignore_then(mutability)
            .then(ty)
            .map_with(|(mutability, pointee), e| {
                e.push_type(TypeKind::Ptr { mutability, pointee })
            });

        choice((ptr, named)).labelled("type")
    })
}

pub(super) fn expr_parser<'a, I>() -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    recursive(|expr| {
        // `return e?` is an expression of type `!`. We expose it at the top of
        // the expression parser (rather than as an atom) so its operand can
        // itself be any expression — `return e + 1` ⇒ `return (e + 1)`.
        let return_form = just(TokenKind::KwReturn)
            .ignore_then(expr.clone().or_not())
            .map_with(|val, e| e.push_expr(ExprKind::Return(val)));

        let int_lit =
            select! { TokenKind::Int(n) => n }.map_with(|n, e| e.push_expr(ExprKind::IntLit(n)));
        let bool_lit =
            select! { TokenKind::Bool(b) => b }.map_with(|b, e| e.push_expr(ExprKind::BoolLit(b)));
        let char_lit =
            select! { TokenKind::Char(c) => c }.map_with(|c, e| e.push_expr(ExprKind::CharLit(c)));
        let str_lit =
            select! { TokenKind::Str(s) => s }.map_with(|s, e| e.push_expr(ExprKind::StrLit(s)));
        let ident_expr = ident_parser().map_with(|id, e| e.push_expr(ExprKind::Ident(id)));

        let paren = expr
            .clone()
            .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
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
            ));

        let block = block_parser_inner(expr.clone());
        let block_expr = block
            .clone()
            .map_with(|bid, e| e.push_expr(ExprKind::Block(bid)));

        let if_expr = if_parser_inner(expr.clone(), block.clone());

        let atom = choice((
            int_lit, bool_lit, char_lit, str_lit, if_expr, block_expr, paren, ident_expr,
        ));

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
        let postfix_op = call_args
            .map(Postfix::Call)
            .or(index.map(Postfix::Index))
            .or(field.map(Postfix::Field));

        let with_postfix = atom.foldl_with(postfix_op.repeated(), |callee, op, e| {
            let kind = match op {
                Postfix::Call(args) => ExprKind::Call { callee, args },
                Postfix::Index(idx) => ExprKind::Index {
                    base: callee,
                    index: idx,
                },
                Postfix::Field(name) => ExprKind::Field { base: callee, name },
            };
            e.push_expr(kind)
        });

        let pratt_expr = with_postfix.pratt((
            prefix_level!(13,
                TokenKind::Minus => UnOp::Neg,
                TokenKind::Bang => UnOp::Not,
                TokenKind::Tilde => UnOp::BitNot,
            ),
            postfix(
                12,
                just(TokenKind::KwAs).ignore_then(type_parser()),
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

        choice((return_form, pratt_expr)).labelled("expression")
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
    let let_form = just(TokenKind::KwLet)
        .ignore_then(just(TokenKind::KwMut).or_not().map(|m| m.is_some()))
        .then(ident_parser())
        .then(just(TokenKind::Colon).ignore_then(type_parser()).or_not())
        .then(just(TokenKind::Eq).ignore_then(expr.clone()).or_not())
        .then_ignore(just(TokenKind::Semi))
        .map_with(|(((mutable, name), ty), init), e| {
            let expr = e.push_expr(ExprKind::Let {
                mutable,
                name,
                ty,
                init,
            });
            Some(BlockItem { expr, has_semi: true })
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

fn if_parser_inner<'a, I, PE, PB>(
    expr: PE,
    block: PB,
) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
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

fn param_parser<'a, I>() -> impl Parser<'a, I, Param, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|(name, ty), e| Param {
            name,
            ty,
            span: e.lex_span(),
        })
}

fn params_parser<'a, I>() -> impl Parser<'a, I, Vec<Param>, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    param_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
}

fn ret_ty_parser<'a, I>() -> impl Parser<'a, I, Option<TypeId>, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    just(TokenKind::Arrow).ignore_then(type_parser()).or_not()
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
    let block = block_parser_inner(expr.clone());
    let body = choice((block.map(Some), just(TokenKind::Semi).to(None)));

    just(TokenKind::KwFn)
        .ignore_then(ident_parser())
        .then(params_parser())
        .then(ret_ty_parser())
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

pub(super) fn module_parser<'a, I>() -> impl Parser<'a, I, Vec<ItemId>, Extra<'a>>
where
    I: OValueInput<'a>,
{
    let item = choice((extern_block_parser(), fn_item_parser()));
    item.repeated().collect::<Vec<_>>().then_ignore(end())
}
