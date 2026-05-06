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
            choice(($($tok.to($op),)+)),
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
            choice(($($tok.to($op),)+)),
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
            choice(($($tok.to($op),)+)),
            |op: UnOp, rhs, e: &mut OMapExtra<'_, '_, I>| {
                e.push_expr(ExprKind::Unary { op, expr: rhs })
            },
        )
    };
}

/// One closing `>` of a generic argument list. Accepts `Gt` (when the `>` is
/// followed by whitespace/EOF) or `JointGt` (when it's joined to whatever
/// comes next — typically another `>`, `=`, or a closing punctuator). Both
/// variants close exactly one bracket, so `Foo<Bar<T>>`, `Foo<Bar<T> >`, and
/// `Vec<Vec<i32>>=0` all parse without forcing a space. See spec/01_LEXER.md
/// "Joint `>` rule" for the lexer side.
fn gt_close<'a, I>() -> impl Parser<'a, I, (), Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    choice((just(TokenKind::Gt), just(TokenKind::JointGt))).ignored()
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
        // `Name<T, U, ...>` — named type with optional type-arg list.
        // Empty `<>` is accepted and produces `vec![]` (matches Rust),
        // and the bracket list as a whole is `or_not`-ed so a bare
        // `Name` collapses to the same `vec![]` shape downstream.
        // See spec/16_GENERIC.md §Surface syntax (extension).
        //
        // No `::` required in type position — the type grammar has no
        // `<` operator, so `Name<T>` is unambiguous (unlike expression
        // position where `name<T>(args)` parses as a comparison).
        let type_args = ty
            .clone()
            .separated_by(just(TokenKind::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(TokenKind::Lt), gt_close())
            .or_not()
            .map(|opt| opt.unwrap_or_default());
        let named = ident_parser()
            .then(type_args)
            .map_with(|(name, type_args), e| e.push_type(TypeKind::Named { name, type_args }));

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

        // `[extern "C"]? fn(p1: T1, p2: T2[, ...]) [-> R]`. See
        // spec/19_FN_PTR.md §7. Param names are kept on the AST for
        // pretty-print + future diagnostics; HIR drops them.
        //
        // Same param-list invariants as `params_parser` (a single linear
        // pass enforces shape: `...` last, no trailing `,` after `...`,
        // `...` requires a fixed param before it). The variadic-without-
        // `extern "C"` rule is the fn-ptr-type analogue of E0271 and is
        // emitted here directly (parser-level diagnostic, same as the
        // E0271 emit in `fn_decl_parser`).
        let fn_ptr_param = ident_parser()
            .then_ignore(just(TokenKind::Colon))
            .or_not()
            .then(ty.clone())
            .map_with(|(name, ty), e| FnPtrParam {
                name,
                ty,
                span: e.lex_span(),
            });

        #[derive(Clone)]
        enum FnPtrEntry {
            Param(FnPtrParam),
            Dots(SimpleSpan),
        }

        let fn_ptr_entry = choice((
            fn_ptr_param.map(FnPtrEntry::Param),
            just(TokenKind::DotDotDot).map_with(|_, e| FnPtrEntry::Dots(e.span())),
        ));

        let fn_ptr_nonempty = fn_ptr_entry
            .clone()
            .then(
                just(TokenKind::Comma)
                    .ignore_then(fn_ptr_entry)
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .then(just(TokenKind::Comma).or_not().map(|c| c.is_some()))
            .map(|((first, rest), trailing_comma)| {
                let mut entries: Vec<FnPtrEntry> = Vec::with_capacity(1 + rest.len());
                entries.push(first);
                entries.extend(rest);
                (entries, trailing_comma)
            });
        let fn_ptr_inside = choice((fn_ptr_nonempty, empty().map(|_| (Vec::new(), false))))
            .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen));

        // `extern "C"` prefix. Only `"C"` is accepted; other ABI
        // strings reject with the same shape `extern_block_parser`
        // uses.
        let abi = just(TokenKind::KwExtern)
            .ignore_then(
                any().try_map(|tok: TokenKind, span: SimpleSpan| match tok {
                    TokenKind::Str(s) if s == "C" => Ok(()),
                    TokenKind::Str(s) => Err(Rich::custom(
                        span,
                        format!("only \"C\" ABI is supported in fn pointer type, got \"{s}\""),
                    )),
                    other => Err(Rich::custom(
                        span,
                        format!("expected ABI string \"C\", got {other:?}"),
                    )),
                }),
            )
            .or_not()
            .map(|opt| opt.is_some());

        let fn_ptr = abi
            .then_ignore(just(TokenKind::KwFn))
            .then(fn_ptr_inside)
            .then(
                just(TokenKind::Arrow)
                    .ignore_then(ty.clone())
                    .or_not(),
            )
            .validate(
                |((is_extern_c, (entries, trailing_comma)), ret_ty), _e, emitter| {
                    let mut params: Vec<FnPtrParam> = Vec::with_capacity(entries.len());
                    let mut variadic: Option<SimpleSpan> = None;
                    let mut malformed = false;
                    let mut emit = |span: SimpleSpan, msg: &str| {
                        emitter.emit(Rich::custom(span, msg.to_string()));
                    };
                    let n = entries.len();
                    for (i, ent) in entries.into_iter().enumerate() {
                        match ent {
                            FnPtrEntry::Param(p) => params.push(p),
                            FnPtrEntry::Dots(dots_span) => {
                                if variadic.is_some() {
                                    emit(
                                        dots_span,
                                        "`...` may appear only once in a parameter list",
                                    );
                                    malformed = true;
                                } else if i == 0 {
                                    emit(
                                        dots_span,
                                        "`...` requires at least one fixed parameter before it",
                                    );
                                    malformed = true;
                                } else if i != n - 1 {
                                    emit(
                                        dots_span,
                                        "`...` must be the last entry in a parameter list",
                                    );
                                    malformed = true;
                                } else if trailing_comma {
                                    emit(dots_span, "no trailing `,` after `...`");
                                    malformed = true;
                                }
                                variadic = Some(dots_span);
                            }
                        }
                    }
                    let mut is_variadic = !malformed && variadic.is_some();
                    if is_variadic && !is_extern_c {
                        // E0271 — variadic only legal under `extern "C"`.
                        // Same wording / error code as `fn_decl_parser`'s
                        // E0271 emit (spec/15_VARIADIC.md).
                        if let Some(dots_span) = variadic {
                            emitter.emit(Rich::custom(
                                dots_span,
                                "E0271: `...` only allowed in `extern \"C\"` declarations; \
                                 help: variadic Oxide functions are not supported; \
                                 use `extern \"C\"` to qualify the fn pointer type"
                                    .to_string(),
                            ));
                        }
                        is_variadic = false;
                    }
                    TypeKind::Fn {
                        is_extern_c,
                        params,
                        is_variadic,
                        ret_ty,
                    }
                },
            )
            .map_with(|kind, e| e.push_type(kind));

        choice((fn_ptr, ptr, array, named)).labelled("type")
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

        // Cast slot and turbofish call sites need a type parser; build
        // one from the recursive expr handle so array-length slots
        // inside cast/turbofish types (e.g. `x as [T; 3]`,
        // `f::<[T; 3]>(...)`) can recurse through expr correctly.
        let ty_in_expr = type_parser(expr.clone());

        // Postfix tower: call (with optional turbofish), index, field —
        // left-folded onto `atom`. See spec/16_GENERIC.md §Surface syntax.
        let with_postfix = postfix_parser(atom, expr.clone(), ty_in_expr.clone());

        let pratt_expr = with_postfix.pratt((
            prefix_level!(13,
                just(TokenKind::Minus) => UnOp::Neg,
                just(TokenKind::Bang)=> UnOp::Not,
                just(TokenKind::Tilde) => UnOp::BitNot,
                // `*expr` — pointer deref. Position-disambiguated from
                // binary `*` (Mul, level 11) by the Pratt builder.
                // See spec/07_POINTER.md "Deref operator".
                just(TokenKind::Star) => UnOp::Deref,
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
                just(TokenKind::Star) => BinOp::Mul,
                just(TokenKind::Slash) => BinOp::Div,
                just(TokenKind::Percent) => BinOp::Rem,
            ),
            binop_level!(left(10),
                just(TokenKind::Plus) => BinOp::Add,
                just(TokenKind::Minus) => BinOp::Sub,
            ),
            // Level 9 — shift. `>>` is a multi-token sequence (`JointGt Gt`)
            // because the lexer always splits `>` per character; we recombine
            // here. Pratt's `Infix` rewinds on op-parser failure (chumsky
            // 0.12 pratt.rs:611), so a partial `JointGt` consume that fails
            // on the second token cleanly falls through to lower levels —
            // important for `>=` (level 5) and `>>=` (level 1).
            binop_level!(left(9),
                just(TokenKind::Shl) => BinOp::Shl,
                just(TokenKind::JointGt).then(just(TokenKind::Gt)).to(BinOp::Shr) => BinOp::Shr,
            ),
            binop_level!(left(8), just(TokenKind::Amp) => BinOp::BitAnd),
            binop_level!(left(7), just(TokenKind::Caret) => BinOp::BitXor),
            binop_level!(left(6), just(TokenKind::Pipe) => BinOp::BitOr),
            // Level 5 — comparison. `>=` is the multi-token sequence
            // `JointGt Eq`. A plain `>` comparison accepts *either* `Gt`
            // (the source `>` was followed by whitespace) *or* `JointGt`
            // — so `id<i32>(7)` (no `::`) still parses as the comparison
            // `(id<i32)>(7)` even though `>(`'s `>` lexes as `JointGt`.
            // chumsky's `choice` atomically rewinds between branches
            // (primitive.rs:957), so the `Ge` branch consuming `JointGt`
            // and failing on a non-`Eq` next token cleanly falls through
            // to the `Gt`/`JointGt` branches.
            binop_level!(left(5),
                just(TokenKind::Lt) => BinOp::Lt,
                just(TokenKind::Le) => BinOp::Le,
                just(TokenKind::JointGt).then(just(TokenKind::Eq)) => BinOp::Ge,
                just(TokenKind::Gt) => BinOp::Gt,
                just(TokenKind::JointGt) => BinOp::Gt,
            ),
            binop_level!(left(4),
                just(TokenKind::EqEq) => BinOp::Eq,
                just(TokenKind::Ne) => BinOp::Ne,
            ),
            binop_level!(left(3), just(TokenKind::AndAnd) => BinOp::And),
            binop_level!(left(2), just(TokenKind::OrOr) => BinOp::Or),
            // Level 1 — assignment. `>>=` is `JointGt JointGt Eq`; pratt's
            // rewind covers a partial-match failure on the third token, so
            // earlier `JointGt`-prefixed levels (5, 9) don't strand the
            // cursor.
            assign_level!(
                just(TokenKind::Eq) => AssignOp::Eq,
                just(TokenKind::PlusEq) => AssignOp::Add,
                just(TokenKind::MinusEq) => AssignOp::Sub,
                just(TokenKind::StarEq) => AssignOp::Mul,
                just(TokenKind::SlashEq) => AssignOp::Div,
                just(TokenKind::PercentEq) => AssignOp::Rem,
                just(TokenKind::AmpEq) => AssignOp::BitAnd,
                just(TokenKind::PipeEq) => AssignOp::BitOr,
                just(TokenKind::CaretEq) => AssignOp::BitXor,
                just(TokenKind::ShlEq) => AssignOp::Shl,
                just(TokenKind::JointGt)
                    .then(just(TokenKind::JointGt))
                    .then(just(TokenKind::Eq))
                    => AssignOp::Shr,
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
    let ty = type_parser(expr.clone());
    let field = ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(expr)
        .map_with(|(name, value), e| StructLitField {
            name,
            value,
            span: e.lex_span(),
        });
    // Turbofish type-args `::<T, U>`. Empty `::<>` is accepted and
    // produces `vec![]` — matches Rust, equivalent to no turbofish.
    // The `or_not().unwrap_or_default()` collapses three source forms
    // (no turbofish, `::<>`, `::<T,...>`) into a single `Vec<TypeId>`.
    // The `::` is mandatory (matches Rust); without it, `Name<T>{...}`
    // parses as a comparison and fails on the trailing `{`.
    // See spec/16_GENERIC.md §Surface syntax (extension).
    let turbofish = just(TokenKind::ColonColon)
        .ignore_then(
            ty.separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::Lt), gt_close()),
        )
        .or_not()
        .map(|opt| opt.unwrap_or_default());
    ident_parser()
        .then(turbofish)
        .then(
            field
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|((name, type_args), fields), e| {
            e.push_expr(ExprKind::StructLit {
                name,
                type_args,
                fields,
            })
        })
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

/// Postfix tower: `f(args)`, `f::<T, U>(args)`, `e[i]`, `e.field`,
/// left-folded onto an atom. Turbofish `::<...>` is gated by `::` —
/// without it, `name<T>(args)` parses as the comparison `(name < T) > (args)`.
/// This matches Rust and is the load-bearing disambiguation for spec/16
/// §Surface syntax.
fn postfix_parser<'a, I, PA, PE, PT>(
    atom: PA,
    expr: PE,
    ty: PT,
) -> impl Parser<'a, I, ExprId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PA: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
    PT: Parser<'a, I, TypeId, Extra<'a>> + Clone + 'a,
{
    // Turbofish type-args `::<T, U>`. Empty `::<>` is accepted and
    // produces `vec![]` — matches Rust, equivalent to no turbofish.
    // The `or_not().unwrap_or_default()` collapses three source forms
    // (no turbofish, `::<>`, `::<T,...>`) into a single `Vec<TypeId>`.
    // See spec/16_GENERIC.md §Surface syntax.
    let turbofish = just(TokenKind::ColonColon)
        .ignore_then(
            ty.clone()
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::Lt), gt_close()),
        )
        .or_not()
        .map(|opt| opt.unwrap_or_default());

    let call_args = expr
        .clone()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen));
    let call = turbofish.then(call_args);
    let index = expr
        .clone()
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket));
    let field = just(TokenKind::Dot).ignore_then(ident_parser());

    #[derive(Clone)]
    enum Postfix {
        Call {
            type_args: Vec<TypeId>,
            args: Vec<ExprId>,
        },
        Index(ExprId),
        Field(Ident),
    }
    let op = call
        .map(|(type_args, args)| Postfix::Call { type_args, args })
        .or(index.map(Postfix::Index))
        .or(field.map(Postfix::Field));

    atom.foldl_with(op.repeated(), |callee, op, e| {
        let kind = match op {
            Postfix::Call { type_args, args } => ExprKind::Call {
                callee,
                args,
                type_args,
            },
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
    recursive(move |block| {
        // Hand the recursive `block` handle to `block_item_parser` so it
        // can build a non-Pratt `block_like` alternative
        // (`if`/`while`/`for`/`loop`/bare-`{…}`) for statement-position
        // block items. See `block_item_parser`.
        let item = block_item_parser(expr.clone(), block.clone());
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
/// - `block_like` (`if`/`while`/`for`/`loop`/bare-`{…}` at statement
///   position, optional trailing `;`) → `Some(BlockItem { has_semi: <…> })`
/// - `expr_item` (any other expression, with optional trailing `;`) →
///   `Some(BlockItem { has_semi: <was `;` present?> })`
/// - `bare_semi` (a `;` with no preceding expression) → `None`
///
/// The `block_like` alternative is tried *before* `expr_item` so that
/// `if true { … } -1` parses as two block items (the `if`, then `-1` via
/// unary `Neg`) rather than `Binary(Sub, If(…), Int(1))`. This mirrors
/// Rust's `Restrictions::STMT_EXPR` rule (`expr_is_complete()` returns
/// true for `If`/`Match`/`Block`/`While`/`Loop`/`ForLoop`, halting the
/// operator-parsing loop). The restriction lives at the block-item level
/// only — when these forms appear in expression position (e.g. RHS of
/// `+`), they go through `expr_parser`'s atom alternation as usual and
/// participate in the full Pratt tower. See spec/03_PARSER.md grammar.
///
/// `let` is intentionally only parseable here, never inside `expr_parser`,
/// so `1 + let x = 5` stays a parse error.
fn block_item_parser<'a, I, PE, PB>(
    expr: PE,
    block: PB,
) -> impl Parser<'a, I, Option<BlockItem>, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PE: Parser<'a, I, ExprId, Extra<'a>> + Clone + 'a,
    PB: Parser<'a, I, BlockId, Extra<'a>> + Clone + 'a,
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

    // Block-form statement: one of `if`/`while`/`for`/`loop`/bare-`{…}`
    // with no Pratt continuation. After the block-like expression ends,
    // the parser returns to `block_parser_inner`'s repeat loop and the
    // next token (e.g. `-`) starts a fresh item — `Minus` then enters
    // the next item's Pratt as prefix-13 `Neg`. Mirrors Rust's
    // `STMT_EXPR` restriction; see the doc comment above and
    // spec/03_PARSER.md grammar (`BlockItem ::= … | IfExpr | BlockExpr | …`).
    let block_like = choice((
        if_parser(expr.clone(), block.clone()),
        while_parser(expr.clone(), block.clone()),
        for_parser(expr.clone(), block.clone()),
        loop_parser(block.clone()),
        block_expr_parser(block.clone()),
    ))
    .then(just(TokenKind::Semi).or_not())
    .map(|(eid, semi)| {
        Some(BlockItem {
            expr: eid,
            has_semi: semi.is_some(),
        })
    });

    // Any other expression followed by an optional `;`. The `has_semi`
    // flag captures whether the user wrote one — typeck uses it to
    // decide which item produces the block's value.
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

    choice((let_form, block_like, expr_item, bare_semi)).labelled("block item")
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

/// Output of `params_parser`. The `variadic` slot carries the span of the
/// `...` token when present so `fn_decl_parser` can emit E0271 with a
/// pointer at the `...` itself rather than the whole signature.
/// See spec/15_VARIADIC.md "Parser".
pub(super) struct ParsedParams {
    pub params: Vec<Param>,
    pub variadic: Option<SimpleSpan>,
}

/// Parameter list. Recognises a trailing `, ...` as a C-style variadic
/// marker and surfaces it as `variadic = Some(span)` for the caller.
/// Malformed shapes — `(...)`, `(a, ..., b)`, `(a, ...,)`, `(... )` — are
/// rejected directly here via `Rich::custom`.
fn params_parser<'a, I, PT>(ty: PT) -> impl Parser<'a, I, ParsedParams, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PT: Parser<'a, I, TypeId, Extra<'a>> + Clone + 'a,
{
    // Layout strategy: we accept any comma-separated mix of real params
    // and `...` markers, then a *single* linear pass enforces:
    //   - `...` must be the last entry,
    //   - `...` requires a fixed param before it,
    //   - no trailing comma after `...`.
    // Catching the trailing-comma-after-`...` case requires looking at
    // the *raw* comma run, so we don't use `.separated_by(...).
    // allow_trailing()`; we drive the comma loop by hand and remember
    // whether the closing `)` was preceded by an extra comma.
    #[derive(Clone)]
    enum Entry {
        Param(Param),
        Dots(SimpleSpan),
    }

    let entry = choice((
        param_parser(ty).map(Entry::Param),
        just(TokenKind::DotDotDot).map_with(|_, e| Entry::Dots(e.span())),
    ));

    // `entry (',' entry)* ','?` — explicit form so we can tell whether
    // the trailing comma was present at the closing paren.
    let nonempty_list = entry
        .clone()
        .then(
            just(TokenKind::Comma)
                .ignore_then(entry)
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then(just(TokenKind::Comma).or_not().map(|c| c.is_some()))
        .map(|((first, rest), trailing_comma)| {
            let mut entries: Vec<Entry> = Vec::with_capacity(1 + rest.len());
            entries.push(first);
            entries.extend(rest);
            (entries, trailing_comma)
        });

    let inside = choice((nonempty_list, empty().map(|_| (Vec::new(), false))));

    inside
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        // `validate` lets us *emit* a diagnostic without failing the
        // parse. We always return a `ParsedParams` so the rest of the
        // fn signature continues to parse and downstream recovery isn't
        // disturbed; the variadic flag is cleared on malformed shapes
        // so callers don't see a half-valid signature.
        .validate(|(entries, trailing_comma), _e, emitter| {
            // Single linear pass. Each malformed shape emits exactly one
            // diagnostic so users see one error per offending `...`, not
            // a cascade. A second `...` would emit its own
            // "may appear only once" message — but the spec disallows
            // any non-extern variadic usage anyway, so that path is
            // covered by E0271 too.
            let mut params: Vec<Param> = Vec::with_capacity(entries.len());
            let mut variadic: Option<SimpleSpan> = None;
            let mut malformed = false;
            let mut emit = |span: SimpleSpan, msg: &str| {
                emitter.emit(Rich::custom(span, msg.to_string()));
            };
            let n = entries.len();
            for (i, ent) in entries.into_iter().enumerate() {
                match ent {
                    Entry::Param(p) => {
                        // The "not last" diagnostic is emitted at the
                        // `...` site; nothing to report here.
                        params.push(p);
                    }
                    Entry::Dots(dots_span) => {
                        if variadic.is_some() {
                            emit(dots_span, "`...` may appear only once in a parameter list");
                            malformed = true;
                        } else if i == 0 {
                            // `fn f(...)` — no fixed param before `...`.
                            emit(
                                dots_span,
                                "`...` requires at least one fixed parameter before it",
                            );
                            malformed = true;
                        } else if i != n - 1 {
                            // `fn f(a, ..., b)` — `...` not the last entry.
                            emit(
                                dots_span,
                                "`...` must be the last entry in a parameter list",
                            );
                            malformed = true;
                        } else if trailing_comma {
                            // `fn f(a, ...,)` — trailing `,` after `...`.
                            // Only relevant when `...` is the last entry,
                            // otherwise the "not last" rule already fired.
                            emit(dots_span, "no trailing `,` after `...`");
                            malformed = true;
                        }
                        variadic = Some(dots_span);
                    }
                }
            }
            ParsedParams {
                params,
                variadic: if malformed { None } else { variadic },
            }
        })
}

fn ret_ty_parser<'a, I, PT>(ty: PT) -> impl Parser<'a, I, Option<TypeId>, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    PT: Parser<'a, I, TypeId, Extra<'a>> + Clone + 'a,
{
    just(TokenKind::Arrow).ignore_then(ty).or_not()
}

/// Parse a fn signature plus either `{ block }` or `;`. The grammar is
/// the same in both top-level and `extern "C"`-block positions; HIR
/// lowering validates the body's presence/absence against the surrounding
/// item context (`BodylessFnOutsideExtern` / `ExternFnHasBody`) and
/// rejects `extern "C"` fns that carry generic params (`GenericExternFn`).
/// See spec/16_GENERIC.md §HIR.
fn fn_decl_parser<'a, I>() -> impl Parser<'a, I, FnDecl, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    let expr = expr_parser();
    let ty = type_parser(expr.clone());
    let block = block_parser_inner(expr.clone());
    let body = choice((block.map(Some), just(TokenKind::Semi).to(None)));

    // Generic parameter list `<T, U, ...>`. Empty `<>` is accepted and
    // produces `vec![]` — matches Rust, where `fn f<>()` is well-formed
    // and equivalent to `fn f()`. Absence of brackets entirely also
    // produces `vec![]` (via `or_not().unwrap_or_default()`), so
    // downstream layers see one shape regardless of source form.
    // See spec/16_GENERIC.md §Surface syntax.
    let generic_params = ident_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::Lt), gt_close())
        .or_not()
        .map(|opt| opt.unwrap_or_default());

    just(TokenKind::KwFn)
        .ignore_then(ident_parser())
        .then(generic_params)
        .then(params_parser(ty.clone()))
        .then(ret_ty_parser(ty))
        .then(body)
        // Validate the variadic-vs-body invariant. E0271 (variadic on a
        // non-extern fn) is reported here because this is the only place
        // where both `is_variadic` and body presence are simultaneously
        // known. `validate` emits the diagnostic without failing the
        // parse — the caller still gets back a usable `FnDecl` (with
        // `is_variadic` cleared) so downstream layers compile cleanly.
        .validate(
            |((((name, generic_params), parsed), ret_ty), body), _e, emitter| {
                let ParsedParams { params, variadic } = parsed;
                let mut is_variadic = variadic.is_some();
                if let Some(dots_span) = variadic {
                    if body.is_some() {
                        // E0271 — `...` only allowed in `extern "C"`
                        // declarations. Reported via the existing parse
                        // error pathway (Rich::custom → ParseError::Custom).
                        // See spec/15_VARIADIC.md.
                        emitter.emit(Rich::custom(
                            dots_span,
                            "E0271: `...` only allowed in `extern \"C\"` declarations; \
                             help: variadic Oxide functions are not supported; \
                             use `extern \"C\"` to call a C variadic"
                                .to_string(),
                        ));
                        is_variadic = false;
                    }
                }
                FnDecl {
                    name,
                    generic_params,
                    params,
                    is_variadic,
                    ret_ty,
                    body,
                }
            },
        )
}

fn fn_item_parser<'a, I>() -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    fn_decl_parser()
        .map_with(|fn_decl, e| e.push_item(ItemKind::Fn(fn_decl)))
        .labelled("function")
}

/// `extern "C" { items... }`. Only `"C"` is a valid ABI string in v0.
/// Children are arbitrary items pushed via `item` (so the same combinator
/// chain is reused at top level and inside a block); HIR lowering decides
/// which item shapes are legal here and validates fn body presence.
fn extern_block_parser<'a, I, P>(item: P) -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
    P: Parser<'a, I, ItemId, Extra<'a>> + Clone + 'a,
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

    let items = item
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

    // Generic parameter list `<T, U, ...>`. Same shape as
    // `fn_decl_parser` — empty `<>` is accepted, missing brackets also
    // collapse to `vec![]`. See spec/16_GENERIC.md §Surface syntax
    // (extension).
    let generic_params = ident_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::Lt), gt_close())
        .or_not()
        .map(|opt| opt.unwrap_or_default());

    just(TokenKind::KwStruct)
        .ignore_then(ident_parser())
        .then(generic_params)
        .then(
            field_decl
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|((name, generic_params), fields), e| {
            let span = e.lex_span();
            e.push_item(ItemKind::Struct(StructDecl {
                name,
                generic_params,
                fields,
                span,
            }))
        })
        .labelled("struct declaration")
}

/// Reusable item-position combinator. `recursive` is required because
/// `extern_block_parser` parses items as children, producing
/// `item → extern_block → item` mutual recursion.
fn item_parser<'a, I>() -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    recursive(|item| {
        choice((
            import_item_parser(),
            extern_block_parser(item.clone()),
            struct_item_parser(),
            fn_item_parser(),
            const_item_parser(),
        ))
    })
}

/// `const Name: Type = LITERAL;`. The RHS slot accepts exactly one
/// literal token — `IntLit | BoolLit | CharLit | StrLit`. No
/// expressions, casts, parens, or const-eval. Same posture as the
/// array-length slot's `int_lit_length_parser`. See
/// spec/18_CONST.md.
fn const_item_parser<'a, I>() -> impl Parser<'a, I, ItemId, Extra<'a>> + Clone
where
    I: OValueInput<'a>,
{
    let expr = expr_parser();
    let ty = type_parser(expr);
    let literal = choice((
        int_lit_parser(),
        bool_lit_parser(),
        char_lit_parser(),
        str_lit_parser(),
    ))
    .labelled("const literal");

    just(TokenKind::KwConst)
        .ignore_then(ident_parser())
        .then_ignore(just(TokenKind::Colon))
        .then(ty)
        .then_ignore(just(TokenKind::Eq))
        .then(literal)
        .then_ignore(just(TokenKind::Semi))
        .map_with(|((name, ty), value), e| {
            let span = e.lex_span();
            e.push_item(ItemKind::Const(ConstDecl {
                name,
                ty,
                value,
                span,
            }))
        })
        .labelled("const item")
}

pub(super) fn module_parser<'a, I>() -> impl Parser<'a, I, Vec<ItemId>, Extra<'a>>
where
    I: OValueInput<'a>,
{
    item_parser()
        .repeated()
        .collect::<Vec<_>>()
        .then_ignore(end())
}
