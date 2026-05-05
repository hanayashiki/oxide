// hir_lower.ox — AST → HIR transformation.
//
// Two-pass lowering:
//   Pass 1: collect signatures (fn names, adt names, generic params,
//           param/field types). Builds the global symbol tables.
//   Pass 2: lower bodies. Walks each fn body's expression tree,
//           resolving every ident in value position to Local/Fn,
//           every type position to Adt/Param/Named.
//
// Single-file mode: imports are accepted at parse time but produce
// `HE_UNRESOLVED_NAME` errors for any name they would have brought
// in. Multi-file lowering lands with M7 (driver).
//
// The `Lowerer` carries scope stacks for value and type namespaces,
// the `loop_depth` for break/continue validation, and the in-flight
// `program` accumulator.

import "stdlib.ox";
import "stdio.ox";
import "string.ox";
import "intrinsics.ox";
import "./util/vec.ox";
import "./util/strbuf.ox";
import "./ast.ox";
import "./hir.ox";

// ---------------------------------------------------------------- //
// Resolution targets                                               //
// ---------------------------------------------------------------- //

fn RES_LOCAL()    -> u8 { 0 }     // value namespace
fn RES_FN()       -> u8 { 1 }     // value namespace
fn RES_ADT()      -> u8 { 2 }     // type namespace
fn RES_TY_PARAM() -> u8 { 3 }     // type namespace

struct ScopeEntry {
    name_off: usize,
    name_len: usize,
    kind:     u8,                 // RES_*
    id:       usize,
}

// ---------------------------------------------------------------- //
// Lowerer state                                                    //
// ---------------------------------------------------------------- //

struct Lowerer {
    program:   HirProgram,
    value_scope: Vec<ScopeEntry>,
    type_scope:  Vec<ScopeEntry>,
    loop_depth:  u32,
    // In-flight fn / adt context for TyParam resolution and body
    // owner tagging.
    current_fn:  usize,           // FnId during pass 2; HID_NONE between fns
    current_adt: usize,           // HAdtId during adt field resolution
}

fn lower_program(ast: Module) -> HirProgram {
    let mut l: Lowerer = Lowerer {
        program: hir_program_new(ast.pool),
        value_scope: vec_new::<ScopeEntry>(),
        type_scope:  vec_new::<ScopeEntry>(),
        loop_depth:  0,
        current_fn:  HID_NONE(),
        current_adt: HID_NONE(),
    };

    // Pass 1: collect signatures
    pass1_collect_signatures(&mut l, &ast);

    // Pass 2: lower bodies
    pass2_lower_bodies(&mut l, &ast);

    l.program
}

// ---------------------------------------------------------------- //
// Pool slice equality (for scope lookups by source name)           //
// ---------------------------------------------------------------- //

fn name_eq(pool_ptr: *const [u8], a_off: usize, a_len: usize,
           b_off: usize, b_len: usize) -> bool {
    if a_len != b_len { return false; }
    let mut i: usize = 0;
    while i < a_len {
        if pool_ptr[a_off + i] != pool_ptr[b_off + i] {
            return false;
        }
        i = i + 1;
    }
    true
}

fn name_eq_lit(pool_ptr: *const [u8], a_off: usize, a_len: usize,
               s: *const [u8], n: usize) -> bool {
    if a_len != n { return false; }
    let mut i: usize = 0;
    while i < a_len {
        if pool_ptr[a_off + i] != s[i] {
            return false;
        }
        i = i + 1;
    }
    true
}

// ---------------------------------------------------------------- //
// Scope stack helpers                                              //
// ---------------------------------------------------------------- //

fn push_value(l: *mut Lowerer, off: usize, len: usize, kind: u8, id: usize) {
    vec_push::<ScopeEntry>(&mut l.value_scope, ScopeEntry {
        name_off: off, name_len: len, kind: kind, id: id,
    });
}

fn push_type(l: *mut Lowerer, off: usize, len: usize, kind: u8, id: usize) {
    vec_push::<ScopeEntry>(&mut l.type_scope, ScopeEntry {
        name_off: off, name_len: len, kind: kind, id: id,
    });
}

fn value_scope_height(l: *const Lowerer) -> usize {
    vec_len::<ScopeEntry>(&l.value_scope)
}

fn type_scope_height(l: *const Lowerer) -> usize {
    vec_len::<ScopeEntry>(&l.type_scope)
}

fn truncate_value_scope(l: *mut Lowerer, h: usize) {
    l.value_scope.len = h;
}

fn truncate_type_scope(l: *mut Lowerer, h: usize) {
    l.type_scope.len = h;
}

// Look up (off, len) in `value_scope`, walking back-to-front so
// inner definitions shadow outer ones. Returns (kind, id) on hit;
// kind = 255 on miss.
fn lookup_value(l: *const Lowerer, off: usize, len: usize, out_id: *mut usize) -> u8 {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);
    let n: usize = vec_len::<ScopeEntry>(&l.value_scope);
    if n == 0 { return 255; }
    let mut i: usize = n;
    while i > 0 {
        i = i - 1;
        let e: ScopeEntry = vec_get::<ScopeEntry>(&l.value_scope, i);
        if name_eq(pool_ptr, e.name_off, e.name_len, off, len) {
            *out_id = e.id;
            return e.kind;
        }
    }
    255
}

fn lookup_type(l: *const Lowerer, off: usize, len: usize, out_id: *mut usize) -> u8 {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);
    let n: usize = vec_len::<ScopeEntry>(&l.type_scope);
    if n == 0 { return 255; }
    let mut i: usize = n;
    while i > 0 {
        i = i - 1;
        let e: ScopeEntry = vec_get::<ScopeEntry>(&l.type_scope, i);
        if name_eq(pool_ptr, e.name_off, e.name_len, off, len) {
            *out_id = e.id;
            return e.kind;
        }
    }
    255
}

// ---------------------------------------------------------------- //
// Error helpers                                                    //
// ---------------------------------------------------------------- //

fn err_at(l: *mut Lowerer, kind: u8, span_start: usize, span_end: usize, n: HName) {
    let mut e: HirError = hir_error_zero();
    e.kind       = kind;
    e.span_start = span_start;
    e.span_end   = span_end;
    e.name1      = n;
    vec_push::<HirError>(&mut l.program.errors, e);
}

fn err_dup(l: *mut Lowerer, kind: u8, first: HName, dup: HName) {
    let mut e: HirError = hir_error_zero();
    e.kind       = kind;
    e.span_start = dup.span_start;
    e.span_end   = dup.span_end;
    e.name1      = first;
    e.name2      = dup;
    vec_push::<HirError>(&mut l.program.errors, e);
}

// ---------------------------------------------------------------- //
// Intrinsic recognition                                            //
// ---------------------------------------------------------------- //

fn intrinsic_for(pool_ptr: *const [u8], off: usize, len: usize) -> u8 {
    if name_eq_lit(pool_ptr, off, len, "ox_transmute", 12) {
        return INTR_TRANSMUTE();
    }
    if name_eq_lit(pool_ptr, off, len, "ox_size_of", 10) {
        return INTR_SIZE_OF();
    }
    INTR_NONE()
}

// ---------------------------------------------------------------- //
// Pass 1 — collect signatures                                      //
// ---------------------------------------------------------------- //

fn pass1_collect_signatures(l: *mut Lowerer, ast: *const Module) {
    // Phase A: walk top-level items, allocate FnId/HAdtId, register
    // names in scope. Resolving param/field/ret types happens in
    // Phase B once all ADT names are known.
    let n: usize = vec_len::<usize>(&ast.root_items);
    let mut i: usize = 0;
    while i < n {
        let item_id: usize = vec_get::<usize>(&ast.root_items, i);
        pass1_register_item(l, ast, item_id, false);
        i = i + 1;
    }

    // Phase B: resolve fn signatures + adt fields. Order: ADTs
    // first (some fn signatures may reference structs), then fns.
    let an: usize = vec_len::<HirAdt>(&l.program.adts);
    let mut j: usize = 0;
    while j < an {
        pass1_resolve_adt(l, ast, j);
        j = j + 1;
    }

    let fnn: usize = vec_len::<HirFn>(&l.program.fns);
    let mut k: usize = 0;
    while k < fnn {
        pass1_resolve_fn_sig(l, ast, k);
        k = k + 1;
    }
}

// `inside_extern`: when true, FnDecls register as is_extern; bodies
// must be absent. When false, we expect a body; absence triggers
// the BodylessFn diagnostic (unless the fn is an intrinsic).
fn pass1_register_item(l: *mut Lowerer, ast: *const Module, item_id: usize,
                       inside_extern: bool) {
    let it: Item = vec_get::<Item>(&ast.items, item_id);
    if it.kind == ITEM_FN() {
        register_fn(l, ast, it, inside_extern);
        return;
    }
    if it.kind == ITEM_STRUCT() {
        register_adt(l, ast, it);
        return;
    }
    if it.kind == ITEM_EXTERN_BLOCK() {
        let cn: usize = vec_len::<usize>(&it.extern_items);
        let mut i: usize = 0;
        while i < cn {
            let child_id: usize = vec_get::<usize>(&it.extern_items, i);
            pass1_register_item(l, ast, child_id, true);
            i = i + 1;
        }
        return;
    }
    if it.kind == ITEM_IMPORT() {
        // Single-file mode — imports are accepted but produce no
        // names. M7 will resolve and load.
        return;
    }
}

fn register_fn(l: *mut Lowerer, ast: *const Module, it: Item, is_extern: bool) {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);

    // Detect duplicate-global-symbol against the value namespace.
    let mut existing_id: usize = 0;
    let lk: u8 = lookup_value(l, it.name.name_off, it.name.name_len, &mut existing_id);
    if lk != 255 {
        // Find the span of the first definition for the diagnostic.
        let first_name: HName = first_value_name(l, lk, existing_id);
        err_dup(l, HE_DUPLICATE_GLOBAL(), first_name, hname_from_ident(it.name));
        // Keep the first; don't overwrite.
        return;
    }

    let mut hf: HirFn = hir_fn_zero();
    hf.name        = hname_from_ident(it.name);
    hf.is_extern   = is_extern;
    hf.is_variadic = it.is_variadic;
    hf.span_start  = it.span_start;
    hf.span_end    = it.span_end;
    hf.intrinsic   = intrinsic_for(pool_ptr, it.name.name_off, it.name.name_len);

    // Validate body shape vs context.
    if is_extern && it.body != ID_NONE() {
        err_at(l, HE_EXTERN_FN_HAS_BODY(), it.span_start, it.span_end,
               hname_from_ident(it.name));
    }
    if !is_extern && it.body == ID_NONE() && hf.intrinsic == INTR_NONE() {
        err_at(l, HE_BODYLESS_FN_OUTSIDE_EXT(), it.span_start, it.span_end,
               hname_from_ident(it.name));
    }
    if is_extern && vec_len::<Ident>(&it.generic_params) > 0 {
        err_at(l, HE_GENERIC_EXTERN_FN(), it.span_start, it.span_end,
               hname_from_ident(it.name));
    }

    let fn_id: usize = vec_len::<HirFn>(&l.program.fns);
    vec_push::<HirFn>(&mut l.program.fns, hf);

    push_value(l, it.name.name_off, it.name.name_len, RES_FN(), fn_id);
    vec_push::<usize>(&mut l.program.root_fns, fn_id);
}

fn register_adt(l: *mut Lowerer, ast: *const Module, it: Item) {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);

    let mut existing_id: usize = 0;
    let lk: u8 = lookup_type(l, it.name.name_off, it.name.name_len, &mut existing_id);
    if lk != 255 {
        let first_name: HName = first_type_name(l, lk, existing_id);
        err_dup(l, HE_DUPLICATE_GLOBAL(), first_name, hname_from_ident(it.name));
        return;
    }

    let mut ha: HirAdt = hir_adt_zero();
    ha.name       = hname_from_ident(it.name);
    ha.span_start = it.span_start;
    ha.span_end   = it.span_end;

    let adt_id: usize = vec_len::<HirAdt>(&l.program.adts);
    vec_push::<HirAdt>(&mut l.program.adts, ha);

    push_type(l, it.name.name_off, it.name.name_len, RES_ADT(), adt_id);
    vec_push::<usize>(&mut l.program.root_adts, adt_id);
}

// Given a hit from lookup_value, retrieve the original definition's
// span — used to render duplicate-symbol diagnostics with two spans.
fn first_value_name(l: *const Lowerer, kind: u8, id: usize) -> HName {
    if kind == RES_FN() {
        let f: HirFn = vec_get::<HirFn>(&l.program.fns, id);
        return f.name;
    }
    if kind == RES_LOCAL() {
        let local: HirLocal = vec_get::<HirLocal>(&l.program.locals, id);
        return local.name;
    }
    hname_zero()
}

fn first_type_name(l: *const Lowerer, kind: u8, id: usize) -> HName {
    if kind == RES_ADT() {
        let a: HirAdt = vec_get::<HirAdt>(&l.program.adts, id);
        return a.name;
    }
    if kind == RES_TY_PARAM() {
        let tp: TyParamInfo = vec_get::<TyParamInfo>(&l.program.ty_params, id);
        return tp.name;
    }
    hname_zero()
}

// ---------------------------------------------------------------- //
// Phase B helpers                                                  //
// ---------------------------------------------------------------- //

// For an ADT (struct), allocate ty_params and resolve field types
// in the type-scope augmented with the struct's own generics.
fn pass1_resolve_adt(l: *mut Lowerer, ast: *const Module, adt_id: usize) {
    // Find the AST item that corresponds to this adt_id.
    let ast_item_id: usize = find_ast_item_for_adt(l, ast, adt_id);
    if ast_item_id == ID_NONE() { return; }
    let it: Item = vec_get::<Item>(&ast.items, ast_item_id);

    l.current_adt = adt_id;
    let saved: usize = type_scope_height(l);

    // Allocate TyParamInfo for each generic param and push into
    // type_scope.
    let gpn: usize = vec_len::<Ident>(&it.generic_params);
    let mut i: usize = 0;
    while i < gpn {
        let g: Ident = vec_get::<Ident>(&it.generic_params, i);
        // Duplicate-name check within this struct's params.
        let mut existing: usize = 0;
        let lk: u8 = lookup_type(l, g.name_off, g.name_len, &mut existing);
        let dup_in_struct: bool = lk == RES_TY_PARAM() && existing >= saved_param_threshold(l, saved);
        if dup_in_struct {
            let first: HName = first_type_name(l, lk, existing);
            err_dup(l, HE_DUPLICATE_TY_PARAM(), first, hname_from_ident(g));
        }
        let mut tp: TyParamInfo = ty_param_zero();
        tp.owner_kind   = TPO_ADT();
        tp.owner_id     = adt_id;
        tp.idx_in_owner = i as u32;
        tp.name         = hname_from_ident(g);
        tp.span_start   = g.span_start;
        tp.span_end     = g.span_end;
        let tpid: usize = vec_len::<TyParamInfo>(&l.program.ty_params);
        vec_push::<TyParamInfo>(&mut l.program.ty_params, tp);
        // Update HirAdt.generic_params
        let mut a: HirAdt = vec_get::<HirAdt>(&l.program.adts, adt_id);
        vec_push::<usize>(&mut a.generic_params, tpid);
        vec_set::<HirAdt>(&mut l.program.adts, adt_id, a);
        push_type(l, g.name_off, g.name_len, RES_TY_PARAM(), tpid);
        i = i + 1;
    }

    // Resolve fields.
    let fcount: usize = vec_len::<FieldDecl>(&it.fields);
    let mut j: usize = 0;
    while j < fcount {
        let fd: FieldDecl = vec_get::<FieldDecl>(&it.fields, j);
        // Duplicate-field check within this struct.
        if has_field_with_name(l, adt_id, fd.name) {
            let mut a: HirAdt = vec_get::<HirAdt>(&l.program.adts, adt_id);
            let exist_count: usize = vec_len::<HirField>(&a.fields);
            let mut prev_name: HName = hname_zero();
            let mut k: usize = 0;
            while k < exist_count {
                let f0: HirField = vec_get::<HirField>(&a.fields, k);
                if name_eq(strbuf_as_ptr(&l.program.pool),
                           f0.name.off, f0.name.len,
                           fd.name.name_off, fd.name.name_len) {
                    prev_name = f0.name;
                    break;
                }
                k = k + 1;
            }
            err_dup(l, HE_DUPLICATE_FIELD(), prev_name, hname_from_ident(fd.name));
        }
        let ty_id: usize = lower_type(l, ast, fd.ty);
        let hf: HirField = HirField {
            name:       hname_from_ident(fd.name),
            ty:         ty_id,
            span_start: fd.span_start,
            span_end:   fd.span_end,
        };
        let mut a: HirAdt = vec_get::<HirAdt>(&l.program.adts, adt_id);
        vec_push::<HirField>(&mut a.fields, hf);
        vec_set::<HirAdt>(&mut l.program.adts, adt_id, a);
        j = j + 1;
    }

    truncate_type_scope(l, saved);
    l.current_adt = HID_NONE();
}

fn has_field_with_name(l: *const Lowerer, adt_id: usize, n: Ident) -> bool {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);
    let a: HirAdt = vec_get::<HirAdt>(&l.program.adts, adt_id);
    let count: usize = vec_len::<HirField>(&a.fields);
    let mut i: usize = 0;
    while i < count {
        let f: HirField = vec_get::<HirField>(&a.fields, i);
        if name_eq(pool_ptr, f.name.off, f.name.len, n.name_off, n.name_len) {
            return true;
        }
        i = i + 1;
    }
    false
}

fn saved_param_threshold(l: *const Lowerer, saved: usize) -> usize {
    saved
}

// Find the AST item id for a given adt_id by name. Linear scan is
// fine — n is small.
fn find_ast_item_for_adt(l: *const Lowerer, ast: *const Module, adt_id: usize) -> usize {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);
    let target: HirAdt = vec_get::<HirAdt>(&l.program.adts, adt_id);
    let n: usize = vec_len::<usize>(&ast.root_items);
    let mut i: usize = 0;
    while i < n {
        let item_id: usize = vec_get::<usize>(&ast.root_items, i);
        let it: Item = vec_get::<Item>(&ast.items, item_id);
        if it.kind == ITEM_STRUCT()
            && name_eq(pool_ptr,
                       it.name.name_off, it.name.name_len,
                       target.name.off, target.name.len)
        {
            return item_id;
        }
        i = i + 1;
    }
    ID_NONE()
}

fn find_ast_item_for_fn(l: *const Lowerer, ast: *const Module, fn_id: usize) -> usize {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);
    let target: HirFn = vec_get::<HirFn>(&l.program.fns, fn_id);
    let n: usize = vec_len::<usize>(&ast.root_items);
    let mut i: usize = 0;
    while i < n {
        let item_id: usize = vec_get::<usize>(&ast.root_items, i);
        let r: usize = match_fn_in_item(l, ast, item_id, target.name);
        if r != ID_NONE() { return r; }
        i = i + 1;
    }
    ID_NONE()
}

fn match_fn_in_item(l: *const Lowerer, ast: *const Module, item_id: usize,
                    target: HName) -> usize {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&l.program.pool);
    let it: Item = vec_get::<Item>(&ast.items, item_id);
    if it.kind == ITEM_FN()
        && name_eq(pool_ptr,
                   it.name.name_off, it.name.name_len,
                   target.off, target.len)
    {
        return item_id;
    }
    if it.kind == ITEM_EXTERN_BLOCK() {
        let cn: usize = vec_len::<usize>(&it.extern_items);
        let mut i: usize = 0;
        while i < cn {
            let child: usize = vec_get::<usize>(&it.extern_items, i);
            let r: usize = match_fn_in_item(l, ast, child, target);
            if r != ID_NONE() { return r; }
            i = i + 1;
        }
    }
    ID_NONE()
}

// Resolve fn signature: allocate ty_params and locals for params,
// resolve param types and ret type. Body lowering happens in pass 2.
fn pass1_resolve_fn_sig(l: *mut Lowerer, ast: *const Module, fn_id: usize) {
    let ast_item_id: usize = find_ast_item_for_fn(l, ast, fn_id);
    if ast_item_id == ID_NONE() { return; }
    let it: Item = vec_get::<Item>(&ast.items, ast_item_id);

    l.current_fn = fn_id;
    let saved: usize = type_scope_height(l);

    // Push generic params.
    let gpn: usize = vec_len::<Ident>(&it.generic_params);
    let mut i: usize = 0;
    while i < gpn {
        let g: Ident = vec_get::<Ident>(&it.generic_params, i);
        let mut existing: usize = 0;
        let lk: u8 = lookup_type(l, g.name_off, g.name_len, &mut existing);
        if lk == RES_TY_PARAM() && existing >= saved {
            let first: HName = first_type_name(l, lk, existing);
            err_dup(l, HE_DUPLICATE_TY_PARAM(), first, hname_from_ident(g));
        }
        let mut tp: TyParamInfo = ty_param_zero();
        tp.owner_kind   = TPO_FN();
        tp.owner_id     = fn_id;
        tp.idx_in_owner = i as u32;
        tp.name         = hname_from_ident(g);
        tp.span_start   = g.span_start;
        tp.span_end     = g.span_end;
        let tpid: usize = vec_len::<TyParamInfo>(&l.program.ty_params);
        vec_push::<TyParamInfo>(&mut l.program.ty_params, tp);
        let mut f: HirFn = vec_get::<HirFn>(&l.program.fns, fn_id);
        vec_push::<usize>(&mut f.generic_params, tpid);
        vec_set::<HirFn>(&mut l.program.fns, fn_id, f);
        push_type(l, g.name_off, g.name_len, RES_TY_PARAM(), tpid);
        i = i + 1;
    }

    // Allocate locals for each fn parameter and resolve its type.
    let pn: usize = vec_len::<Param>(&it.params);
    let mut j: usize = 0;
    while j < pn {
        let pr: Param = vec_get::<Param>(&it.params, j);
        let ty_id: usize = lower_type(l, ast, pr.ty);
        let mut local: HirLocal = hir_local_zero();
        local.name       = hname_from_ident(pr.name);
        local.mutable    = pr.mutable;
        local.ty         = ty_id;
        local.span_start = pr.span_start;
        local.span_end   = pr.span_end;
        let local_id: usize = vec_len::<HirLocal>(&l.program.locals);
        vec_push::<HirLocal>(&mut l.program.locals, local);
        let mut f: HirFn = vec_get::<HirFn>(&l.program.fns, fn_id);
        vec_push::<usize>(&mut f.params, local_id);
        vec_set::<HirFn>(&mut l.program.fns, fn_id, f);
        j = j + 1;
    }

    // Resolve return type if annotated.
    if it.ret_ty != ID_NONE() {
        let rt: usize = lower_type(l, ast, it.ret_ty);
        let mut f: HirFn = vec_get::<HirFn>(&l.program.fns, fn_id);
        f.ret_ty = rt;
        vec_set::<HirFn>(&mut l.program.fns, fn_id, f);
    }

    truncate_type_scope(l, saved);
    l.current_fn = HID_NONE();
}

// ---------------------------------------------------------------- //
// Type lowering                                                    //
// ---------------------------------------------------------------- //

fn lower_type(l: *mut Lowerer, ast: *const Module, ast_ty_id: usize) -> usize {
    let t: Type = vec_get::<Type>(&ast.types, ast_ty_id);
    let mut h: HirTy = hir_ty_zero();
    h.span_start = t.span_start;
    h.span_end   = t.span_end;

    if t.kind == TY_NAMED() {
        // Try the type-scope; falls back to Named for primitives /
        // unresolved.
        let mut id: usize = 0;
        let lk: u8 = lookup_type(l, t.name.name_off, t.name.name_len, &mut id);
        if lk == RES_ADT() {
            h.kind = HTY_ADT();
            h.adt  = id;
            // Lower type args (recursive).
            let tan: usize = vec_len::<usize>(&t.type_args);
            let mut i: usize = 0;
            while i < tan {
                let arg: usize = vec_get::<usize>(&t.type_args, i);
                let arg_id: usize = lower_type(l, ast, arg);
                vec_push::<usize>(&mut h.type_args, arg_id);
                i = i + 1;
            }
        } else if lk == RES_TY_PARAM() {
            h.kind     = HTY_PARAM();
            h.ty_param = id;
            // Type args on a Param are nonsensical; v0 ignores them
            // (the grammar admits them; typeck would surface this if
            // it mattered). Lower them anyway for fidelity.
            let tan: usize = vec_len::<usize>(&t.type_args);
            let mut i: usize = 0;
            while i < tan {
                let arg: usize = vec_get::<usize>(&t.type_args, i);
                let arg_id: usize = lower_type(l, ast, arg);
                vec_push::<usize>(&mut h.type_args, arg_id);
                i = i + 1;
            }
        } else {
            // Primitive or unresolved — keep the name; typeck finishes
            // the lookup.
            h.kind = HTY_NAMED();
            h.name = hname_from_ident(t.name);
            let tan: usize = vec_len::<usize>(&t.type_args);
            let mut i: usize = 0;
            while i < tan {
                let arg: usize = vec_get::<usize>(&t.type_args, i);
                let arg_id: usize = lower_type(l, ast, arg);
                vec_push::<usize>(&mut h.type_args, arg_id);
                i = i + 1;
            }
        }
    } else if t.kind == TY_PTR() {
        h.kind       = HTY_PTR();
        h.mutability = t.mutability;
        h.pointee    = lower_type(l, ast, t.pointee);
    } else if t.kind == TY_ARRAY() {
        h.kind = HTY_ARRAY();
        h.elem = lower_type(l, ast, t.elem);
        if t.len_expr == ID_NONE() {
            h.len_is_some = false;
        } else {
            let le: Expr = vec_get::<Expr>(&ast.exprs, t.len_expr);
            h.len_is_some = true;
            h.len_val     = le.int_val;
        }
    } else {
        h.kind = HTY_ERROR();
    }

    let id: usize = vec_len::<HirTy>(&l.program.types);
    vec_push::<HirTy>(&mut l.program.types, h);
    id
}

// ---------------------------------------------------------------- //
// Pass 2 — lower bodies                                            //
// ---------------------------------------------------------------- //

fn pass2_lower_bodies(l: *mut Lowerer, ast: *const Module) {
    let n: usize = vec_len::<HirFn>(&l.program.fns);
    let mut i: usize = 0;
    while i < n {
        let f: HirFn = vec_get::<HirFn>(&l.program.fns, i);
        if f.is_extern || f.intrinsic != INTR_NONE() {
            i = i + 1;
            continue;
        }
        // Look up the AST item; if it has no body, BodylessFnOutsideExtern
        // already fired in pass 1 and we skip cleanly.
        let ast_item_id: usize = find_ast_item_for_fn(l, ast, i);
        if ast_item_id == ID_NONE() {
            i = i + 1;
            continue;
        }
        let it: Item = vec_get::<Item>(&ast.items, ast_item_id);
        if it.body == ID_NONE() {
            i = i + 1;
            continue;
        }
        lower_one_fn_body(l, ast, i);
        i = i + 1;
    }
}

fn lower_one_fn_body(l: *mut Lowerer, ast: *const Module, fn_id: usize) {
    let ast_item_id: usize = find_ast_item_for_fn(l, ast, fn_id);
    if ast_item_id == ID_NONE() { return; }
    let it: Item = vec_get::<Item>(&ast.items, ast_item_id);
    if it.body == ID_NONE() { return; }

    l.current_fn = fn_id;
    let val_saved: usize = value_scope_height(l);
    let ty_saved:  usize = type_scope_height(l);

    // Re-push the fn's generic params so type lookups in the body
    // see them.
    let f0: HirFn = vec_get::<HirFn>(&l.program.fns, fn_id);
    let gpn: usize = vec_len::<usize>(&f0.generic_params);
    let mut i: usize = 0;
    while i < gpn {
        let tpid: usize = vec_get::<usize>(&f0.generic_params, i);
        let tp: TyParamInfo = vec_get::<TyParamInfo>(&l.program.ty_params, tpid);
        push_type(l, tp.name.off, tp.name.len, RES_TY_PARAM(), tpid);
        i = i + 1;
    }

    // Push parameter locals into the value scope.
    let pn: usize = vec_len::<usize>(&f0.params);
    let mut j: usize = 0;
    while j < pn {
        let lid: usize = vec_get::<usize>(&f0.params, j);
        let local: HirLocal = vec_get::<HirLocal>(&l.program.locals, lid);
        push_value(l, local.name.off, local.name.len, RES_LOCAL(), lid);
        j = j + 1;
    }

    let body_block_id: usize = lower_block(l, ast, it.body);
    let mut f: HirFn = vec_get::<HirFn>(&l.program.fns, fn_id);
    f.body = body_block_id;
    vec_set::<HirFn>(&mut l.program.fns, fn_id, f);

    truncate_value_scope(l, val_saved);
    truncate_type_scope(l, ty_saved);
    l.current_fn = HID_NONE();
}

fn lower_block(l: *mut Lowerer, ast: *const Module, ast_block_id: usize) -> usize {
    let b: Block = vec_get::<Block>(&ast.blocks, ast_block_id);
    let val_saved: usize = value_scope_height(l);

    let mut hb: HirBlock = hir_block_zero();
    hb.span_start = b.span_start;
    hb.span_end   = b.span_end;

    let n: usize = vec_len::<BlockItem>(&b.items);
    let mut i: usize = 0;
    while i < n {
        let bi: BlockItem = vec_get::<BlockItem>(&b.items, i);
        let e_id: usize = lower_expr(l, ast, bi.expr);
        vec_push::<HirBlockItem>(&mut hb.items, HirBlockItem {
            expr: e_id, has_semi: bi.has_semi,
        });
        i = i + 1;
    }

    truncate_value_scope(l, val_saved);

    let id: usize = vec_len::<HirBlock>(&l.program.blocks);
    vec_push::<HirBlock>(&mut l.program.blocks, hb);
    id
}

// ---------------------------------------------------------------- //
// Expression lowering                                              //
// ---------------------------------------------------------------- //

fn push_hir_expr(l: *mut Lowerer, e: HirExpr) -> usize {
    let id: usize = vec_len::<HirExpr>(&l.program.exprs);
    vec_push::<HirExpr>(&mut l.program.exprs, e);
    id
}

fn lower_expr(l: *mut Lowerer, ast: *const Module, ast_expr_id: usize) -> usize {
    let ae: Expr = vec_get::<Expr>(&ast.exprs, ast_expr_id);
    let k: u8 = ae.kind;

    if k == EX_INT_LIT() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_INT_LIT();
        e.int_val    = ae.int_val;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_BOOL_LIT() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_BOOL_LIT();
        e.bool_val   = ae.bool_val;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_CHAR_LIT() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_CHAR_LIT();
        e.char_val   = ae.char_val as u8;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_STR_LIT() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind            = HEK_STR_LIT();
        e.name.off        = ae.name.name_off;
        e.name.len        = ae.name.name_len;
        e.span_start      = ae.span_start;
        e.span_end        = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_NULL() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_NULL();
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_PAREN() {
        // Unwrap — Paren has no semantic content.
        return lower_expr(l, ast, ae.e1);
    }
    if k == EX_IDENT() {
        return lower_ident(l, ast, ae);
    }
    if k == EX_UNARY() {
        let inner: usize = lower_expr(l, ast, ae.e1);
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_UNARY();
        e.op         = ae.op;
        e.e1         = inner;
        e.is_place   = ae.op == UN_DEREF();
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_BINARY() {
        let lhs: usize = lower_expr(l, ast, ae.e1);
        let rhs: usize = lower_expr(l, ast, ae.e2);
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_BINARY();
        e.op         = ae.op;
        e.e1         = lhs;
        e.e2         = rhs;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_ASSIGN() {
        let target: usize = lower_expr(l, ast, ae.e1);
        let rhs:    usize = lower_expr(l, ast, ae.e2);
        // Validate target is a place.
        let tgt: HirExpr = vec_get::<HirExpr>(&l.program.exprs, target);
        if !tgt.is_place {
            err_at(l, HE_NON_PLACE_ASSIGN(), tgt.span_start, tgt.span_end, hname_zero());
        }
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_ASSIGN();
        e.op         = ae.op;
        e.e1         = target;
        e.e2         = rhs;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_CALL() {
        return lower_call(l, ast, ae);
    }
    if k == EX_INDEX() {
        let base: usize = lower_expr(l, ast, ae.e1);
        let idx:  usize = lower_expr(l, ast, ae.e2);
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_INDEX();
        e.e1         = base;
        e.e2         = idx;
        e.is_place   = true;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_FIELD() {
        let base: usize = lower_expr(l, ast, ae.e1);
        let base_e: HirExpr = vec_get::<HirExpr>(&l.program.exprs, base);
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_FIELD();
        e.e1         = base;
        e.name       = hname_from_ident(ae.name);
        // Field's place-ness inherits from base (per spec/08).
        e.is_place   = base_e.is_place;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_STRUCT_LIT() {
        return lower_struct_lit(l, ast, ae);
    }
    if k == EX_ARRAY_LIT() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind = HEK_ARRAY_LIT();
        let an: usize = vec_len::<usize>(&ae.args);
        let mut i: usize = 0;
        while i < an {
            let a: usize = vec_get::<usize>(&ae.args, i);
            let lid: usize = lower_expr(l, ast, a);
            vec_push::<usize>(&mut e.args, lid);
            i = i + 1;
        }
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_ARRAY_RPT() {
        let init: usize = lower_expr(l, ast, ae.e1);
        // The length expression is a parsed IntLit; we extract its value.
        let len_e: Expr = vec_get::<Expr>(&ast.exprs, ae.e2);
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_ARRAY_RPT();
        e.e1         = init;
        e.int_val    = len_e.int_val;     // stash the length
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_ADDR_OF() {
        let inner: usize = lower_expr(l, ast, ae.e1);
        let inner_e: HirExpr = vec_get::<HirExpr>(&l.program.exprs, inner);
        if !inner_e.is_place {
            err_at(l, HE_NON_PLACE_ADDR_OF(), inner_e.span_start, inner_e.span_end,
                   hname_zero());
        }
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_ADDR_OF();
        e.e1         = inner;
        e.mutable    = ae.mutable;
        e.op         = ae.op;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_CAST() {
        let inner: usize = lower_expr(l, ast, ae.e1);
        let ty: usize = lower_type(l, ast, ae.t1);
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_CAST();
        e.e1         = inner;
        e.t1         = ty;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_IF() {
        let cond:  usize = lower_expr(l, ast, ae.e1);
        let then_id: usize = lower_block(l, ast, ae.e3);
        let mut else_id: usize = HID_NONE();
        let mut else_is_block: bool = false;
        if ae.e4 != ID_NONE() {
            if ae.e4_is_block {
                else_id = lower_block(l, ast, ae.e4);
                else_is_block = true;
            } else {
                else_id = lower_expr(l, ast, ae.e4);
                else_is_block = false;
            }
        }
        let mut e: HirExpr = hir_expr_zero();
        e.kind        = HEK_IF();
        e.e1          = cond;
        e.e3          = then_id;
        e.e4          = else_id;
        e.e4_is_block = else_is_block;
        e.span_start  = ae.span_start;
        e.span_end    = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_BLOCK() {
        let bid: usize = lower_block(l, ast, ae.e3);
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_BLOCK();
        e.e3         = bid;
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_RETURN() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_RETURN();
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        if ae.e1 != ID_NONE() {
            e.e1 = lower_expr(l, ast, ae.e1);
        }
        return push_hir_expr(l, e);
    }
    if k == EX_WHILE() {
        return lower_loop(l, ast, ae, LS_WHILE());
    }
    if k == EX_LOOP() {
        return lower_loop(l, ast, ae, LS_LOOP());
    }
    if k == EX_FOR() {
        return lower_loop(l, ast, ae, LS_FOR());
    }
    if k == EX_BREAK() {
        if l.loop_depth == 0 {
            err_at(l, HE_BREAK_OUTSIDE_LOOP(), ae.span_start, ae.span_end, hname_zero());
        }
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_BREAK();
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        if ae.e1 != ID_NONE() {
            e.e1 = lower_expr(l, ast, ae.e1);
        }
        return push_hir_expr(l, e);
    }
    if k == EX_CONTINUE() {
        if l.loop_depth == 0 {
            err_at(l, HE_CONTINUE_OUTSIDE_LOOP(), ae.span_start, ae.span_end, hname_zero());
        }
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_CONTINUE();
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }
    if k == EX_LET() {
        return lower_let(l, ast, ae);
    }
    if k == EX_POISON() {
        let mut e: HirExpr = hir_expr_zero();
        e.kind       = HEK_POISON();
        e.is_place   = true;          // suppress cascading errors
        e.span_start = ae.span_start;
        e.span_end   = ae.span_end;
        return push_hir_expr(l, e);
    }

    // Should not reach: cover remaining kinds with poison.
    let mut e: HirExpr = hir_expr_zero();
    e.kind       = HEK_POISON();
    e.span_start = ae.span_start;
    e.span_end   = ae.span_end;
    return push_hir_expr(l, e);
}

fn lower_ident(l: *mut Lowerer, ast: *const Module, ae: Expr) -> usize {
    let mut id: usize = 0;
    let lk: u8 = lookup_value(l, ae.name.name_off, ae.name.name_len, &mut id);
    let mut e: HirExpr = hir_expr_zero();
    e.span_start = ae.span_start;
    e.span_end   = ae.span_end;

    // Lift turbofish args from the ident into the HirExpr, even
    // though we don't use them on Local/Fn directly — typeck will
    // see them via the surrounding Call.
    let tan: usize = vec_len::<usize>(&ae.type_args);
    let mut i: usize = 0;
    while i < tan {
        let arg: usize = vec_get::<usize>(&ae.type_args, i);
        let arg_id: usize = lower_type(l, ast, arg);
        vec_push::<usize>(&mut e.type_args, arg_id);
        i = i + 1;
    }

    if lk == RES_LOCAL() {
        e.kind     = HEK_LOCAL();
        e.local_id = id;
        e.is_place = true;
    } else if lk == RES_FN() {
        e.kind  = HEK_FN();
        e.fn_id = id;
    } else {
        e.kind = HEK_UNRESOLVED();
        e.name = hname_from_ident(ae.name);
        e.is_place = true;        // suppress cascading errors
        err_at(l, HE_UNRESOLVED_NAME(), ae.span_start, ae.span_end,
               hname_from_ident(ae.name));
    }
    push_hir_expr(l, e)
}

fn lower_call(l: *mut Lowerer, ast: *const Module, ae: Expr) -> usize {
    let callee: usize = lower_expr(l, ast, ae.e1);
    let mut e: HirExpr = hir_expr_zero();
    e.kind = HEK_CALL();
    e.e1   = callee;

    // Lower turbofish args.
    let tan: usize = vec_len::<usize>(&ae.type_args);
    let mut i: usize = 0;
    while i < tan {
        let arg: usize = vec_get::<usize>(&ae.type_args, i);
        let arg_id: usize = lower_type(l, ast, arg);
        vec_push::<usize>(&mut e.type_args, arg_id);
        i = i + 1;
    }

    // Lower each arg.
    let an: usize = vec_len::<usize>(&ae.args);
    let mut j: usize = 0;
    while j < an {
        let a: usize = vec_get::<usize>(&ae.args, j);
        let lid: usize = lower_expr(l, ast, a);
        vec_push::<usize>(&mut e.args, lid);
        j = j + 1;
    }

    e.span_start = ae.span_start;
    e.span_end   = ae.span_end;
    push_hir_expr(l, e)
}

fn lower_struct_lit(l: *mut Lowerer, ast: *const Module, ae: Expr) -> usize {
    let mut id: usize = 0;
    let lk: u8 = lookup_type(l, ae.name.name_off, ae.name.name_len, &mut id);
    let mut e: HirExpr = hir_expr_zero();
    e.kind = HEK_STRUCT_LIT();
    if lk == RES_ADT() {
        e.adt = id;
    } else {
        // Unresolved type name in struct-lit position — file a
        // diagnostic. We still lower the value sub-expressions so
        // their diagnostics fire.
        err_at(l, HE_UNRESOLVED_TYPE_NAME(), ae.span_start, ae.span_end,
               hname_from_ident(ae.name));
        e.adt = HID_NONE();
    }

    // Turbofish args.
    let tan: usize = vec_len::<usize>(&ae.type_args);
    let mut i: usize = 0;
    while i < tan {
        let arg: usize = vec_get::<usize>(&ae.type_args, i);
        let arg_id: usize = lower_type(l, ast, arg);
        vec_push::<usize>(&mut e.type_args, arg_id);
        i = i + 1;
    }

    // Fields.
    let fn_count: usize = vec_len::<StructLitField>(&ae.sl_fields);
    let mut j: usize = 0;
    while j < fn_count {
        let sf: StructLitField = vec_get::<StructLitField>(&ae.sl_fields, j);
        let val: usize = lower_expr(l, ast, sf.value);
        vec_push::<HirStructLitField>(&mut e.sl_fields, HirStructLitField {
            name:       hname_from_ident(sf.name),
            value:      val,
            span_start: sf.span_start,
            span_end:   sf.span_end,
        });
        j = j + 1;
    }

    e.span_start = ae.span_start;
    e.span_end   = ae.span_end;
    push_hir_expr(l, e)
}

fn lower_loop(l: *mut Lowerer, ast: *const Module, ae: Expr, source: u8) -> usize {
    // For-loop init may be a `let`; its scope must include the body
    // and the cond/update slots. We don't have a separate "for-loop
    // scope" — treat the for-loop expression as introducing a
    // value-scope frame.
    let val_saved: usize = value_scope_height(l);
    l.loop_depth = l.loop_depth + 1;

    let init_id: usize = if ae.e1 != ID_NONE() {
        lower_expr(l, ast, ae.e1)
    } else {
        HID_NONE()
    };
    let cond_id: usize = if ae.e1 != ID_NONE() && source == LS_WHILE() {
        // While stores cond in e1. (See lower_while-ish dispatch.)
        // But our AST stores while.cond in e1 too — wait, see notes
        // below.
        HID_NONE()
    } else {
        HID_NONE()
    };
    // The above is a placeholder; we re-do dispatch per source.
    let _ = init_id;
    let _ = cond_id;

    let result: usize = lower_loop_dispatch(l, ast, ae, source);

    l.loop_depth = l.loop_depth - 1;
    truncate_value_scope(l, val_saved);
    result
}

// Per-source dispatch. We inline the body rather than thread `_id`
// vars because the AST encoding of init/cond/update differs by
// source (for has 4 slots; while has 2; loop has 1).
fn lower_loop_dispatch(l: *mut Lowerer, ast: *const Module, ae: Expr, source: u8) -> usize {
    let mut e: HirExpr = hir_expr_zero();
    e.kind        = HEK_LOOP();
    e.loop_source = source;
    e.span_start  = ae.span_start;
    e.span_end    = ae.span_end;

    if source == LS_WHILE() {
        e.e1 = lower_expr(l, ast, ae.e1);          // cond
        e.e3 = lower_block(l, ast, ae.e3);         // body
    } else if source == LS_LOOP() {
        e.e3 = lower_block(l, ast, ae.e3);         // body
    } else if source == LS_FOR() {
        // For has init in e1, cond in e2, update in t1, body in e3.
        if ae.e1 != ID_NONE() {
            e.e2 = lower_expr(l, ast, ae.e1);      // init in e2
        }
        if ae.e2 != ID_NONE() {
            e.e1 = lower_expr(l, ast, ae.e2);      // cond in e1
        }
        if ae.t1 != ID_NONE() {
            e.t1 = lower_expr(l, ast, ae.t1);      // update in t1
        }
        e.e3 = lower_block(l, ast, ae.e3);         // body in e3
    }
    push_hir_expr(l, e)
}

fn lower_let(l: *mut Lowerer, ast: *const Module, ae: Expr) -> usize {
    let mut local: HirLocal = hir_local_zero();
    local.name       = hname_from_ident(ae.name);
    local.mutable    = ae.mutable;
    local.span_start = ae.span_start;
    local.span_end   = ae.span_end;
    if ae.t1 != ID_NONE() {
        local.ty = lower_type(l, ast, ae.t1);
    }
    let local_id: usize = vec_len::<HirLocal>(&l.program.locals);
    vec_push::<HirLocal>(&mut l.program.locals, local);

    let init_id: usize = if ae.e1 != ID_NONE() {
        lower_expr(l, ast, ae.e1)
    } else {
        HID_NONE()
    };

    // Push into the value scope *after* lowering init — `let x = x;`
    // must reach the outer `x`, not the binding being introduced.
    push_value(l, ae.name.name_off, ae.name.name_len, RES_LOCAL(), local_id);

    let mut e: HirExpr = hir_expr_zero();
    e.kind        = HEK_LET();
    e.local_id    = local_id;
    e.e1          = init_id;
    e.mutable     = ae.mutable;
    e.span_start  = ae.span_start;
    e.span_end    = ae.span_end;
    push_hir_expr(l, e)
}
