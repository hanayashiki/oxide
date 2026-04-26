//! AST → HIR lowering. Resolves both value-namespace names (locals, fns)
//! and type-namespace names (user-defined ADTs). Primitive type names stay
//! as `HirTyKind::Named(_)` for typeck to interpret.
//!
//! Three-phase walk:
//!   1. Pre-scan all module items, allocating `FnId` per fn (including
//!      extern-block children) and `HAdtId` per struct, registering each
//!      name in the appropriate module-level scope.
//!   2. Walk each `HirAdt`, lowering its field type annotations against
//!      `ty_scopes`. Catches duplicate field names within an ADT.
//!   3. Walk each fn body, lowering expressions and resolving names
//!      (existing pass-2 work, now with type-namespace lookup wired in).

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use index_vec::IndexVec;

use crate::lexer::Span;
use crate::parser::{Ident, StructLitField, ast};

use super::ir::*;

pub fn lower(ast_module: &ast::Module) -> (HirModule, Vec<HirError>) {
    let mut lowerer = Lowerer::new(ast_module);
    let to_lower = lowerer.prescan_items();
    lowerer.resolve_adt_fields();
    for (fid, fn_decl) in to_lower {
        lowerer.lower_fn(fid, &fn_decl);
    }
    lowerer.finish()
}

struct Lowerer<'a> {
    ast: &'a ast::Module,
    fns: IndexVec<FnId, HirFn>,
    adts: IndexVec<HAdtId, HirAdt>,
    locals: IndexVec<LocalId, HirLocal>,
    exprs: IndexVec<HExprId, HirExpr>,
    blocks: IndexVec<HBlockId, HirBlock>,
    root_fns: Vec<FnId>,
    root_adts: Vec<HAdtId>,
    errors: Vec<HirError>,
    /// Module-level value-namespace scope (fn names). Forward references work.
    module_scope: HashMap<String, FnId>,
    /// Stack of block scopes for locals. Innermost is last; lookup is LIFO.
    scopes: Vec<HashMap<String, LocalId>>,
    /// Type-namespace scope stack. v0 only ever has a single (module-level)
    /// frame; the stack shape leaves room for fn-local type aliases / generic
    /// params later without churn.
    ty_scopes: Vec<HashMap<String, HAdtId>>,
    /// AST source for each adt's field decls, captured during pass 1 so
    /// pass 2 doesn't have to re-walk module items to find them.
    adt_field_sources: IndexVec<HAdtId, Vec<ast::FieldDecl>>,
}

impl<'a> Lowerer<'a> {
    fn new(ast: &'a ast::Module) -> Self {
        Self {
            ast,
            fns: IndexVec::new(),
            adts: IndexVec::new(),
            locals: IndexVec::new(),
            exprs: IndexVec::new(),
            blocks: IndexVec::new(),
            root_fns: Vec::new(),
            root_adts: Vec::new(),
            errors: Vec::new(),
            module_scope: HashMap::new(),
            scopes: Vec::new(),
            ty_scopes: vec![HashMap::new()],
            adt_field_sources: IndexVec::new(),
        }
    }

    /// Pass 1: allocate IDs for every module item (fn, extern-block child,
    /// struct), populate the module-level scopes. Returns the work list of
    /// `(FnId, FnDecl)` pairs that pass 3 walks.
    fn prescan_items(&mut self) -> Vec<(FnId, ast::FnDecl)> {
        let mut out = Vec::new();
        for &iid in &self.ast.root_items {
            let item = &self.ast.items[iid];
            match &item.kind {
                ast::ItemKind::Fn(fn_decl) => {
                    let fid = self.register_fn_stub(fn_decl, &item.span, false);
                    out.push((fid, fn_decl.clone()));
                }
                ast::ItemKind::ExternBlock(block) => {
                    for fn_decl in &block.items {
                        let fid = self.register_fn_stub(fn_decl, &block.span, true);
                        out.push((fid, fn_decl.clone()));
                    }
                }
                ast::ItemKind::Struct(s) => {
                    self.register_adt_stub(s, &item.span);
                }
            }
        }
        out
    }

    /// Allocate an `HirFn` stub, append to `root_fns`, and register the
    /// name in `module_scope` (emitting `DuplicateFn` on collision).
    fn register_fn_stub(&mut self, fn_decl: &ast::FnDecl, span: &Span, is_extern: bool) -> FnId {
        let fid = self.fns.push(HirFn {
            name: fn_decl.name.name.clone(),
            params: Vec::new(),
            ret_ty: None,
            body: None,
            is_extern,
            span: span.clone(),
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
                    dup: span.clone(),
                });
            }
        }
        fid
    }

    /// Allocate an `HirAdt` stub with empty fields, append to `root_adts`,
    /// register the name in the module-level type scope. Field types are
    /// resolved in pass 2.
    fn register_adt_stub(&mut self, decl: &ast::StructDecl, span: &Span) -> HAdtId {
        let haid = self.adts.push(HirAdt {
            name: decl.name.name.clone(),
            kind: AdtKind::Struct,
            variants: IndexVec::new(),
            span: span.clone(),
        });
        self.root_adts.push(haid);
        // The single source vector parallels `adts` — pass 2 reads
        // `adt_field_sources[haid]` to recover the AST field decls.
        self.adt_field_sources.push(decl.fields.clone());

        let cur_ty_scope = self
            .ty_scopes
            .last_mut()
            .expect("adt registered outside any ty scope");
        match cur_ty_scope.entry(decl.name.name.clone()) {
            Entry::Vacant(e) => {
                e.insert(haid);
            }
            Entry::Occupied(e) => {
                let first_haid = *e.get();
                self.errors.push(HirError::DuplicateAdt {
                    name: decl.name.name.clone(),
                    first: self.adts[first_haid].span.clone(),
                    dup: span.clone(),
                });
            }
        }
        haid
    }

    /// Pass 2: lower each ADT's field types now that every `HAdtId` is
    /// known. Catches duplicate field names within a single ADT.
    fn resolve_adt_fields(&mut self) {
        // Iterate by index so we can borrow self mutably inside the loop.
        for raw in 0..self.adts.len() {
            let haid = HAdtId::from_raw(raw as u32);
            let sources = self.adt_field_sources[haid].clone();
            let adt_name = self.adts[haid].name.clone();

            let mut fields: IndexVec<FieldIdx, HirField> = IndexVec::new();
            let mut seen: HashMap<String, Span> = HashMap::new();
            for fd in sources {
                let ty = self.lower_ty(fd.ty);
                if let Some(first_span) = seen.get(&fd.name.name) {
                    self.errors.push(HirError::DuplicateField {
                        adt: adt_name.clone(),
                        name: fd.name.name.clone(),
                        first: first_span.clone(),
                        dup: fd.name.span.clone(),
                    });
                    continue;
                }
                seen.insert(fd.name.name.clone(), fd.name.span.clone());
                fields.push(HirField {
                    name: fd.name.name.clone(),
                    ty,
                    span: fd.span.clone(),
                });
            }
            let span = self.adts[haid].span.clone();
            self.adts[haid].variants.push(HirVariant {
                name: None,
                fields,
                span,
            });
        }
    }

    /// Pass 3: lower a function. Lowers params + ret_ty for every fn;
    /// lowers the body block only when present (foreign fns skip).
    fn lower_fn(&mut self, fid: FnId, fn_decl: &ast::FnDecl) {
        // Outer scope holds parameters; the body block (if any) pushes its
        // own sub-scope on top.
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
        let body = fn_decl.body.map(|bid| self.lower_block(bid));

        self.scopes.pop();

        let hir_fn = &mut self.fns[fid];
        hir_fn.params = params;
        hir_fn.ret_ty = ret_ty;
        hir_fn.body = body;
    }

    fn lower_block(&mut self, bid: ast::BlockId) -> HBlockId {
        let block = &self.ast.blocks[bid];
        let span = block.span.clone();
        let raw_items: Vec<_> = block.items.clone();

        self.scopes.push(HashMap::new());
        let items: Vec<HBlockItem> = raw_items
            .into_iter()
            .map(|it| HBlockItem {
                expr: self.lower_expr(it.expr),
                has_semi: it.has_semi,
            })
            .collect();
        self.scopes.pop();

        self.blocks.push(HirBlock { items, span })
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
                HirExprKind::Field {
                    base,
                    name: name.name,
                }
            }
            ast::ExprKind::StructLit { name, fields } => self.lower_struct_lit(name, fields),
            ast::ExprKind::Cast { expr, ty } => {
                let expr = self.lower_expr(expr);
                let ty = self.lower_ty(ty);
                HirExprKind::Cast { expr, ty }
            }
            ast::ExprKind::If {
                cond,
                then_block,
                else_arm,
            } => {
                let cond = self.lower_expr(cond);
                let then_block = self.lower_block(then_block);
                let else_arm = else_arm.map(|arm| self.lower_else_arm(arm));
                HirExprKind::If {
                    cond,
                    then_block,
                    else_arm,
                }
            }
            ast::ExprKind::Block(bid) => HirExprKind::Block(self.lower_block(bid)),
            ast::ExprKind::Return(val) => {
                let val = val.map(|v| self.lower_expr(v));
                HirExprKind::Return(val)
            }
            ast::ExprKind::Let {
                mutable,
                name,
                ty,
                init,
            } => {
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

    /// Lower a type-position name. Looks up `Named(_)` in `ty_scopes`
    /// (innermost first) — a hit becomes `HirTyKind::Adt(haid)`. A miss
    /// stays `HirTyKind::Named(name)` for typeck to resolve as a primitive
    /// or report as unknown.
    fn lower_ty(&mut self, tid: ast::TypeId) -> HirTy {
        let ty = &self.ast.types[tid];
        let span = ty.span.clone();
        let kind = match &ty.kind {
            ast::TypeKind::Named(id) => match self.lookup_ty(&id.name) {
                Some(haid) => HirTyKind::Adt(haid),
                None => HirTyKind::Named(id.name.clone()),
            },
            ast::TypeKind::Ptr {
                mutability,
                pointee,
            } => {
                let pointee = Box::new(self.lower_ty(*pointee));
                HirTyKind::Ptr {
                    mutability: *mutability,
                    pointee,
                }
            }
        };
        HirTy { kind, span }
    }

    fn lookup_ty(&self, name: &str) -> Option<HAdtId> {
        for scope in self.ty_scopes.iter().rev() {
            if let Some(&haid) = scope.get(name) {
                return Some(haid);
            }
        }
        None
    }

    fn lower_struct_lit(
        &mut self,
        name: ast::Ident,
        fields: Vec<ast::StructLitField>,
    ) -> HirExprKind {
        match self.lookup_ty(&name.name) {
            Some(adt) => {
                let fields = fields
                    .into_iter()
                    .map(
                        |StructLitField {
                             name: Ident { name, .. },
                             value,
                             span,
                         }| HirStructLitField {
                            name,
                            value: self.lower_expr(value),
                            span,
                        },
                    )
                    .collect();
                HirExprKind::StructLit { adt, fields }
            }
            None => {
                // Lower the field expressions for side effects (so any
                // diagnostics inside them still surface) but discard the IDs.
                for StructLitField { value, .. } in fields {
                    let _ = self.lower_expr(value);
                }
                self.errors.push(HirError::UnresolvedAdt {
                    name: name.name.clone(),
                    span: name.span.clone(),
                });
                HirExprKind::Poison
            }
        }
    }

    fn lower_char_lit(&mut self, c: char, span: &Span) -> HirExprKind {
        let value = c as u32;
        if value <= u8::MAX as u32 {
            HirExprKind::CharLit(value as u8)
        } else {
            self.errors.push(HirError::CharOutOfRange {
                ch: c,
                span: span.clone(),
            });
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
            adts: self.adts,
            locals: self.locals,
            exprs: self.exprs,
            blocks: self.blocks,
            root_fns: self.root_fns,
            root_adts: self.root_adts,
            span: self.ast.span.clone(),
        };
        (module, self.errors)
    }
}
