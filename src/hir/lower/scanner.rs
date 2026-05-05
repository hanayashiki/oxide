use index_vec::IndexVec;

use crate::{
    hir::{HirError, ir::*},
    loader::{INTRINSICS_FILE, LoadedFile},
    reporter::{FileId, Span},
};

use super::ty;

use crate::parser::ast::{self, *};
use std::{cell::RefCell, collections::HashMap};

/// Name → `Intrinsic` lookup. **Single source of truth for the intrinsic
/// allowlist** — every recognized intrinsic name has an arm here, and
/// nowhere else. See spec/17_LAYOUT.md §Intrinsic recognition.
fn name_to_intrinsic(name: &str) -> Option<Intrinsic> {
    match name {
        "ox_transmute" => Some(Intrinsic::Transmute),
        "ox_size_of" => Some(Intrinsic::SizeOf),
        _ => None,
    }
}

/// Identifier in the value namespace. Both `fn` items and `const`
/// items occupy this namespace; lookup yields one of these. See
/// spec/18_CONST.md.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ValueId {
    Fn(FnId),
    Const(ConstId),
}

#[derive(Default)]
pub(super) struct ModuleScope {
    pub types: HashMap<String, HAdtId>,
    pub values: HashMap<String, ValueId>,
}

/// Per-file scoping.
/// When conflict arises, priority of resolution:
/// 1. local items
/// 2. items from imported modules
///
/// Rule: local wins import; last import wins + last definition wins
///
/// A conflict is a duplication error.
#[derive(Default)]
pub(super) struct ModuleScopeCtx {
    pub local_scope: ModuleScope,
    pub import_scope: ModuleScope,
}

impl ModuleScopeCtx {
    /// Resolve a value-namespace name. Local first, then import.
    pub fn lookup_value(&self, name: &str) -> Option<ValueId> {
        self.local_scope
            .values
            .get(name)
            .copied()
            .or_else(|| self.import_scope.values.get(name).copied())
    }

    /// Resolve a type-namespace name. Local first, then import.
    pub fn lookup_type(&self, name: &str) -> Option<HAdtId> {
        self.local_scope
            .types
            .get(name)
            .copied()
            .or_else(|| self.import_scope.types.get(name).copied())
    }
}

/// Flat list of all items
#[derive(Default)]
pub(super) struct ProgramItems {
    pub adts: IndexVec<HAdtId, HirAdt>,
    pub fns: IndexVec<FnId, HirFn>,
    /// All `const` items in the program. Allocated at scanner prescan;
    /// used by typeck (`const_tys`) and codegen (`emit_expr` Const arm).
    /// See spec/18_CONST.md.
    pub consts: IndexVec<ConstId, HirConstItem>,
    /// All type parameters in the program, keyed by `HTyParamId`.
    /// Populated at prescan time so `lower_ty` can resolve `Named(T)`
    /// → `Param(tpid)` during body lowering. See spec/16_GENERIC.md §HIR.
    pub ty_params: IndexVec<HTyParamId, TyParamInfo>,
}

#[derive(Default)]
struct Scanner {
    items: ProgramItems,
    scopes: HashMap<FileId, ModuleScopeCtx>,
    /// Body-lowering work list, in source/prescan order. Owning file
    /// is `items.fns[fid].span.file`.
    fn_work: Vec<(FnId, ItemId)>,
    /// Per-ADT raw `FieldDecl`s captured during prescan; consumed by
    /// `seal_adts`. Parallels `items.adts` by `HAdtId`.
    adt_field_decls: IndexVec<HAdtId, Vec<ast::FieldDecl>>,
    /// Per-const raw type-position AST id captured during prescan.
    /// Lowered later (see `seal_consts`) once every ADT's name is
    /// registered in its file's scope, so a const annotation can
    /// reference a user-defined ADT declared later in source. See
    /// spec/18_CONST.md.
    const_ty_decls: IndexVec<ConstId, ast::TypeId>,
    errors: RefCell<Vec<HirError>>,
}

#[derive(Clone, Copy)]
struct ScanCtx<'a> {
    ast: &'a Module,

    /// Are we under extern "C" context?
    in_extern_c: bool,
}

pub(super) struct ScanResult {
    pub items: ProgramItems,
    pub file_scopes: HashMap<FileId, ModuleScopeCtx>,
    pub fn_work: Vec<(FnId, ItemId)>,
}

impl<'a> ScanCtx<'a> {
    fn get_item(&self, iid: ItemId) -> &'a Item {
        &self.ast.items[iid]
    }
}

pub(super) fn scan(files: &IndexVec<FileId, LoadedFile>) -> (ScanResult, Vec<HirError>) {
    let mut scanner = Scanner::default();

    // Pass 1: prescan each file for item names, leaving content empty, building local scopes.
    for file in files {
        scanner.prescan_file(file);
    }

    // Pass 2: build import scopes, resolving conflicts
    for file in files {
        scanner.build_import_scopes(file, files);
    }

    // Pass 3: check for global conflicts across files
    scanner.check_global_conflicts(files);

    // Pass 4a: lower each ADT's field types under its origin file's scope.
    scanner.seal_adts(files);

    // Pass 4b: lower each const's annotated type under its origin
    // file's scope. Deferred from prescan so a const can name an ADT
    // declared later in source. Empty TyParamScope (consts are not
    // generic in v0). See spec/18_CONST.md.
    scanner.seal_consts(files);

    (
        ScanResult {
            items: scanner.items,
            file_scopes: scanner.scopes,
            fn_work: scanner.fn_work,
        },
        scanner.errors.into_inner(),
    )
}

impl Scanner {
    fn get_fn(&self, fn_id: FnId) -> &HirFn {
        &self.items.fns[fn_id]
    }

    fn get_adt(&self, adt_id: HAdtId) -> &HirAdt {
        &self.items.adts[adt_id]
    }

    fn get_const(&self, cid: ConstId) -> &HirConstItem {
        &self.items.consts[cid]
    }

    /// Span of a value-namespace entry — fn or const. Used by the
    /// duplicate-detection arms to attribute "first defined here" /
    /// "duplicate definition" labels uniformly. See spec/18_CONST.md.
    fn value_span(&self, vid: ValueId) -> Span {
        match vid {
            ValueId::Fn(fid) => self.get_fn(fid).span.clone(),
            ValueId::Const(cid) => self.get_const(cid).span.clone(),
        }
    }

    fn get_scope_ctx_mut(&mut self, file: FileId) -> &mut ModuleScopeCtx {
        self.scopes.entry(file).or_default()
    }

    fn get_scope_ctx(&self, file: FileId) -> Option<&ModuleScopeCtx> {
        self.scopes.get(&file)
    }

    fn get_local_scope_mut(&mut self, file: FileId) -> &mut ModuleScope {
        &mut self.get_scope_ctx_mut(file).local_scope
    }

    fn get_local_scope(&self, file: FileId) -> Option<&ModuleScope> {
        self.get_scope_ctx(file).map(|ctx| &ctx.local_scope)
    }

    fn get_import_scope_mut(&mut self, file: FileId) -> &mut ModuleScope {
        &mut self.get_scope_ctx_mut(file).import_scope
    }

    /// Mutably borrow the error list to push an error.
    /// Tricks the borrow checker by only borrowing for the duration of this function call, allowing multiple
    /// non-overlapping borrows across the scanning process.
    fn emit_error(&self, error: HirError) {
        self.errors.borrow_mut().push(error);
    }

    fn prescan_file(&mut self, file: &LoadedFile) {
        self.prescan_items(
            file,
            &file.ast.root_items,
            ScanCtx {
                ast: &file.ast,
                in_extern_c: false,
            },
        );
    }

    fn prescan_items(&mut self, file: &LoadedFile, items: &[ItemId], scan_ctx: ScanCtx) {
        for &iid in items {
            let item = scan_ctx.get_item(iid);

            // v0: only `fn` declarations may appear inside `extern "C"`
            // blocks. Non-fn children (struct, nested extern, import)
            // are filed as `UnsupportedExternItem` with the source-
            // form label and skipped — no stub allocated.
            if scan_ctx.in_extern_c && !matches!(item.kind, ItemKind::Fn(_)) {
                self.emit_error(HirError::UnsupportedExternItem {
                    kind: item.kind.label().to_string(),
                    span: item.span.clone(),
                });
                continue;
            }

            match &item.kind {
                ItemKind::Fn(fn_decl) => {
                    let (name, span) = (&fn_decl.name.name, item.span.clone());

                    let fn_id = self.items.fns.push(HirFn {
                        name: name.clone(),
                        span: span.clone(),
                        is_extern: scan_ctx.in_extern_c,
                        is_variadic: fn_decl.is_variadic,
                        ..HirFn::default()
                    });

                    // Mint HTyParamIds for the fn's generic params and
                    // store them on the stub. Done at prescan because
                    // `lower_ty` (called from body lowering) needs the
                    // IDs allocated *before* any body walk runs, so
                    // `Named(T)` can resolve to `Param(tpid)`. The
                    // arena is global on `HirProgram.ty_params`.
                    // See spec/16_GENERIC.md §HIR.
                    let mut generic_params = Vec::with_capacity(fn_decl.generic_params.len());
                    for (idx, gp_ident) in fn_decl.generic_params.iter().enumerate() {
                        let tpid = self.items.ty_params.push(TyParamInfo {
                            owner: TyParamOwner::Fn(fn_id),
                            idx_in_owner: idx as u32,
                            name: gp_ident.name.clone(),
                            span: gp_ident.span.clone(),
                        });
                        generic_params.push(tpid);
                    }
                    self.items.fns[fn_id].generic_params = generic_params;

                    // Insert and check in one shot. `HashMap.insert`
                    // returns the displaced (older) entry — that's
                    // the *first* definition; the one we just pushed
                    // is the duplicate. fn-vs-fn keeps `DuplicateFn`
                    // for diagnostic continuity; fn-vs-const routes
                    // through `DuplicateValueSymbol` since consts and
                    // fns share the value namespace. See
                    // spec/18_CONST.md.
                    if let Some(displaced) = self
                        .get_local_scope_mut(file.file)
                        .values
                        .insert(name.clone(), ValueId::Fn(fn_id))
                    {
                        let dup = self.get_fn(fn_id).span.clone();
                        match displaced {
                            ValueId::Fn(displaced_fn_id) => {
                                self.emit_error(HirError::DuplicateFn {
                                    name: name.clone(),
                                    first: self.get_fn(displaced_fn_id).span.clone(),
                                    dup,
                                });
                            }
                            ValueId::Const(displaced_cid) => {
                                self.emit_error(HirError::DuplicateValueSymbol {
                                    name: name.clone(),
                                    first: self.get_const(displaced_cid).span.clone(),
                                    dup,
                                });
                            }
                        }
                    }

                    if scan_ctx.in_extern_c {
                        if fn_decl.body.is_some() {
                            self.emit_error(HirError::ExternFnHasBody {
                                name: name.clone(),
                                span: span.clone(),
                            });
                        }
                        // spec/16: extern "C" fns cannot be generic.
                        // Recovery contract: signature kept intact;
                        // driver short-circuits on HirError so
                        // downstream phases never see the contradictory
                        // `is_extern && !generic_params.is_empty()`.
                        if !fn_decl.generic_params.is_empty() {
                            self.emit_error(HirError::GenericExternFn {
                                name: name.clone(),
                                span: span.clone(),
                            });
                        }
                    } else if fn_decl.body.is_none() {
                        // Body-less non-`extern` fn. Two-gate intrinsic
                        // recognition (spec/17_LAYOUT.md §Intrinsic
                        // recognition):
                        //   1. file gate: the source file is the bundled
                        //      `stdlib/intrinsics.ox`,
                        //   2. name gate: `name_to_intrinsic(...)` returns
                        //      `Some(_)`.
                        // Both true → stamp the intrinsic flag and skip
                        // E0209. Otherwise (user file, or unknown name in
                        // intrinsics.ox) E0209 still fires.
                        let in_intrinsics_file = &file.path == INTRINSICS_FILE;
                        match (in_intrinsics_file, name_to_intrinsic(name)) {
                            (true, Some(intr)) => {
                                self.items.fns[fn_id].intrinsic = Some(intr);
                            }
                            _ => {
                                self.emit_error(HirError::BodylessFnOutsideExtern {
                                    name: name.clone(),
                                    span: span.clone(),
                                });
                            }
                        }
                    }

                    self.fn_work.push((fn_id, iid));
                }
                ItemKind::ExternBlock(block) => {
                    self.prescan_items(
                        file,
                        block.items.as_slice(),
                        ScanCtx {
                            in_extern_c: true,
                            ..scan_ctx
                        },
                    );
                }
                ItemKind::Struct(struct_decl) => {
                    let (name, span) = (&struct_decl.name.name, item.span.clone());

                    let adt_id = self.items.adts.push(HirAdt {
                        name: name.clone(),
                        kind: AdtKind::Struct,
                        span: span.clone(),
                        ..HirAdt::default()
                    });
                    // Field decls captured for `seal_adts`.
                    self.adt_field_decls.push(struct_decl.fields.clone());

                    // Mint HTyParamIds for the ADT's generic params,
                    // mirror of the fn arm above. Done at prescan
                    // because `seal_adts` (Pass 4a) lowers field types
                    // and needs the IDs allocated to build a per-ADT
                    // `TyParamScope`. See spec/16_GENERIC.md §HIR
                    // (extension).
                    let mut generic_params = Vec::with_capacity(struct_decl.generic_params.len());
                    for (idx, gp_ident) in struct_decl.generic_params.iter().enumerate() {
                        let tpid = self.items.ty_params.push(TyParamInfo {
                            owner: TyParamOwner::Adt(adt_id),
                            idx_in_owner: idx as u32,
                            name: gp_ident.name.clone(),
                            span: gp_ident.span.clone(),
                        });
                        generic_params.push(tpid);
                    }
                    self.items.adts[adt_id].generic_params = generic_params;

                    if let Some(displaced_adt_id) = self
                        .get_local_scope_mut(file.file)
                        .types
                        .insert(name.clone(), adt_id)
                    {
                        self.emit_error(HirError::DuplicateAdt {
                            name: name.clone(),
                            first: self.get_adt(displaced_adt_id).span.clone(),
                            dup: self.get_adt(adt_id).span.clone(),
                        });
                    };
                }
                ItemKind::Import(_) => {
                    // imports are resolved at the loader
                }
                ItemKind::Const(const_decl) => {
                    let (name, span) = (&const_decl.name.name, item.span.clone());

                    // Extract literal value. Parser pinned RHS to a
                    // literal — anything else is unreachable. CharLit
                    // gets the same out-of-range guard as
                    // `lower_char_lit` in the body lowerer.
                    let value = match &scan_ctx.ast.exprs[const_decl.value].kind {
                        ast::ExprKind::IntLit(n) => HirConstValue::Int(*n),
                        ast::ExprKind::BoolLit(b) => HirConstValue::Bool(*b),
                        ast::ExprKind::CharLit(c) => {
                            let v = *c as u32;
                            if v <= u8::MAX as u32 {
                                HirConstValue::Char(v as u8)
                            } else {
                                self.emit_error(HirError::CharOutOfRange {
                                    ch: *c,
                                    span: scan_ctx.ast.exprs[const_decl.value].span.clone(),
                                });
                                HirConstValue::Char(0)
                            }
                        }
                        ast::ExprKind::StrLit(s) => HirConstValue::Str(s.clone()),
                        other => unreachable!(
                            "parser ensures const RHS is one of IntLit/BoolLit/CharLit/StrLit; got {other:?}"
                        ),
                    };

                    // Push a stub with a placeholder `HirTy` (Error).
                    // The real annotation is lowered in `seal_consts`
                    // (Pass 4b), after every ADT's name has been
                    // registered, so a const can name an ADT declared
                    // later in source. Same shape as how ADT field
                    // types are deferred to `seal_adts`. See
                    // spec/18_CONST.md.
                    let placeholder_ty = HirTy {
                        kind: HirTyKind::Error,
                        span: span.clone(),
                    };
                    let cid = self.items.consts.push(HirConstItem {
                        name: name.clone(),
                        ty: placeholder_ty,
                        value,
                        span: span.clone(),
                    });
                    self.const_ty_decls.push(const_decl.ty);

                    if let Some(displaced) = self
                        .get_local_scope_mut(file.file)
                        .values
                        .insert(name.clone(), ValueId::Const(cid))
                    {
                        let dup = self.get_const(cid).span.clone();
                        let first = self.value_span(displaced);
                        self.emit_error(HirError::DuplicateValueSymbol {
                            name: name.clone(),
                            first,
                            dup,
                        });
                    }
                }
            };
        }
    }

    fn build_import_scopes(&mut self, file: &LoadedFile, files: &IndexVec<FileId, LoadedFile>) {
        for &import in &file.direct_imports {
            let imported_file = &files[import];

            // Values namespace. Values are now `ValueId` (fn or const)
            // — both kinds flow through the same import path. Cross-
            // kind collisions route through `DuplicateValueSymbol`;
            // fn-vs-fn keeps `DuplicateFn` for diagnostic continuity.
            // See spec/18_CONST.md.
            let imported_values: Vec<(String, ValueId)> = self
                .get_local_scope(imported_file.file)
                .into_iter()
                .flat_map(|s| s.values.iter())
                .map(|(name, &vid)| (name.clone(), vid))
                .collect();
            for (imported_name, imported_vid) in imported_values {
                if let Some(old) = self
                    .get_import_scope_mut(file.file)
                    .values
                    .insert(imported_name.clone(), imported_vid)
                    .or_else(|| {
                        self.get_local_scope(file.file)
                            .and_then(|s| s.values.get(&imported_name).copied())
                    })
                {
                    let first = self.value_span(old);
                    let dup = self.value_span(imported_vid);
                    match (old, imported_vid) {
                        (ValueId::Fn(_), ValueId::Fn(_)) => {
                            self.emit_error(HirError::DuplicateFn {
                                name: imported_name,
                                first,
                                dup,
                            });
                        }
                        _ => {
                            self.emit_error(HirError::DuplicateValueSymbol {
                                name: imported_name,
                                first,
                                dup,
                            });
                        }
                    }
                }
            }

            // Types namespace — same shape, different error.
            let imported_types = self
                .get_local_scope(imported_file.file)
                .into_iter()
                .flat_map(|s| s.types.iter())
                .map(|(name, &haid)| (name.clone(), haid))
                .collect::<Vec<_>>();
            for (imported_name, imported_haid) in imported_types {
                if let Some(old) = self
                    .get_import_scope_mut(file.file)
                    .types
                    .insert(imported_name.clone(), imported_haid)
                    .or_else(|| {
                        self.get_local_scope(file.file)
                            .and_then(|s| s.types.get(&imported_name).copied())
                    })
                {
                    self.emit_error(HirError::DuplicateAdt {
                        name: imported_name,
                        first: self.get_adt(old).span.clone(),
                        dup: self.get_adt(imported_haid).span.clone(),
                    });
                }
            }
        }
    }

    fn check_global_conflicts(&mut self, files: &IndexVec<FileId, LoadedFile>) {
        // Cross-file value-namespace dup check. Values are `ValueId`
        // (fn or const); cross-kind collisions route through
        // `DuplicateGlobalSymbol` just like the same-kind cases — the
        // renderer doesn't distinguish, since the labels are the
        // spans. See spec/18_CONST.md.
        let mut seen_value: HashMap<&String, ValueId> = HashMap::default();
        let mut seen_adt_id = HashMap::<&String, HAdtId>::default();

        for file in files {
            // Values namespace.
            for (name, &vid) in self
                .get_local_scope(file.file)
                .iter()
                .flat_map(|s| &s.values)
            {
                if let Some(&first) = seen_value.get(name) {
                    let first_span = self.value_span(first);
                    let dup_span = self.value_span(vid);
                    self.emit_error(HirError::DuplicateGlobalSymbol {
                        name: name.clone(),
                        first: first_span,
                        dup: dup_span,
                        root: file.file,
                    });
                }
                seen_value.insert(name, vid);
            }
            // Types namespace.
            for (name, &haid) in self
                .get_local_scope(file.file)
                .iter()
                .flat_map(|s| &s.types)
            {
                if let Some(&first) = seen_adt_id.get(name) {
                    self.emit_error(HirError::DuplicateGlobalSymbol {
                        name: name.clone(),
                        first: self.get_adt(first).span.clone(),
                        dup: self.get_adt(haid).span.clone(),
                        root: file.file,
                    });
                }
                seen_adt_id.insert(name, haid);
            }
        }
    }

    /// Pass 4a: lower each ADT's field types under its origin file's
    /// resolution scope and the ADT's own generic-param scope. Walks
    /// all ADTs in `HAdtId` (= prescan = source) order.
    /// `DuplicateField` recovery skips the duplicate but keeps
    /// subsequent fields.
    ///
    /// The per-ADT type-param scope is built from `HirAdt.generic_params`
    /// (minted in prescan). This is what makes `value: T` inside
    /// `struct LinkedList<T> { value: T, ... }` lower to
    /// `HirTyKind::Param(tpid)` rather than `Named("T")`. See
    /// spec/16_GENERIC.md §HIR (extension).
    fn seal_adts(&mut self, files: &IndexVec<FileId, LoadedFile>) {
        // Move out so iteration doesn't alias self.items.adts when we
        // push variants back.
        let field_decls = std::mem::take(&mut self.adt_field_decls);
        for (haid, decls) in field_decls.into_iter_enumerated() {
            let origin = self.items.adts[haid].span.file;
            let lf = &files[origin];
            let scope = self
                .scopes
                .get(&origin)
                .expect("file scope built in passes 1+2");
            let adt_name = self.items.adts[haid].name.clone();

            // Build per-ADT TyParamScope from the ADT's generic_params.
            // Empty for non-generic ADTs.
            let ty_param_pairs: Vec<(String, HTyParamId)> = self.items.adts[haid]
                .generic_params
                .iter()
                .map(|&tpid| {
                    let info = &self.items.ty_params[tpid];
                    (info.name.clone(), tpid)
                })
                .collect();
            let ty_params_scope = ty::TyParamScope(&ty_param_pairs);

            let mut fields: IndexVec<FieldIdx, HirField> = IndexVec::new();
            let mut seen: HashMap<String, Span> = HashMap::new();
            for fd in decls {
                let lowered_ty = ty::lower_ty(
                    &lf.ast,
                    scope,
                    ty_params_scope,
                    &mut self.errors.borrow_mut(),
                    fd.ty,
                );
                if let Some(first_span) = seen.get(&fd.name.name) {
                    self.emit_error(HirError::DuplicateField {
                        adt: adt_name.clone(),
                        name: fd.name.name.clone(),
                        first: first_span.clone(),
                        dup: fd.name.span.clone(),
                    });
                    continue;
                }
                seen.insert(fd.name.name.clone(), fd.name.span.clone());
                fields.push(HirField {
                    name: fd.name.name,
                    ty: lowered_ty,
                    span: fd.span,
                });
            }
            let span = self.items.adts[haid].span.clone();
            self.items.adts[haid].variants.push(HirVariant {
                name: None,
                fields,
                span,
            });
        }
    }

    /// Pass 4b: lower each const item's annotated type under its
    /// origin file's resolution scope. Deferred from prescan so a
    /// const can name an ADT declared later in source. Walks all
    /// consts in `ConstId` (= prescan = source) order. Const items
    /// are not generic in v0, so the type-param scope is empty.
    /// See spec/18_CONST.md.
    fn seal_consts(&mut self, files: &IndexVec<FileId, LoadedFile>) {
        let const_ty_decls = std::mem::take(&mut self.const_ty_decls);
        for (cid, ast_ty) in const_ty_decls.into_iter_enumerated() {
            let origin = self.items.consts[cid].span.file;
            let lf = &files[origin];
            let scope = self
                .scopes
                .get(&origin)
                .expect("file scope built in passes 1+2");
            let empty_ty_params = ty::TyParamScope(&[]);
            let lowered_ty = ty::lower_ty(
                &lf.ast,
                scope,
                empty_ty_params,
                &mut self.errors.borrow_mut(),
                ast_ty,
            );
            self.items.consts[cid].ty = lowered_ty;
        }
    }
}
