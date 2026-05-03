//! AST → HIR body lowering. The scanner has already allocated every
//! `FnId`/`HAdtId`, built per-file resolution scopes, and sealed ADT
//! field types. This module walks each fn body once: lowers params,
//! ret_ty, and (when present) the body block — populating the
//! `locals`/`exprs`/`blocks` arenas and writing back into each
//! `HirFn` stub.
//!
//! Entry points:
//!   - `lower(&ast::Module)` — single-file convenience for tests; builds
//!     a one-element `IndexVec<FileId, LoadedFile>` with no imports
//!     and forwards to `lower_program`.
//!   - `lower_program(files, root)` — multi-file driver: scan, then
//!     body-lower each fn, then assemble `HirProgram`.
//!
//! Body lowering is structured around a `BodyCtx` view-struct that
//! borrows the scanner result whole plus the body-arena fields of
//! `Lowerer`. NLL field-disjointness lets `&mut scan.items.fns[..]`
//! coexist with `&scan.file_scopes[..]` *only* when both go through
//! raw field paths, never through accessor methods — so writeback
//! and scope lookup live as inline path expressions, not via the
//! `ScanResult` accessors.

use std::collections::HashMap;
use std::path::PathBuf;

use index_vec::IndexVec;

use crate::loader::LoadedFile;
use crate::parser::ast::UnOp;
use crate::parser::{Ident, StructLitField, ast};
use crate::reporter::{FileId, Span};

use crate::hir::{HirError, ir::*};

use super::scanner::{self, ModuleScopeCtx, ScanResult};
use super::ty;

/// Compute the `is_place` bit for an expression at construction time.
/// Children are already in `exprs` (children-first lowering), so the
/// projection arms can read child bits directly without recursion.
///
/// Rules per spec/08_ADT.md "Place expressions" and spec/07_POINTER.md:
///   - `Local(_)` — direct producer.
///   - `Unary { Deref, .. }` — direct producer; the operand need not be
///     a place (per 07_POINTER §HIR — `*make_ptr()` is fine).
///   - `Field { base, .. }` — projection; inherits from base.
///   - `Index { base, .. }` — projection; inherits from base.
///   - `Unresolved(_) | Poison` — recovery; treat as place to suppress
///     cascading "InvalidAssignTarget"-style errors when the underlying
///     issue (unresolved name, malformed expr) was already filed.
///   - everything else — value expression.
fn compute_is_place(kind: &HirExprKind, exprs: &IndexVec<HExprId, HirExpr>) -> bool {
    match kind {
        HirExprKind::Local(_) => true,
        HirExprKind::Unary {
            op: UnOp::Deref, ..
        } => true,
        HirExprKind::Field { base, .. } => exprs[*base].is_place,
        HirExprKind::Index { base, .. } => exprs[*base].is_place,
        HirExprKind::Unresolved(_) | HirExprKind::Poison => true,
        _ => false,
    }
}

/// Single-file convenience entry point. Wraps `ast` in a one-element
/// loaded-file list with no imports and forwards to `lower_program`.
pub fn lower(ast_module: &ast::Module) -> (HirProgram, Vec<HirError>) {
    let file = ast_module.span.file;
    let loaded = LoadedFile {
        file,
        path: PathBuf::new(),
        ast: ast_module.clone(),
        direct_imports: Vec::new(),
    };
    let files: IndexVec<FileId, LoadedFile> = IndexVec::from_vec(vec![loaded]);
    lower_program(files, file)
}

/// Multi-file driver: run the scanner, then body-lower every fn,
/// then assemble `HirProgram`.
pub fn lower_program(
    files: IndexVec<FileId, LoadedFile>,
    root: FileId,
) -> (HirProgram, Vec<HirError>) {
    assert!(!files.is_empty(), "lower_program: empty file list");

    let (scan, mut errors) = scanner::scan(&files);
    let mut lowerer = Lowerer {
        scan,
        files: &files,
        locals: IndexVec::new(),
        exprs: IndexVec::new(),
        blocks: IndexVec::new(),
        errors: Vec::new(),
    };
    lowerer.run();
    errors.append(&mut lowerer.errors);
    let program = lowerer.into_program(root);
    (program, errors)
}

struct Lowerer<'a> {
    scan: ScanResult,
    files: &'a IndexVec<FileId, LoadedFile>,
    locals: IndexVec<LocalId, HirLocal>,
    exprs: IndexVec<HExprId, HirExpr>,
    blocks: IndexVec<HBlockId, HirBlock>,
    errors: Vec<HirError>,
}

impl<'a> Lowerer<'a> {
    fn run(&mut self) {
        // `fn_work` is owned by the scan result; move it out so the loop
        // body is free to borrow `self.scan` for `BodyCtx`.
        let work = std::mem::take(&mut self.scan.fn_work);
        for (fid, iid) in work {
            let file = self.scan.items.fns[fid].span.file;
            let mut bcx = BodyCtx {
                file,
                files: self.files,
                scan: &mut self.scan,
                locals: &mut self.locals,
                exprs: &mut self.exprs,
                blocks: &mut self.blocks,
                errors: &mut self.errors,
                scopes: Vec::new(),
                loop_stack: Vec::new(),
            };
            bcx.lower_fn(fid, iid);
        }
    }

    /// Convert lowerer state into a `HirProgram`, using `root` as the
    /// program's root file. One `HirModule` per file.
    fn into_program(self, root: FileId) -> HirProgram {
        let n_files = self.files.len();
        let mut module_fns: Vec<Vec<FnId>> = vec![Vec::new(); n_files];
        let mut module_adts: Vec<Vec<HAdtId>> = vec![Vec::new(); n_files];

        // `iter_enumerated` over the program-wide arenas yields stubs in
        // prescan (= source) order; bucketing by `span.file` recovers
        // each file's source-ordered list with no parallel side-tables.
        for (fid, hir_fn) in self.scan.items.fns.iter_enumerated() {
            if let Some(slot) = module_fns.get_mut(hir_fn.span.file.0 as usize) {
                slot.push(fid);
            }
        }
        for (haid, hir_adt) in self.scan.items.adts.iter_enumerated() {
            if let Some(slot) = module_adts.get_mut(hir_adt.span.file.0 as usize) {
                slot.push(haid);
            }
        }

        let mut modules: IndexVec<FileId, HirModule> = IndexVec::with_capacity(n_files);
        for (i, (fns, adts)) in module_fns.into_iter().zip(module_adts.into_iter()).enumerate() {
            let file = FileId(i as u32);
            let span = self.files[file].ast.span.clone();
            modules.push(HirModule {
                file,
                root_fns: fns.clone(),
                root_adts: adts.clone(),
                fns,
                adts,
                span,
            });
        }

        HirProgram {
            fns: self.scan.items.fns,
            adts: self.scan.items.adts,
            locals: self.locals,
            exprs: self.exprs,
            blocks: self.blocks,
            modules,
            root,
        }
    }
}

/// One stack entry per enclosing loop being lowered. `has_break` is
/// flipped to `true` by `Break` lowering when this is the innermost
/// frame; the final value is captured into the resulting
/// `HirExprKind::Loop` node.
struct LoopFrame {
    has_break: bool,
}

/// View-struct for body lowering. Borrows the scanner result whole
/// (so `&scan.file_scopes[file]` and `&mut scan.items.fns[fid]`
/// disjoint-field reborrow inside method bodies) plus the
/// body-arena fields of `Lowerer`. The locals stack and loop stack
/// are owned here — they reset between fns and never leak.
struct BodyCtx<'a> {
    /// File of the fn currently being lowered. Together with `files`
    /// resolves the AST module; together with `scan.file_scopes`
    /// resolves the name-scope.
    file: FileId,
    files: &'a IndexVec<FileId, LoadedFile>,
    scan: &'a mut ScanResult,
    locals: &'a mut IndexVec<LocalId, HirLocal>,
    exprs: &'a mut IndexVec<HExprId, HirExpr>,
    blocks: &'a mut IndexVec<HBlockId, HirBlock>,
    errors: &'a mut Vec<HirError>,
    /// Stack of block scopes for locals. Innermost is last; lookup is LIFO.
    scopes: Vec<HashMap<String, LocalId>>,
    /// Stack of enclosing loops, one frame per `Loop` whose body is
    /// currently being lowered. `Break` lowers flip the innermost
    /// frame's `has_break = true`; an empty stack at `Break` /
    /// `Continue` time produces the matching HIR error.
    loop_stack: Vec<LoopFrame>,
}

impl<'a> BodyCtx<'a> {
    fn ast(&self) -> &ast::Module {
        &self.files[self.file].ast
    }

    fn scope(&self) -> &ModuleScopeCtx {
        self.scan
            .file_scopes
            .get(&self.file)
            .expect("scope built by scanner")
    }

    /// Lower a fn body. Reads the AST decl from `cx.ast.items[iid]` —
    /// the scanner records `(FnId, ItemId)` pairs in `fn_work` so the
    /// AST node never has to be cloned.
    ///
    /// Behavior on extern-fn-with-body recovery: the scanner already
    /// fired `ExternFnHasBody`. We still walk the body so any inner
    /// diagnostics surface and the user can move the fn out of the
    /// extern block as a no-op refactor — `HirFn.body` ends up
    /// `Some(...)` even with `is_extern: true`. Matches user intent.
    fn lower_fn(&mut self, fid: FnId, iid: ItemId_) {
        // Snapshot the AST decl's params/ret_ty/body — `params` is
        // cloned (Vec<Param>) since lowering it pushes locals through
        // self.locals, and we don't want a long-lived borrow into
        // self.files's ast arena across those mutations. ret_ty and
        // body are Copy IDs.
        let (params_ast, ret_ty_id, body_id) = {
            let item = &self.ast().items[iid];
            let ast::ItemKind::Fn(fn_decl) = &item.kind else {
                unreachable!("scanner recorded non-fn item in fn_work");
            };
            (fn_decl.params.clone(), fn_decl.ret_ty, fn_decl.body)
        };

        // Outer scope holds parameters; the body block (if any) pushes
        // its own sub-scope on top.
        self.scopes.push(HashMap::new());

        let mut params = Vec::with_capacity(params_ast.len());
        for p in &params_ast {
            let ty = ty::lower_ty(self.ast(), self.scope(), p.ty);
            let lid = self.locals.push(HirLocal {
                name: p.name.name.clone(),
                mutable: p.mutable,
                ty: Some(ty),
                span: p.span.clone(),
            });
            self.scopes
                .last_mut()
                .expect("pushed above")
                .insert(p.name.name.clone(), lid);
            params.push(lid);
        }

        let ret_ty = ret_ty_id.map(|t| ty::lower_ty(self.ast(), self.scope(), t));
        let body = body_id.map(|bid| self.lower_block(bid));

        self.scopes.pop();

        // Writeback. Raw field path (not the `ScanResult::get_fn_mut`
        // accessor) — accessor would reborrow all of `self.scan` and
        // hide field-disjointness from NLL, which we may need next
        // iteration when the next BodyCtx splits scope vs. fns.
        let fn_stub = &mut self.scan.items.fns[fid];
        fn_stub.params = params;
        fn_stub.ret_ty = ret_ty;
        fn_stub.body = body;
    }

    fn lower_block(&mut self, bid: ast::BlockId) -> HBlockId {
        let block = &self.ast().blocks[bid];
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
        let span = self.ast().exprs[eid].span.clone();

        // `Paren` is purely syntactic — drop the wrapper, return the inner.
        if let ast::ExprKind::Paren(inner) = &self.ast().exprs[eid].kind {
            let inner = *inner;
            return self.lower_expr(inner);
        }

        let kind_ast = self.ast().exprs[eid].kind.clone();
        let kind = match kind_ast {
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
                let ty = ty::lower_ty(self.ast(), self.scope(), ty);
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
                let ty = ty.map(|t| ty::lower_ty(self.ast(), self.scope(), t));
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
                    self.errors
                        .push(HirError::BreakOutsideLoop { span: span.clone() });
                }
                HirExprKind::Break { expr }
            }
            ast::ExprKind::Continue => {
                if self.loop_stack.is_empty() {
                    self.errors
                        .push(HirError::ContinueOutsideLoop { span: span.clone() });
                }
                HirExprKind::Continue
            }
        };

        let is_place = compute_is_place(&kind, self.exprs);
        self.exprs.push(HirExpr {
            kind,
            span,
            is_place,
        })
    }

    fn lower_else_arm(&mut self, arm: ast::ElseArm) -> HElseArm {
        match arm {
            ast::ElseArm::Block(bid) => HElseArm::Block(self.lower_block(bid)),
            ast::ElseArm::If(eid) => HElseArm::If(self.lower_expr(eid)),
        }
    }

    /// Single point of truth for `while` / `loop` / `for` lowering. See
    /// spec/13_LOOPS.md "Scope of the `Loop` node" — the for-scope is
    /// pushed here so `let i = 0` in `init` is visible to
    /// `cond`/`update`/`body` and gone after the loop.
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
        // init/cond/update targets the enclosing loop, not this one.
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

    fn lower_array_lit(&mut self, lit: ast::ArrayLit) -> HirExprKind {
        match lit {
            ast::ArrayLit::Elems(es) => {
                let elems: Vec<_> = es.into_iter().map(|e| self.lower_expr(e)).collect();
                HirExprKind::ArrayLit(HirArrayLit::Elems(elems))
            }
            ast::ArrayLit::Repeat { init, len } => {
                let init = self.lower_expr(init);
                let len = ty::extract_length_const(self.ast(), len);
                HirExprKind::ArrayLit(HirArrayLit::Repeat { init, len })
            }
        }
    }

    fn lower_struct_lit(
        &mut self,
        name: ast::Ident,
        fields: Vec<ast::StructLitField>,
    ) -> HirExprKind {
        match self.scope().lookup_type(&name.name) {
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

    /// Look up `name` innermost-scope-first, then file scope. Emits
    /// `UnresolvedName` on miss and returns `Unresolved(name)` so typeck
    /// has something to walk.
    fn resolve_ident(&mut self, id: &ast::Ident) -> HirExprKind {
        for scope in self.scopes.iter().rev() {
            if let Some(&lid) = scope.get(&id.name) {
                return HirExprKind::Local(lid);
            }
        }
        if let Some(fid) = self.scope().lookup_value(&id.name) {
            return HirExprKind::Fn(fid);
        }
        self.errors.push(HirError::UnresolvedName {
            name: id.name.clone(),
            span: id.span.clone(),
        });
        HirExprKind::Unresolved(id.name.clone())
    }
}

// ItemId is parser::ast::ItemId; alias to keep the `lower_fn` signature
// readable without dragging the full ast:: prefix into impl blocks.
type ItemId_ = ast::ItemId;
