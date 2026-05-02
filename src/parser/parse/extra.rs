//! Chumsky-side type glue: trait/type aliases and the borrow-splitter helper.
//!
//! Nothing in here describes the language. It exists to keep combinator
//! signatures readable and to factor out the one borrow-checker dance we hit
//! repeatedly.

use chumsky::error::Rich;
use chumsky::extra;
use chumsky::input::{MapExtra, ValueInput};
use chumsky::span::SimpleSpan;

use crate::lexer::TokenKind;
use crate::parser::ast::*;
use crate::reporter::Span;

use super::builder::ModuleBuilder;

/// Chumsky `Extra` for our parser: rich errors over `TokenKind`, parser state
/// is `ModuleBuilder` (carrying the in-progress arenas), no context.
pub(super) type Extra<'a> =
    extra::Full<Rich<'a, TokenKind, SimpleSpan>, extra::SimpleState<ModuleBuilder>, ()>;

/// Shorthand for the `MapExtra` chumsky hands to `.map_with` closures.
pub(super) type OMapExtra<'src, 'b, I> = MapExtra<'src, 'b, I, Extra<'src>>;

/// Trait alias for our specific chumsky input shape: any `ValueInput` that
/// produces our `TokenKind`s with byte-offset spans. Replaces the verbose
/// `ValueInput<'a, Token = TokenKind, Span = SimpleSpan>` bound that would
/// otherwise repeat on every combinator.
pub(super) trait OValueInput<'a>:
    ValueInput<'a, Token = TokenKind, Span = SimpleSpan>
{
}
impl<'a, T> OValueInput<'a> for T where T: ValueInput<'a, Token = TokenKind, Span = SimpleSpan> {}

/// `MapExtra::span()` and `MapExtra::state()` both take `&mut self`, so they
/// can't appear in a single expression. This helper grabs the span first,
/// then the state ref, returning both. Used internally by `MapExtraExt`.
#[inline(always)]
fn ss_then_state<'a, 'b, 'c, I>(
    e: &'c mut OMapExtra<'a, 'b, I>,
) -> (SimpleSpan, &'c mut ModuleBuilder)
where
    I: OValueInput<'a>,
{
    let ss = e.span();
    let st: &mut extra::SimpleState<ModuleBuilder> = e.state();
    (ss, &mut **st)
}

/// Convenience methods on `OMapExtra` that hide the
/// `let (ss, st) = ss_then_state(e); st.push_…(ss, kind)` dance. Each
/// `.map_with` closure becomes a one-liner.
///
/// `*_at` variants accept an explicit `SimpleSpan` (used inside
/// `nested_delimiters` recovery, where chumsky hands the closure the recovery
/// span instead of producing one from `e.span()`).
pub(super) trait MapExtraExt {
    fn push_expr(&mut self, kind: ExprKind) -> ExprId;
    fn push_expr_at(&mut self, ss: SimpleSpan, kind: ExprKind) -> ExprId;
    fn push_block(&mut self, items: Vec<BlockItem>) -> BlockId;
    fn push_block_at(&mut self, ss: SimpleSpan, items: Vec<BlockItem>) -> BlockId;
    fn push_type(&mut self, kind: TypeKind) -> TypeId;
    fn push_item(&mut self, kind: ItemKind) -> ItemId;
    /// The full `lexer::Span` (byte + LSP) covering this combinator's input.
    fn lex_span(&mut self) -> Span;
}

impl<'a, 'b, I> MapExtraExt for OMapExtra<'a, 'b, I>
where
    I: OValueInput<'a>,
{
    fn push_expr(&mut self, kind: ExprKind) -> ExprId {
        let (ss, st) = ss_then_state(self);
        st.push_expr(ss, kind)
    }
    fn push_expr_at(&mut self, ss: SimpleSpan, kind: ExprKind) -> ExprId {
        let st: &mut ModuleBuilder = &mut **self.state();
        st.push_expr(ss, kind)
    }
    fn push_block(&mut self, items: Vec<BlockItem>) -> BlockId {
        let (ss, st) = ss_then_state(self);
        st.push_block(ss, items)
    }
    fn push_block_at(&mut self, ss: SimpleSpan, items: Vec<BlockItem>) -> BlockId {
        let st: &mut ModuleBuilder = &mut **self.state();
        st.push_block(ss, items)
    }
    fn push_type(&mut self, kind: TypeKind) -> TypeId {
        let (ss, st) = ss_then_state(self);
        st.push_type(ss, kind)
    }
    fn push_item(&mut self, kind: ItemKind) -> ItemId {
        let (ss, st) = ss_then_state(self);
        st.push_item(ss, kind)
    }
    fn lex_span(&mut self) -> Span {
        let (ss, st) = ss_then_state(self);
        st.span(ss)
    }
}
