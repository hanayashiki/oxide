//! AST arena bookkeeping. Pure storage — no chumsky parser surface.
//!
//! `ModuleBuilder` is the parser state chumsky carries via `extra::SimpleState`.
//! Combinators call its `push_*` methods to append nodes and get back typed
//! indices.

use std::collections::BTreeMap;

use chumsky::span::SimpleSpan;
use index_vec::IndexVec;

use crate::lexer::{BytePos, LspPos, Span};
use crate::parser::ast::*;
use crate::parser::error::ParseError;

/// Mutable parser state carried by chumsky via `extra::SimpleState`.
/// Combinators push new AST nodes here and return their typed-index handles.
///
/// **Invariant**: arenas may contain orphan nodes left by chumsky backtracks
/// (push is not rolled back when an alternative fails). All consumers MUST
/// walk from `root_items` — never iterate the arenas directly. If we ever
/// want arena-wide passes, implement `chumsky::inspector::Inspector` for
/// `ModuleBuilder` and truncate each `IndexVec` on rollback.
#[derive(Default)]
pub(super) struct ModuleBuilder {
    pub(super) items: IndexVec<ItemId, Item>,
    pub(super) exprs: IndexVec<ExprId, Expr>,
    pub(super) blocks: IndexVec<BlockId, Block>,
    pub(super) types: IndexVec<TypeId, Type>,
    pub(super) root_items: Vec<ItemId>,
    pub(super) errors: Vec<ParseError>,
    pub(super) byte_to_lsp: BTreeMap<usize, LspPos>,
}

impl ModuleBuilder {
    /// Convert a chumsky byte-range span into a full `lexer::Span` (byte + LSP).
    /// AST nodes always span token boundaries, so the lookup never misses for
    /// well-formed input. For synthesized spans we fall back to the nearest
    /// preceding boundary.
    pub(super) fn span(&self, ss: SimpleSpan) -> Span {
        Span {
            start: BytePos { offset: ss.start },
            end: BytePos { offset: ss.end },
            lsp_start: self.lsp_at(ss.start),
            lsp_end: self.lsp_at(ss.end),
        }
    }

    fn lsp_at(&self, byte: usize) -> LspPos {
        if let Some(p) = self.byte_to_lsp.get(&byte) {
            return *p;
        }
        self.byte_to_lsp
            .range(..=byte)
            .next_back()
            .map(|(_, p)| *p)
            .unwrap_or(LspPos { line: 0, character: 0 })
    }

    pub(super) fn finish(self, span: Span) -> (Module, Vec<ParseError>) {
        let module = Module {
            items: self.items,
            exprs: self.exprs,
            blocks: self.blocks,
            types: self.types,
            root_items: self.root_items,
            span,
        };
        (module, self.errors)
    }

    pub(super) fn push_expr(&mut self, ss: SimpleSpan, kind: ExprKind) -> ExprId {
        let span = self.span(ss);
        self.exprs.push(Expr { kind, span })
    }

    pub(super) fn push_block(
        &mut self,
        ss: SimpleSpan,
        items: Vec<ExprId>,
        tail: Option<ExprId>,
    ) -> BlockId {
        let span = self.span(ss);
        self.blocks.push(Block { items, tail, span })
    }

    pub(super) fn push_type(&mut self, ss: SimpleSpan, kind: TypeKind) -> TypeId {
        let span = self.span(ss);
        self.types.push(Type { kind, span })
    }

    pub(super) fn push_item(&mut self, ss: SimpleSpan, kind: ItemKind) -> ItemId {
        let span = self.span(ss);
        self.items.push(Item { kind, span })
    }
}
