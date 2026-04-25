//! AST → HIR lowering. Resolves value-namespace names (locals, fns) and
//! propagates spans. Types pass through as syntactic names.
//!
//! Two-phase walk:
//!   1. Pre-scan module items to allocate `FnId`s and populate the module
//!      scope (so functions can be forward-referenced).
//!   2. Walk each function body, lowering expressions and resolving names.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use index_vec::IndexVec;

use crate::lexer::Span;
use crate::parser::ast;

use super::ir::*;

pub fn lower(ast_module: &ast::Module) -> (HirModule, Vec<HirError>) {
    let mut lowerer = Lowerer::new(ast_module);
    let item_to_fid = lowerer.prescan_fns();
    for (i, &iid) in ast_module.root_items.iter().enumerate() {
        if let Some(fid) = item_to_fid[i] {
            lowerer.lower_fn(iid, fid);
        }
    }
    lowerer.finish()
}

struct Lowerer<'a> {
    ast: &'a ast::Module,
    fns: IndexVec<FnId, HirFn>,
    locals: IndexVec<LocalId, HirLocal>,
    exprs: IndexVec<HExprId, HirExpr>,
    blocks: IndexVec<HBlockId, HirBlock>,
    root_fns: Vec<FnId>,
    errors: Vec<HirError>,
    /// Module-level fn names. Always-on (forward references work).
    module_scope: HashMap<String, FnId>,
    /// Stack of block scopes for locals. Innermost is last; lookup is LIFO.
    scopes: Vec<HashMap<String, LocalId>>,
}

impl<'a> Lowerer<'a> {
    fn new(ast: &'a ast::Module) -> Self {
        Self {
            ast,
            fns: IndexVec::new(),
            locals: IndexVec::new(),
            exprs: IndexVec::new(),
            blocks: IndexVec::new(),
            root_fns: Vec::new(),
            errors: Vec::new(),
            module_scope: HashMap::new(),
            scopes: Vec::new(),
        }
    }

    /// Pass 1: allocate a `FnId` for each module-level fn item, push a stub
    /// `HirFn` into the arena (body filled in pass 2), and register the
    /// name in `module_scope`. Duplicate names emit `DuplicateFn` but both
    /// items still get FnIds — only the first wins name resolution.
    fn prescan_fns(&mut self) -> Vec<Option<FnId>> {
        let mut out = Vec::with_capacity(self.ast.root_items.len());
        for &iid in &self.ast.root_items {
            let item = &self.ast.items[iid];
            match &item.kind {
                ast::ItemKind::Fn(fn_decl) => {
                    let fid = self.fns.push(HirFn {
                        name: fn_decl.name.name.clone(),
                        params: Vec::new(),
                        ret_ty: None,
                        body: HBlockId::from_raw(u32::MAX), // sentinel; replaced in pass 2
                        span: item.span.clone(),
                    });
                    self.root_fns.push(fid);
                    match self.module_scope.entry(fn_decl.name.name.clone()) {
                        Entry::Vacant(e) => {
                            e.insert(fid);
                        }
                        Entry::Occupied(e) => {
                            let first_fid = *e.get();
                            self.errors.push(HirError::DuplicateFn {
                                name: fn_decl.name.name.clone(),
                                first: self.fns[first_fid].span.clone(),
                                dup: item.span.clone(),
                            });
                        }
                    }
                    out.push(Some(fid));
                }
            }
        }
        out
    }

    /// Pass 2: lower a function body. Pushes a scope for params, lowers
    /// each, lowers the body block, and writes back into the stub `HirFn`.
    fn lower_fn(&mut self, iid: ast::ItemId, fid: FnId) {
        let item = &self.ast.items[iid];
        let ast::ItemKind::Fn(fn_decl) = &item.kind;
        // ^ infallible: prescan only allocated FnIds for fn items.

        // Outer scope holds parameters; the body block pushes its own
        // sub-scope on top.
        self.scopes.push(HashMap::new());

        let param_specs: Vec<_> = fn_decl
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty, p.span.clone()))
            .collect();

        let mut params = Vec::with_capacity(param_specs.len());
        for (name, ty_id, span) in param_specs {
            let ty = self.lower_ty(ty_id);
            let lid = self.locals.push(HirLocal {
                name: name.name.clone(),
                mutable: false,
                ty: Some(ty),
                span,
            });
            self.scopes.last_mut().unwrap().insert(name.name, lid);
            params.push(lid);
        }

        let ret_ty = fn_decl.ret_ty.map(|t| self.lower_ty(t));
        let body_ast_id = fn_decl.body;
        let body = self.lower_block(body_ast_id);

        self.scopes.pop();

        let hir_fn = &mut self.fns[fid];
        hir_fn.params = params;
        hir_fn.ret_ty = ret_ty;
        hir_fn.body = body;
    }

    fn lower_block(&mut self, bid: ast::BlockId) -> HBlockId {
        let block = &self.ast.blocks[bid];
        let span = block.span.clone();
        let item_ids: Vec<_> = block.items.clone();
        let tail_id = block.tail;

        self.scopes.push(HashMap::new());
        let items: Vec<_> = item_ids.into_iter().map(|e| self.lower_expr(e)).collect();
        let tail = tail_id.map(|e| self.lower_expr(e));
        self.scopes.pop();

        self.blocks.push(HirBlock { items, tail, span })
    }

    fn lower_expr(&mut self, eid: ast::ExprId) -> HExprId {
        let expr = &self.ast.exprs[eid];
        let span = expr.span.clone();

        // `Paren` is purely syntactic — drop the wrapper, return the inner.
        if let ast::ExprKind::Paren(inner) = &expr.kind {
            let inner = *inner;
            return self.lower_expr(inner);
        }

        let kind = match expr.kind.clone() {
            ast::ExprKind::IntLit(n) => HirExprKind::IntLit(n),
            ast::ExprKind::BoolLit(b) => HirExprKind::BoolLit(b),
            ast::ExprKind::CharLit(c) => self.lower_char_lit(c, &span),
            ast::ExprKind::StrLit(s) => HirExprKind::StrLit(s),
            ast::ExprKind::Ident(id) => self.resolve_ident(&id),
            ast::ExprKind::Paren(_) => unreachable!("handled above"),
            ast::ExprKind::Unary { op, expr } => {
                let expr = self.lower_expr(expr);
                HirExprKind::Unary { op, expr }
            }
            ast::ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.lower_expr(lhs);
                let rhs = self.lower_expr(rhs);
                HirExprKind::Binary { op, lhs, rhs }
            }
            ast::ExprKind::Assign { op, lhs, rhs } => {
                let target = self.lower_expr(lhs);
                let rhs = self.lower_expr(rhs);
                HirExprKind::Assign { op, target, rhs }
            }
            ast::ExprKind::Call { callee, args } => {
                let callee = self.lower_expr(callee);
                let args: Vec<_> = args.into_iter().map(|a| self.lower_expr(a)).collect();
                HirExprKind::Call { callee, args }
            }
            ast::ExprKind::Index { base, index } => {
                let base = self.lower_expr(base);
                let index = self.lower_expr(index);
                HirExprKind::Index { base, index }
            }
            ast::ExprKind::Field { base, name } => {
                let base = self.lower_expr(base);
                HirExprKind::Field { base, name: name.name }
            }
            ast::ExprKind::Cast { expr, ty } => {
                let expr = self.lower_expr(expr);
                let ty = self.lower_ty(ty);
                HirExprKind::Cast { expr, ty }
            }
            ast::ExprKind::If { cond, then_block, else_arm } => {
                let cond = self.lower_expr(cond);
                let then_block = self.lower_block(then_block);
                let else_arm = else_arm.map(|arm| self.lower_else_arm(arm));
                HirExprKind::If { cond, then_block, else_arm }
            }
            ast::ExprKind::Block(bid) => HirExprKind::Block(self.lower_block(bid)),
            ast::ExprKind::Return(val) => {
                let val = val.map(|v| self.lower_expr(v));
                HirExprKind::Return(val)
            }
            ast::ExprKind::Let { mutable, name, ty, init } => {
                // Lower init FIRST so `let x = x;` doesn't see the new binding —
                // matches Rust's semantics.
                let init = init.map(|i| self.lower_expr(i));
                let ty = ty.map(|t| self.lower_ty(t));
                let lid = self.locals.push(HirLocal {
                    name: name.name.clone(),
                    mutable,
                    ty,
                    span: name.span.clone(),
                });
                self.scopes
                    .last_mut()
                    .expect("let outside any block scope")
                    .insert(name.name.clone(), lid);
                HirExprKind::Let { local: lid, init }
            }
            ast::ExprKind::Poison => HirExprKind::Poison,
        };

        self.exprs.push(HirExpr { kind, span })
    }

    fn lower_else_arm(&mut self, arm: ast::ElseArm) -> HElseArm {
        match arm {
            ast::ElseArm::Block(bid) => HElseArm::Block(self.lower_block(bid)),
            ast::ElseArm::If(eid) => HElseArm::If(self.lower_expr(eid)),
        }
    }

    /// Pass type-position names through unchanged. HIR doesn't resolve or
    /// intern types — that's typeck's job. For v0 (primitives only) this
    /// is fine; once user-defined types (struct/enum) land, type-namespace
    /// name resolution will need a similar prescan to what we do for fns,
    /// likely living in typeck rather than here.
    fn lower_ty(&mut self, tid: ast::TypeId) -> HirTy {
        let ty = &self.ast.types[tid];
        let kind = match &ty.kind {
            ast::TypeKind::Named(id) => HirTyKind::Named(id.name.clone()),
        };
        HirTy { kind, span: ty.span.clone() }
    }

    fn lower_char_lit(&mut self, c: char, span: &Span) -> HirExprKind {
        let value = c as u32;
        if value <= u8::MAX as u32 {
            HirExprKind::CharLit(value as u8)
        } else {
            self.errors.push(HirError::CharOutOfRange { ch: c, span: span.clone() });
            HirExprKind::Poison
        }
    }

    /// Look up `name` innermost-scope-first, then module scope. Emits
    /// `UnresolvedName` on miss and returns `Unresolved(name)` so typeck
    /// has something to walk.
    fn resolve_ident(&mut self, id: &ast::Ident) -> HirExprKind {
        for scope in self.scopes.iter().rev() {
            if let Some(&lid) = scope.get(&id.name) {
                return HirExprKind::Local(lid);
            }
        }
        if let Some(&fid) = self.module_scope.get(&id.name) {
            return HirExprKind::Fn(fid);
        }
        self.errors.push(HirError::UnresolvedName {
            name: id.name.clone(),
            span: id.span.clone(),
        });
        HirExprKind::Unresolved(id.name.clone())
    }

    fn finish(self) -> (HirModule, Vec<HirError>) {
        let module = HirModule {
            fns: self.fns,
            locals: self.locals,
            exprs: self.exprs,
            blocks: self.blocks,
            root_fns: self.root_fns,
            span: self.ast.span.clone(),
        };
        (module, self.errors)
    }
}
