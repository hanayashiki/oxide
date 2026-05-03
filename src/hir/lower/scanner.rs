use index_vec::IndexVec;

use crate::{
    hir::{HirError, ir::*},
    loader::LoadedFile,
    reporter::{FileId, Span},
};

use super::ty;

use crate::parser::ast::{self, *};
use std::{
    cell::RefCell,
    collections::HashMap,
};

#[derive(Default)]
pub(super) struct ModuleScope {
    pub types: HashMap<String, HAdtId>,
    pub values: HashMap<String, FnId>,
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
    pub fn lookup_value(&self, name: &str) -> Option<FnId> {
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

pub(super) fn scan(
    files: &IndexVec<FileId, LoadedFile>,
) -> (ScanResult, Vec<HirError>) {
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

                    // Insert and check in one shot. `HashMap.insert`
                    // returns the displaced (older) entry — that's
                    // the *first* definition; the one we just pushed
                    // is the duplicate.
                    if let Some(displaced_fn_id) = self
                        .get_local_scope_mut(file.file)
                        .values
                        .insert(name.clone(), fn_id)
                    {
                        self.emit_error(HirError::DuplicateFn {
                            name: name.clone(),
                            first: self.get_fn(displaced_fn_id).span.clone(),
                            dup: self.get_fn(fn_id).span.clone(),
                        });
                    }

                    if scan_ctx.in_extern_c {
                        if fn_decl.body.is_some() {
                            self.emit_error(HirError::ExternFnHasBody {
                                name: name.clone(),
                                span: span.clone(),
                            });
                        }
                    } else if fn_decl.body.is_none() {
                        self.emit_error(HirError::BodylessFnOutsideExtern {
                            name: name.clone(),
                            span: span.clone(),
                        });
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
            };
        }
    }

    fn build_import_scopes(&mut self, file: &LoadedFile, files: &IndexVec<FileId, LoadedFile>) {
        for &import in &file.direct_imports {
            let imported_file = &files[import];

            // Values namespace.
            let imported_values = self
                .get_local_scope(imported_file.file)
                .into_iter()
                .flat_map(|s| s.values.iter())
                .map(|(name, &fn_id)| (name.clone(), fn_id))
                .collect::<Vec<_>>();
            for (imported_name, imported_fn_id) in imported_values {
                if let Some(old) = self
                    .get_import_scope_mut(file.file)
                    .values
                    .insert(imported_name.clone(), imported_fn_id)
                    .or_else(|| {
                        self.get_local_scope(file.file)
                            .and_then(|s| s.values.get(&imported_name).copied())
                    })
                {
                    self.emit_error(HirError::DuplicateFn {
                        name: imported_name,
                        first: self.get_fn(old).span.clone(),
                        dup: self.get_fn(imported_fn_id).span.clone(),
                    });
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
        let mut seen_fn_id = HashMap::<&String, FnId>::default();
        let mut seen_adt_id = HashMap::<&String, HAdtId>::default();

        for file in files {
            // Values namespace.
            for (name, &fn_id) in self
                .get_local_scope(file.file)
                .iter()
                .flat_map(|s| &s.values)
            {
                if let Some(&first) = seen_fn_id.get(name) {
                    self.emit_error(HirError::DuplicateGlobalSymbol {
                        name: name.clone(),
                        first: self.get_fn(first).span.clone(),
                        dup: self.get_fn(fn_id).span.clone(),
                        root: file.file,
                    });
                }
                seen_fn_id.insert(name, fn_id);
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
    /// resolution scope. Walks all ADTs in `HAdtId` (= prescan = source)
    /// order. `DuplicateField` recovery skips the duplicate but keeps
    /// subsequent fields.
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

            let mut fields: IndexVec<FieldIdx, HirField> = IndexVec::new();
            let mut seen: HashMap<String, Span> = HashMap::new();
            for fd in decls {
                let lowered_ty = ty::lower_ty(&lf.ast, scope, fd.ty);
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
}
