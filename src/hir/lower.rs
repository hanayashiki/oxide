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

use crate::parser::ast::UnOp;
use crate::reporter::Span;
use crate::parser::{Ident, StructLitField, ast};

use super::ir::*;

/// Compute the `is_place` bit for an expression at construction time.
/// Children are already in `exprs` (children-first lowering), so the
/// projection arms can read child bits directly without recursion.
///
/// Rules per spec/08_ADT.md "Place expressions" and spec/07_POINTER.md:
///   - `Local(_)` — direct producer.
///   - `Unary { Deref, .. }` — direct producer; the operand need not be
///     a place (per 07_POINTER §HIR — `*make_ptr()` is fine).
///   - `Field { base, .. }` — projection; inherits from base.
///   - `Index { base, .. }` — projection; inherits from base. Indexing
///     itself is `UnsupportedFeature` at typeck today, but the place
///     rule lives at the structural HIR layer, so we wire it now and
///     it stays correct when arrays land.
///   - `Unresolved(_) | Poison` — recovery; treat as place to suppress
///     cascading "InvalidAssignTarget"-style errors when the underlying
///     issue (unresolved name, malformed expr) was already filed.
///   - everything else — value expression.
fn compute_is_place(kind: &HirExprKind, exprs: &IndexVec<HExprId, HirExpr>) -> bool {
    match kind {
        HirExprKind::Local(_) => true,
        HirExprKind::Unary { op: UnOp::Deref, .. } => true,
        HirExprKind::Field { base, .. } => exprs[*base].is_place,
        HirExprKind::Index { base, .. } => exprs[*base].is_place,
        HirExprKind::Unresolved(_) | HirExprKind::Poison => true,
        _ => false,
    }
}

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
    /// Stack of enclosing loops, one frame per `Loop` whose body is
    /// currently being lowered. The frame is pushed *after* the loop's
    /// header (`init`/`cond`/`update`) is lowered and popped after the
    /// body — so a `break` / `continue` in init/cond/update targets the
    /// outer loop, matching spec/13_LOOPS.md "Lowering". `Break` lowers
    /// flip the innermost frame's `has_break = true`; an empty stack at
    /// `Break` / `Continue` time produces the matching HIR error.
    loop_stack: Vec<LoopFrame>,
}

/// One stack entry per enclosing loop being lowered. `has_break` is set
/// to `true` by `Break` lowering when this is the innermost frame; the
/// final value is captured into the resulting `HirExprKind::Loop` node.
struct LoopFrame {
    has_break: bool,
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
            loop_stack: Vec::new(),
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
                // Imports are consumed once the loader + lower_program land
                // (Steps 5+7). Until then, swallow them at HIR-lower time.
                ast::ItemKind::Import(_) => {}
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

        let mut params = Vec::with_capacity(fn_decl.params.len());
        for p in &fn_decl.params {
            let ty = self.lower_ty(p.ty);
            let lid = self.locals.push(HirLocal {
                name: p.name.name.clone(),
                mutable: p.mutable,
                ty: Some(ty),
                span: p.span.clone(),
            });
            self.scopes.last_mut().unwrap().insert(p.name.name.clone(), lid);
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
            ast::ExprKind::Null => HirExprKind::Null,
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
                if !self.exprs[target].is_place {
                    self.errors.push(HirError::InvalidAssignTarget {
                        span: self.exprs[target].span.clone(),
                    });
                }
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
            ast::ExprKind::AddrOf { mutability, expr } => {
                let inner = self.lower_expr(expr);
                if !self.exprs[inner].is_place {
                    self.errors.push(HirError::AddrOfNonPlace {
                        span: self.exprs[inner].span.clone(),
                    });
                }
                HirExprKind::AddrOf {
                    mutability,
                    expr: inner,
                }
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
            ast::ExprKind::ArrayLit(lit) => self.lower_array_lit(lit),
            ast::ExprKind::While { cond, body } => {
                self.lower_loop(None, Some(cond), None, body, LoopSource::While)
            }
            ast::ExprKind::Loop { body } => {
                self.lower_loop(None, None, None, body, LoopSource::Loop)
            }
            ast::ExprKind::For {
                init,
                cond,
                update,
                body,
            } => self.lower_loop(init, cond, update, body, LoopSource::For),
            ast::ExprKind::Break { expr } => {
                // Lower operand first so a nested loop inside the operand
                // (e.g. `break (loop { break 5; })`) gets its own frame
                // managed correctly before we touch this frame.
                let expr = expr.map(|e| self.lower_expr(e));
                if let Some(top) = self.loop_stack.last_mut() {
                    top.has_break = true;
                } else {
                    self.errors.push(HirError::BreakOutsideLoop {
                        span: span.clone(),
                    });
                }
                HirExprKind::Break { expr }
            }
            ast::ExprKind::Continue => {
                if self.loop_stack.is_empty() {
                    self.errors.push(HirError::ContinueOutsideLoop {
                        span: span.clone(),
                    });
                }
                HirExprKind::Continue
            }
        };

        let is_place = compute_is_place(&kind, &self.exprs);
        self.exprs.push(HirExpr { kind, span, is_place })
    }

    fn lower_else_arm(&mut self, arm: ast::ElseArm) -> HElseArm {
        match arm {
            ast::ElseArm::Block(bid) => HElseArm::Block(self.lower_block(bid)),
            ast::ElseArm::If(eid) => HElseArm::If(self.lower_expr(eid)),
        }
    }

    /// Single point of truth for `while` / `loop` / `for` lowering. Each
    /// surface form supplies a different subset of the optional header
    /// slots; the structural rule (cond.is_some() / has_break) is what
    /// drives later layers, not `source`.
    ///
    /// Scope shape (spec/13_LOOPS.md "Scope of the `Loop` node"):
    ///
    /// ```text
    /// { for-scope ──────────────────────────────────────┐
    ///     init / cond / update                          │
    ///     { body-scope ────────────────────────┐        │
    ///         body items                       │        │
    ///     } ───────────────────────────────────┘        │
    /// } ────────────────────────────────────────────────┘
    /// ```
    ///
    /// We push the for-scope here so `let i = 0` in `init` is visible to
    /// `cond` / `update` / `body` and gone after the loop. `lower_block`
    /// adds its own body-scope on top — strict subset, matching C99
    /// §6.8.5/5. The for-scope is pushed for `while` / `loop` too even
    /// though they have no init; an empty extra scope is a no-op and
    /// keeps this helper uniform.
    fn lower_loop(
        &mut self,
        init: Option<ast::ExprId>,
        cond: Option<ast::ExprId>,
        update: Option<ast::ExprId>,
        body: ast::BlockId,
        source: LoopSource,
    ) -> HirExprKind {
        self.scopes.push(HashMap::new());

        let init = init.map(|e| self.lower_expr(e));
        let cond = cond.map(|e| self.lower_expr(e));
        let update = update.map(|e| self.lower_expr(e));

        // Loop frame is pushed *after* header lowering — a `break` inside
        // init/cond/update targets the enclosing loop, not this one. See
        // spec/13_LOOPS.md "Lowering" pseudocode.
        self.loop_stack.push(LoopFrame { has_break: false });
        let body = self.lower_block(body);
        let frame = self.loop_stack.pop().expect("pushed above");

        self.scopes.pop();

        HirExprKind::Loop {
            init,
            cond,
            update,
            body,
            has_break: frame.has_break,
            source,
        }
    }

    /// Lower an array literal. Element-list form (`[a, b, c]`) lowers each
    /// element through the normal expression pipeline. Repeat form
    /// (`[init; N]`) lowers `init` and extracts `N` to a `HirConst` via
    /// `extract_length_const`.
    fn lower_array_lit(&mut self, lit: ast::ArrayLit) -> HirExprKind {
        match lit {
            ast::ArrayLit::Elems(es) => {
                let elems: Vec<_> = es.into_iter().map(|e| self.lower_expr(e)).collect();
                HirExprKind::ArrayLit(HirArrayLit::Elems(elems))
            }
            ast::ArrayLit::Repeat { init, len } => {
                let init = self.lower_expr(init);
                let len = self.extract_length_const(len);
                HirExprKind::ArrayLit(HirArrayLit::Repeat { init, len })
            }
        }
    }

    /// Extract a `HirConst` from an AST length expression. Per
    /// spec/09_ARRAY.md "Length literal extraction", v0 only accepts a
    /// bare `IntLit` token — and the parser already enforces that
    /// (the length slot in `[T; N]` and `[init; N]` only matches an
    /// `Int` token). This is therefore a structural pattern match
    /// with no error path. Future work (an ICE evaluator, `const`
    /// items, const generics) relaxes the parser and extends this
    /// match.
    fn extract_length_const(&mut self, eid: ast::ExprId) -> HirConst {
        let expr = &self.ast.exprs[eid];
        match &expr.kind {
            ast::ExprKind::IntLit(n) => HirConst::Lit(*n),
            other => unreachable!(
                "parser ensures length slot is IntLit; got {other:?}"
            ),
        }
    }

    /// Lower a type-position name in **value position** — the type must
    /// be sized. Looks up `Named(_)` in `ty_scopes` (innermost first); a
    /// hit becomes `HirTyKind::Adt(haid)`. A miss stays
    /// `HirTyKind::Named(name)` for typeck to resolve.
    ///
    /// **Note:** unsized-array-in-value-position rejection happens at
    /// **typeck**, not here. HIR doesn't fully resolve types — a future
    /// `type Buf = [i32]` alias would be `Named("Buf")` at HIR with the
    /// unsized shape only visible after typeck resolves the alias. So
    /// the structural `Array(_, None)` check at HIR would catch only
    /// the syntactic case, missing aliased ones, while typeck has to
    /// run the check anyway. Keeping it in one place (typeck) avoids
    /// duplicating an incomplete check.
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
            ast::TypeKind::Array { elem, len } => {
                let elem = Box::new(self.lower_ty(*elem));
                let len_const = len.map(|eid| self.extract_length_const(eid));
                HirTyKind::Array(elem, len_const)
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
