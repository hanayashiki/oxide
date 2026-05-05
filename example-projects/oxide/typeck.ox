// typeck.ox — minimum-viable type checker for stage-1.
//
// Strategy: top-down expected-type propagation. Each `infer_expr`
// takes an optional "expected" TyId (or HID_NONE meaning "no
// constraint") and returns the assigned TyId. Literals look at the
// expected slot to pick a concrete int type; unannotated calls/let-
// bindings still resolve when the inputs are concrete.
//
// Stage-1 source is heavily annotated (every `let` carries a type,
// almost every generic call uses turbofish), so this propagation is
// usually enough. Where it's not, integer literals default to i32.
//
// The pass writes:
//   results.expr_tys[hexpr_id]   — type of every HirExpr
//   results.call_type_args[id]   — concrete type-args at each
//                                  call site (for monomorphization)

import "stdlib.ox";
import "stdio.ox";
import "string.ox";
import "intrinsics.ox";
import "./util/vec.ox";
import "./util/strbuf.ox";
import "./ast.ox";
import "./hir.ox";

// ---------------------------------------------------------------- //
// Ty kind tags                                                     //
// ---------------------------------------------------------------- //

fn TYK_PRIM()  -> u8 { 0 }
fn TYK_UNIT()  -> u8 { 1 }
fn TYK_NEVER() -> u8 { 2 }
fn TYK_PTR()   -> u8 { 3 }
fn TYK_ARRAY() -> u8 { 4 }
fn TYK_ADT()   -> u8 { 5 }
fn TYK_FN()    -> u8 { 6 }
fn TYK_PARAM() -> u8 { 7 }
fn TYK_INFER() -> u8 { 8 }
fn TYK_ERROR() -> u8 { 9 }

// ---------------------------------------------------------------- //
// PrimTy tags                                                      //
// ---------------------------------------------------------------- //

fn PRIM_I8()    -> u8 { 0 }
fn PRIM_I16()   -> u8 { 1 }
fn PRIM_I32()   -> u8 { 2 }
fn PRIM_I64()   -> u8 { 3 }
fn PRIM_U8()    -> u8 { 4 }
fn PRIM_U16()   -> u8 { 5 }
fn PRIM_U32()   -> u8 { 6 }
fn PRIM_U64()   -> u8 { 7 }
fn PRIM_BOOL()  -> u8 { 8 }
fn PRIM_USIZE() -> u8 { 9 }
fn PRIM_ISIZE() -> u8 {10 }

fn prim_is_int(p: u8) -> bool {
    if p == PRIM_BOOL() { return false; }
    return true;
}

fn prim_is_signed(p: u8) -> bool {
    p == PRIM_I8() || p == PRIM_I16() || p == PRIM_I32() || p == PRIM_I64()
        || p == PRIM_ISIZE()
}

fn prim_byte_width(p: u8) -> u32 {
    if p == PRIM_I8() || p == PRIM_U8() || p == PRIM_BOOL() { return 1; }
    if p == PRIM_I16() || p == PRIM_U16() { return 2; }
    if p == PRIM_I32() || p == PRIM_U32() { return 4; }
    if p == PRIM_I64() || p == PRIM_U64() { return 8; }
    if p == PRIM_USIZE() || p == PRIM_ISIZE() { return 8; }
    return 0;
}

fn prim_name_lookup(pool_ptr: *const [u8], off: usize, len: usize) -> i32 {
    if tc_name_eq_lit(pool_ptr, off, len, "i8", 2)    { return PRIM_I8()    as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "i16", 3)   { return PRIM_I16()   as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "i32", 3)   { return PRIM_I32()   as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "i64", 3)   { return PRIM_I64()   as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "u8", 2)    { return PRIM_U8()    as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "u16", 3)   { return PRIM_U16()   as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "u32", 3)   { return PRIM_U32()   as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "u64", 3)   { return PRIM_U64()   as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "bool", 4)  { return PRIM_BOOL()  as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "usize", 5) { return PRIM_USIZE() as i32; }
    if tc_name_eq_lit(pool_ptr, off, len, "isize", 5) { return PRIM_ISIZE() as i32; }
    return -1;
}

fn tc_name_eq_lit(pool_ptr: *const [u8], a_off: usize, a_len: usize,
               s: *const [u8], n: usize) -> bool {
    if a_len != n { return false; }
    let mut i: usize = 0;
    while i < a_len {
        if pool_ptr[a_off + i] != s[i] { return false; }
        i = i + 1;
    }
    true
}

// ---------------------------------------------------------------- //
// Ty                                                               //
// ---------------------------------------------------------------- //

struct Ty {
    kind: u8,

    // TYK_PRIM
    prim: u8,

    // TYK_PTR
    mutability: u8,
    pointee:    usize,    // TyId

    // TYK_ARRAY
    elem:        usize,
    len_is_some: bool,
    len_val:     u64,

    // TYK_ADT
    adt:        usize,           // HAdtId
    type_args:  Vec<usize>,      // TyId

    // TYK_FN  (params/ret + variadic flag)
    params:     Vec<usize>,
    ret:        usize,
    is_variadic: bool,

    // TYK_PARAM
    ty_param: usize,             // HTyParamId

    // TYK_INFER (resolved by binding)
    infer_id: usize,             // index into TyArena.infer_vars
}

fn ty_zero() -> Ty {
    Ty {
        kind: 0, prim: 0,
        mutability: 0, pointee: HID_NONE(),
        elem: HID_NONE(), len_is_some: false, len_val: 0,
        adt: HID_NONE(), type_args: vec_new::<usize>(),
        params: vec_new::<usize>(), ret: HID_NONE(),
        is_variadic: false,
        ty_param: HID_NONE(),
        infer_id: HID_NONE(),
    }
}

struct InferVar {
    binding:    usize,           // TyId or HID_NONE
    int_default: bool,           // resolves to i32 if unbound at finalize
    span_start: usize,
    span_end:   usize,
}

fn infer_var_zero() -> InferVar {
    InferVar { binding: HID_NONE(), int_default: false, span_start: 0, span_end: 0 }
}

// ---------------------------------------------------------------- //
// TyArena                                                          //
// ---------------------------------------------------------------- //

struct TyArena {
    tys:         Vec<Ty>,
    infer_vars:  Vec<InferVar>,

    // Pre-allocated common types
    i8_id:    usize, i16_id: usize, i32_id: usize, i64_id: usize,
    u8_id:    usize, u16_id: usize, u32_id: usize, u64_id: usize,
    bool_id:  usize,
    usize_id: usize, isize_id: usize,
    unit_id:  usize, never_id: usize, error_id: usize,
}

fn ty_arena_new() -> TyArena {
    let mut a: TyArena = TyArena {
        tys: vec_new::<Ty>(), infer_vars: vec_new::<InferVar>(),
        i8_id: 0, i16_id: 0, i32_id: 0, i64_id: 0,
        u8_id: 0, u16_id: 0, u32_id: 0, u64_id: 0,
        bool_id: 0, usize_id: 0, isize_id: 0,
        unit_id: 0, never_id: 0, error_id: 0,
    };
    a.i8_id    = mk_prim(&mut a, PRIM_I8());
    a.i16_id   = mk_prim(&mut a, PRIM_I16());
    a.i32_id   = mk_prim(&mut a, PRIM_I32());
    a.i64_id   = mk_prim(&mut a, PRIM_I64());
    a.u8_id    = mk_prim(&mut a, PRIM_U8());
    a.u16_id   = mk_prim(&mut a, PRIM_U16());
    a.u32_id   = mk_prim(&mut a, PRIM_U32());
    a.u64_id   = mk_prim(&mut a, PRIM_U64());
    a.bool_id  = mk_prim(&mut a, PRIM_BOOL());
    a.usize_id = mk_prim(&mut a, PRIM_USIZE());
    a.isize_id = mk_prim(&mut a, PRIM_ISIZE());

    let mut t_unit: Ty = ty_zero();
    t_unit.kind = TYK_UNIT();
    a.unit_id = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t_unit);

    let mut t_never: Ty = ty_zero();
    t_never.kind = TYK_NEVER();
    a.never_id = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t_never);

    let mut t_err: Ty = ty_zero();
    t_err.kind = TYK_ERROR();
    a.error_id = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t_err);

    a
}

fn mk_prim(a: *mut TyArena, p: u8) -> usize {
    let mut t: Ty = ty_zero();
    t.kind = TYK_PRIM();
    t.prim = p;
    let id: usize = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t);
    id
}

fn prim_id(a: *const TyArena, p: u8) -> usize {
    if p == PRIM_I8()    { return a.i8_id; }
    if p == PRIM_I16()   { return a.i16_id; }
    if p == PRIM_I32()   { return a.i32_id; }
    if p == PRIM_I64()   { return a.i64_id; }
    if p == PRIM_U8()    { return a.u8_id; }
    if p == PRIM_U16()   { return a.u16_id; }
    if p == PRIM_U32()   { return a.u32_id; }
    if p == PRIM_U64()   { return a.u64_id; }
    if p == PRIM_BOOL()  { return a.bool_id; }
    if p == PRIM_USIZE() { return a.usize_id; }
    if p == PRIM_ISIZE() { return a.isize_id; }
    return a.error_id;
}

fn mk_ptr(a: *mut TyArena, mutability: u8, pointee: usize) -> usize {
    let mut t: Ty = ty_zero();
    t.kind       = TYK_PTR();
    t.mutability = mutability;
    t.pointee    = pointee;
    let id: usize = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t);
    id
}

fn mk_array(a: *mut TyArena, elem: usize, len_is_some: bool, len_val: u64) -> usize {
    let mut t: Ty = ty_zero();
    t.kind        = TYK_ARRAY();
    t.elem        = elem;
    t.len_is_some = len_is_some;
    t.len_val     = len_val;
    let id: usize = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t);
    id
}

fn mk_adt(a: *mut TyArena, adt_id: usize, type_args: Vec<usize>) -> usize {
    let mut t: Ty = ty_zero();
    t.kind      = TYK_ADT();
    t.adt       = adt_id;
    t.type_args = type_args;
    let id: usize = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t);
    id
}

fn mk_param(a: *mut TyArena, tpid: usize) -> usize {
    let mut t: Ty = ty_zero();
    t.kind     = TYK_PARAM();
    t.ty_param = tpid;
    let id: usize = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t);
    id
}

fn mk_infer(a: *mut TyArena, int_default: bool, span_start: usize, span_end: usize) -> usize {
    let iv_id: usize = vec_len::<InferVar>(&a.infer_vars);
    let mut iv: InferVar = infer_var_zero();
    iv.int_default = int_default;
    iv.span_start  = span_start;
    iv.span_end    = span_end;
    vec_push::<InferVar>(&mut a.infer_vars, iv);

    let mut t: Ty = ty_zero();
    t.kind     = TYK_INFER();
    t.infer_id = iv_id;
    let id: usize = vec_len::<Ty>(&a.tys);
    vec_push::<Ty>(&mut a.tys, t);
    id
}

// Walk Infer indirection. Returns the Ty's underlying TyId after
// resolving Infer-bindings.
fn resolve(a: *const TyArena, id: usize) -> usize {
    let t: Ty = vec_get::<Ty>(&a.tys, id);
    if t.kind == TYK_INFER() {
        let iv: InferVar = vec_get::<InferVar>(&a.infer_vars, t.infer_id);
        if iv.binding == HID_NONE() {
            return id;
        }
        return resolve(a, iv.binding);
    }
    id
}

// ---------------------------------------------------------------- //
// TypeckResults                                                    //
// ---------------------------------------------------------------- //

struct CallTypeArgs {
    expr_id:   usize,
    type_args: Vec<usize>,
}

struct TypeckResults {
    tys:            TyArena,
    expr_tys:       Vec<usize>,         // indexed by HExprId; TyId or HID_NONE
    local_tys:      Vec<usize>,         // indexed by LocalId; TyId
    fn_sig_params:  Vec<Vec<usize>>,    // indexed by FnId; param TyIds
    fn_sig_ret:     Vec<usize>,         // indexed by FnId; ret TyId
    call_type_args: Vec<CallTypeArgs>,  // collected for mono
    errors:         Vec<TypeckError>,
}

fn TE_TYPE_MISMATCH()      -> u8 { 0 }
fn TE_UNRESOLVED_TYPE()    -> u8 { 1 }
fn TE_NOT_INDEXABLE()      -> u8 { 2 }
fn TE_FIELD_NOT_FOUND()    -> u8 { 3 }
fn TE_NOT_CALLABLE()       -> u8 { 4 }
fn TE_ARITY_MISMATCH()     -> u8 { 5 }
fn TE_DEREF_NON_PTR()      -> u8 { 6 }
fn TE_GENERIC_INFER_FAIL() -> u8 { 7 }

struct TypeckError {
    kind:       u8,
    span_start: usize,
    span_end:   usize,
}

fn tc_error_zero() -> TypeckError {
    TypeckError { kind: 0, span_start: 0, span_end: 0 }
}

fn tc_results_new(n_exprs: usize, n_locals: usize, n_fns: usize) -> TypeckResults {
    let mut r: TypeckResults = TypeckResults {
        tys:            ty_arena_new(),
        expr_tys:       vec_with_capacity::<usize>(n_exprs),
        local_tys:      vec_with_capacity::<usize>(n_locals),
        fn_sig_params:  vec_with_capacity::<Vec<usize>>(n_fns),
        fn_sig_ret:     vec_with_capacity::<usize>(n_fns),
        call_type_args: vec_new::<CallTypeArgs>(),
        errors:         vec_new::<TypeckError>(),
    };
    let mut i: usize = 0;
    while i < n_exprs { vec_push::<usize>(&mut r.expr_tys, HID_NONE()); i = i + 1; }
    let mut j: usize = 0;
    while j < n_locals { vec_push::<usize>(&mut r.local_tys, HID_NONE()); j = j + 1; }
    let mut k: usize = 0;
    while k < n_fns {
        vec_push::<Vec<usize>>(&mut r.fn_sig_params, vec_new::<usize>());
        vec_push::<usize>(&mut r.fn_sig_ret, HID_NONE());
        k = k + 1;
    }
    r
}

// ---------------------------------------------------------------- //
// Checker state                                                    //
// ---------------------------------------------------------------- //

struct Checker {
    program: HirProgram,             // owned
    results: TypeckResults,
    // Substitution map for the in-flight fn body / call site.
    // Entries indexed by HTyParamId; HID_NONE means "no substitution".
    subst:  Vec<usize>,
    // Current fn under inspection (for ret-type lookups in `return e`).
    current_fn: usize,
}

fn typeck(program: HirProgram) -> Checker {
    let n_exprs:  usize = vec_len::<HirExpr>(&program.exprs);
    let n_locals: usize = vec_len::<HirLocal>(&program.locals);
    let n_fns:    usize = vec_len::<HirFn>(&program.fns);
    let n_tps:    usize = vec_len::<TyParamInfo>(&program.ty_params);

    let mut c: Checker = Checker {
        program: program,
        results: tc_results_new(n_exprs, n_locals, n_fns),
        subst: vec_with_capacity::<usize>(n_tps),
        current_fn: HID_NONE(),
    };
    let mut i: usize = 0;
    while i < n_tps { vec_push::<usize>(&mut c.subst, HID_NONE()); i = i + 1; }

    // Pass A: resolve fn signatures (param + ret tys), local-param types.
    let fn_count: usize = vec_len::<HirFn>(&c.program.fns);
    let mut f: usize = 0;
    while f < fn_count {
        check_fn_sig(&mut c, f);
        f = f + 1;
    }

    // Pass B: walk fn bodies.
    let mut g: usize = 0;
    while g < fn_count {
        let fnf: HirFn = vec_get::<HirFn>(&c.program.fns, g);
        if fnf.body == HID_NONE() {
            g = g + 1;
            continue;
        }
        c.current_fn = g;
        let ret_ty: usize = vec_get::<usize>(&c.results.fn_sig_ret, g);
        check_block(&mut c, fnf.body, ret_ty);
        c.current_fn = HID_NONE();
        g = g + 1;
    }

    c
}

// ---------------------------------------------------------------- //
// Resolve HirTy → TyId                                             //
// ---------------------------------------------------------------- //

fn resolve_hir_ty(c: *mut Checker, hty_id: usize) -> usize {
    if hty_id == HID_NONE() {
        return c.results.tys.error_id;
    }
    let h: HirTy = vec_get::<HirTy>(&c.program.types, hty_id);
    if h.kind == HTY_NAMED() {
        // Try primitive lookup; fall through to Error.
        let pool_ptr: *const [u8] = strbuf_as_ptr(&c.program.pool);
        let pr: i32 = prim_name_lookup(pool_ptr, h.name.off, h.name.len);
        if pr >= 0 {
            return prim_id(&c.results.tys, pr as u8);
        }
        // Unresolved name — error type.
        let mut e: TypeckError = tc_error_zero();
        e.kind       = TE_UNRESOLVED_TYPE();
        e.span_start = h.span_start;
        e.span_end   = h.span_end;
        vec_push::<TypeckError>(&mut c.results.errors, e);
        return c.results.tys.error_id;
    }
    if h.kind == HTY_ADT() {
        let mut args: Vec<usize> = vec_new::<usize>();
        let an: usize = vec_len::<usize>(&h.type_args);
        let mut i: usize = 0;
        while i < an {
            let aid: usize = vec_get::<usize>(&h.type_args, i);
            let t: usize = resolve_hir_ty(c, aid);
            vec_push::<usize>(&mut args, t);
            i = i + 1;
        }
        return mk_adt(&mut c.results.tys, h.adt, args);
    }
    if h.kind == HTY_PARAM() {
        return mk_param(&mut c.results.tys, h.ty_param);
    }
    if h.kind == HTY_PTR() {
        let pointee: usize = resolve_hir_ty(c, h.pointee);
        return mk_ptr(&mut c.results.tys, h.mutability, pointee);
    }
    if h.kind == HTY_ARRAY() {
        let elem: usize = resolve_hir_ty(c, h.elem);
        return mk_array(&mut c.results.tys, elem, h.len_is_some, h.len_val);
    }
    return c.results.tys.error_id;
}

// Apply current Checker.subst to a TyId. Walks structurally; replaces
// Param(tpid) with subst[tpid] when that slot is set. Other kinds are
// unchanged (we re-emit a fresh TyId for nested types).
fn substitute(c: *mut Checker, ty_id: usize) -> usize {
    let t: Ty = vec_get::<Ty>(&c.results.tys.tys, ty_id);
    if t.kind == TYK_PARAM() {
        let s: usize = vec_get::<usize>(&c.subst, t.ty_param);
        if s != HID_NONE() {
            return s;
        }
        return ty_id;
    }
    if t.kind == TYK_PTR() {
        let inner: usize = substitute(c, t.pointee);
        if inner == t.pointee { return ty_id; }
        return mk_ptr(&mut c.results.tys, t.mutability, inner);
    }
    if t.kind == TYK_ARRAY() {
        let inner: usize = substitute(c, t.elem);
        if inner == t.elem { return ty_id; }
        return mk_array(&mut c.results.tys, inner, t.len_is_some, t.len_val);
    }
    if t.kind == TYK_ADT() {
        let mut new_args: Vec<usize> = vec_new::<usize>();
        let n: usize = vec_len::<usize>(&t.type_args);
        let mut changed: bool = false;
        let mut i: usize = 0;
        while i < n {
            let a: usize = vec_get::<usize>(&t.type_args, i);
            let s: usize = substitute(c, a);
            if s != a { changed = true; }
            vec_push::<usize>(&mut new_args, s);
            i = i + 1;
        }
        if !changed { return ty_id; }
        return mk_adt(&mut c.results.tys, t.adt, new_args);
    }
    return ty_id;
}

// ---------------------------------------------------------------- //
// Unify                                                            //
// ---------------------------------------------------------------- //

fn unify(c: *mut Checker, a_id: usize, b_id: usize) -> bool {
    let ar: usize = resolve(&c.results.tys, a_id);
    let br: usize = resolve(&c.results.tys, b_id);
    if ar == br { return true; }
    let a: Ty = vec_get::<Ty>(&c.results.tys.tys, ar);
    let b: Ty = vec_get::<Ty>(&c.results.tys.tys, br);

    // Bind Infer variables.
    if a.kind == TYK_INFER() {
        bind_infer(c, a.infer_id, br);
        return true;
    }
    if b.kind == TYK_INFER() {
        bind_infer(c, b.infer_id, ar);
        return true;
    }

    // Error and Never absorb.
    if a.kind == TYK_ERROR() || b.kind == TYK_ERROR() { return true; }
    if a.kind == TYK_NEVER() || b.kind == TYK_NEVER() { return true; }

    if a.kind != b.kind { return false; }

    if a.kind == TYK_PRIM() {
        return a.prim == b.prim;
    }
    if a.kind == TYK_UNIT() { return true; }
    if a.kind == TYK_PTR() {
        // Mutability ignored at unify (loose); coercion check happens
        // elsewhere if we ever bother.
        return unify(c, a.pointee, b.pointee);
    }
    if a.kind == TYK_ARRAY() {
        if a.len_is_some {
            if !b.len_is_some { return false; }
            if a.len_val != b.len_val { return false; }
        } else {
            if b.len_is_some { return false; }
        }
        return unify(c, a.elem, b.elem);
    }
    if a.kind == TYK_ADT() {
        if a.adt != b.adt { return false; }
        let an: usize = vec_len::<usize>(&a.type_args);
        if an != vec_len::<usize>(&b.type_args) { return false; }
        let mut i: usize = 0;
        while i < an {
            let aa: usize = vec_get::<usize>(&a.type_args, i);
            let bb: usize = vec_get::<usize>(&b.type_args, i);
            if !unify(c, aa, bb) { return false; }
            i = i + 1;
        }
        return true;
    }
    if a.kind == TYK_PARAM() {
        return a.ty_param == b.ty_param;
    }
    return false;
}

fn bind_infer(c: *mut Checker, iv_id: usize, ty: usize) {
    let mut iv: InferVar = vec_get::<InferVar>(&c.results.tys.infer_vars, iv_id);
    iv.binding = ty;
    vec_set::<InferVar>(&mut c.results.tys.infer_vars, iv_id, iv);
}

// Emit a type-mismatch error (no rendering — kind+span only).
fn err_mismatch(c: *mut Checker, span_start: usize, span_end: usize) {
    let mut e: TypeckError = tc_error_zero();
    e.kind       = TE_TYPE_MISMATCH();
    e.span_start = span_start;
    e.span_end   = span_end;
    vec_push::<TypeckError>(&mut c.results.errors, e);
}

// ---------------------------------------------------------------- //
// Pass A: fn signatures                                            //
// ---------------------------------------------------------------- //

fn check_fn_sig(c: *mut Checker, fn_id: usize) {
    let f: HirFn = vec_get::<HirFn>(&c.program.fns, fn_id);

    let mut params: Vec<usize> = vec_new::<usize>();
    let pn: usize = vec_len::<usize>(&f.params);
    let mut i: usize = 0;
    while i < pn {
        let lid: usize = vec_get::<usize>(&f.params, i);
        let local: HirLocal = vec_get::<HirLocal>(&c.program.locals, lid);
        let ty_id: usize = if local.ty == HID_NONE() {
            c.results.tys.error_id
        } else {
            resolve_hir_ty(c, local.ty)
        };
        vec_push::<usize>(&mut params, ty_id);
        vec_set::<usize>(&mut c.results.local_tys, lid, ty_id);
        i = i + 1;
    }
    vec_set::<Vec<usize>>(&mut c.results.fn_sig_params, fn_id, params);

    let ret_ty: usize = if f.ret_ty == HID_NONE() {
        c.results.tys.unit_id
    } else {
        resolve_hir_ty(c, f.ret_ty)
    };
    vec_set::<usize>(&mut c.results.fn_sig_ret, fn_id, ret_ty);
}

// ---------------------------------------------------------------- //
// Pass B: bodies                                                   //
// ---------------------------------------------------------------- //

fn check_block(c: *mut Checker, block_id: usize, expected: usize) -> usize {
    let b: HirBlock = vec_get::<HirBlock>(&c.program.blocks, block_id);
    let n: usize = vec_len::<HirBlockItem>(&b.items);
    if n == 0 { return c.results.tys.unit_id; }

    // Items 0..n-1 are statements (typed but value discarded).
    let mut i: usize = 0;
    let last_idx: usize = n - 1;
    while i < last_idx {
        let bi: HirBlockItem = vec_get::<HirBlockItem>(&b.items, i);
        check_expr(c, bi.expr, HID_NONE());
        i = i + 1;
    }
    // Last item: if has_semi, its value is unit; else its value is
    // the block's value. Type accordingly.
    let last: HirBlockItem = vec_get::<HirBlockItem>(&b.items, last_idx);
    if last.has_semi {
        check_expr(c, last.expr, HID_NONE());
        return c.results.tys.unit_id;
    } else {
        return check_expr(c, last.expr, expected);
    }
}

fn check_expr(c: *mut Checker, expr_id: usize, expected: usize) -> usize {
    let e: HirExpr = vec_get::<HirExpr>(&c.program.exprs, expr_id);
    let k: u8 = e.kind;

    let ty: usize = if k == HEK_INT_LIT() {
        infer_int_lit(c, e, expected)
    } else if k == HEK_BOOL_LIT() {
        c.results.tys.bool_id
    } else if k == HEK_CHAR_LIT() {
        c.results.tys.u8_id
    } else if k == HEK_STR_LIT() {
        // *const [u8; len+1]
        let inner: usize = mk_array(&mut c.results.tys, c.results.tys.u8_id,
                                    true, (e.name.len + 1) as u64);
        mk_ptr(&mut c.results.tys, MUT_CONST(), inner)
    } else if k == HEK_NULL() {
        // *mut <fresh-infer>
        let pointee: usize = mk_infer(&mut c.results.tys, false, e.span_start, e.span_end);
        mk_ptr(&mut c.results.tys, MUT_MUT(), pointee)
    } else if k == HEK_LOCAL() {
        vec_get::<usize>(&c.results.local_tys, e.local_id)
    } else if k == HEK_FN() {
        // Reference to a fn — yields a callable. We don't bother
        // with FnPtr type for v0; the surrounding Call extracts the
        // fn directly via e.fn_id. Just return Error to signal "not
        // a value type".
        c.results.tys.error_id
    } else if k == HEK_UNRESOLVED() {
        c.results.tys.error_id
    } else if k == HEK_UNARY() {
        check_unary(c, e, expected)
    } else if k == HEK_BINARY() {
        check_binary(c, e, expected)
    } else if k == HEK_ASSIGN() {
        let lhs_ty: usize = check_expr(c, e.e1, HID_NONE());
        check_expr(c, e.e2, lhs_ty);
        c.results.tys.unit_id
    } else if k == HEK_CALL() {
        check_call(c, expr_id, e, expected)
    } else if k == HEK_INDEX() {
        check_index(c, e)
    } else if k == HEK_FIELD() {
        check_field(c, e)
    } else if k == HEK_STRUCT_LIT() {
        check_struct_lit(c, e, expected)
    } else if k == HEK_ARRAY_LIT() {
        check_array_lit(c, e, expected)
    } else if k == HEK_ARRAY_RPT() {
        check_array_rpt(c, e, expected)
    } else if k == HEK_ADDR_OF() {
        let inner: usize = check_expr(c, e.e1, HID_NONE());
        let mu: u8 = if e.mutable { MUT_MUT() } else { MUT_CONST() };
        mk_ptr(&mut c.results.tys, mu, inner)
    } else if k == HEK_CAST() {
        check_expr(c, e.e1, HID_NONE());
        resolve_hir_ty(c, e.t1)
    } else if k == HEK_IF() {
        check_if(c, e, expected)
    } else if k == HEK_BLOCK() {
        check_block(c, e.e3, expected)
    } else if k == HEK_RETURN() {
        if e.e1 != HID_NONE() {
            let ret_ty: usize = vec_get::<usize>(&c.results.fn_sig_ret, c.current_fn);
            check_expr(c, e.e1, ret_ty);
        }
        c.results.tys.never_id
    } else if k == HEK_LOOP() {
        check_loop(c, e, expected)
    } else if k == HEK_BREAK() {
        if e.e1 != HID_NONE() {
            check_expr(c, e.e1, HID_NONE());
        }
        c.results.tys.never_id
    } else if k == HEK_CONTINUE() {
        c.results.tys.never_id
    } else if k == HEK_LET() {
        check_let(c, e)
    } else if k == HEK_POISON() {
        c.results.tys.error_id
    } else {
        c.results.tys.error_id
    };

    vec_set::<usize>(&mut c.results.expr_tys, expr_id, ty);
    return ty;
}

// ---------------------------------------------------------------- //
// Per-kind handlers                                                //
// ---------------------------------------------------------------- //

fn infer_int_lit(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    if expected != HID_NONE() {
        let er: usize = resolve(&c.results.tys, expected);
        let et: Ty = vec_get::<Ty>(&c.results.tys.tys, er);
        if et.kind == TYK_PRIM() && prim_is_int(et.prim) {
            return er;
        }
        if et.kind == TYK_INFER() {
            // Bind the expected infer to int-default i32 for now;
            // a more correct path would carry int-default through
            // unification.
            return c.results.tys.i32_id;
        }
    }
    return c.results.tys.i32_id;
}

fn check_unary(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    if e.op == UN_NEG() || e.op == UN_BITNOT() {
        let inner_ty: usize = check_expr(c, e.e1, expected);
        return inner_ty;
    }
    if e.op == UN_NOT() {
        check_expr(c, e.e1, c.results.tys.bool_id);
        return c.results.tys.bool_id;
    }
    if e.op == UN_DEREF() {
        let inner_ty: usize = check_expr(c, e.e1, HID_NONE());
        let r: usize = resolve(&c.results.tys, inner_ty);
        let t: Ty = vec_get::<Ty>(&c.results.tys.tys, r);
        if t.kind == TYK_PTR() {
            return t.pointee;
        }
        let mut err: TypeckError = tc_error_zero();
        err.kind       = TE_DEREF_NON_PTR();
        err.span_start = e.span_start;
        err.span_end   = e.span_end;
        vec_push::<TypeckError>(&mut c.results.errors, err);
        return c.results.tys.error_id;
    }
    return c.results.tys.error_id;
}

fn check_binary(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    let op: u8 = e.op;
    // Comparisons, logical ops yield bool.
    let is_cmp: bool = op == BIN_EQ() || op == BIN_NE() || op == BIN_LT() || op == BIN_LE()
        || op == BIN_GT() || op == BIN_GE();
    let is_logic: bool = op == BIN_AND() || op == BIN_OR();

    if is_logic {
        check_expr(c, e.e1, c.results.tys.bool_id);
        check_expr(c, e.e2, c.results.tys.bool_id);
        return c.results.tys.bool_id;
    }
    if is_cmp {
        let lhs_ty: usize = check_expr(c, e.e1, HID_NONE());
        check_expr(c, e.e2, lhs_ty);
        return c.results.tys.bool_id;
    }
    // Arithmetic / bitwise / shift: lhs and rhs same type; result == lhs.
    let lhs_ty: usize = check_expr(c, e.e1, expected);
    check_expr(c, e.e2, lhs_ty);
    return lhs_ty;
}

fn check_call(c: *mut Checker, expr_id: usize, e: HirExpr, expected: usize) -> usize {
    // Resolve callee. We recognize the direct `Fn(fid)` form to apply
    // its signature directly. Anything else is "not callable".
    let callee_e: HirExpr = vec_get::<HirExpr>(&c.program.exprs, e.e1);
    if callee_e.kind != HEK_FN() {
        // Type the args as best we can; return Error.
        let an: usize = vec_len::<usize>(&e.args);
        let mut i: usize = 0;
        while i < an {
            let a: usize = vec_get::<usize>(&e.args, i);
            check_expr(c, a, HID_NONE());
            i = i + 1;
        }
        let mut err: TypeckError = tc_error_zero();
        err.kind       = TE_NOT_CALLABLE();
        err.span_start = e.span_start;
        err.span_end   = e.span_end;
        vec_push::<TypeckError>(&mut c.results.errors, err);
        return c.results.tys.error_id;
    }
    let fid: usize = callee_e.fn_id;

    // Build subst from turbofish or fresh-infers.
    let f: HirFn = vec_get::<HirFn>(&c.program.fns, fid);
    let gpn: usize = vec_len::<usize>(&f.generic_params);
    let tan: usize = vec_len::<usize>(&e.type_args);

    let mut local_args: Vec<usize> = vec_new::<usize>();
    let mut i: usize = 0;
    while i < gpn {
        let tpid: usize = vec_get::<usize>(&f.generic_params, i);
        let arg_ty: usize = if i < tan {
            let h_arg: usize = vec_get::<usize>(&e.type_args, i);
            resolve_hir_ty(c, h_arg)
        } else {
            mk_infer(&mut c.results.tys, false, e.span_start, e.span_end)
        };
        vec_push::<usize>(&mut local_args, arg_ty);
        // Install in c.subst at slot tpid for substitute() to find.
        vec_set::<usize>(&mut c.subst, tpid, arg_ty);
        i = i + 1;
    }

    // Substitute params and ret with the subst map.
    let sig_params: Vec<usize> = vec_get::<Vec<usize>>(&c.results.fn_sig_params, fid);
    let pn: usize = vec_len::<usize>(&sig_params);
    let mut params_subst: Vec<usize> = vec_new::<usize>();
    let mut p: usize = 0;
    while p < pn {
        let pt: usize = vec_get::<usize>(&sig_params, p);
        let s: usize = substitute(c, pt);
        vec_push::<usize>(&mut params_subst, s);
        p = p + 1;
    }
    let ret_pre: usize = vec_get::<usize>(&c.results.fn_sig_ret, fid);
    let ret_subst: usize = substitute(c, ret_pre);

    // Clear subst slots (they're per-call).
    let mut q: usize = 0;
    while q < gpn {
        let tpid: usize = vec_get::<usize>(&f.generic_params, q);
        vec_set::<usize>(&mut c.subst, tpid, HID_NONE());
        q = q + 1;
    }

    // Type-check args against substituted params.
    let an: usize = vec_len::<usize>(&e.args);
    let mut j: usize = 0;
    while j < an {
        let arg_id: usize = vec_get::<usize>(&e.args, j);
        let exp_ty: usize = if j < pn {
            vec_get::<usize>(&params_subst, j)
        } else if f.is_variadic {
            // Variadic positions: no expected type at unify time.
            HID_NONE()
        } else {
            HID_NONE()
        };
        check_expr(c, arg_id, exp_ty);
        j = j + 1;
    }

    // Record call-type-args for mono.
    if gpn > 0 {
        vec_push::<CallTypeArgs>(&mut c.results.call_type_args, CallTypeArgs {
            expr_id: expr_id, type_args: local_args,
        });
    }

    // Coerce ret to expected if relevant.
    if expected != HID_NONE() {
        unify(c, ret_subst, expected);
    }
    return ret_subst;
}

fn check_index(c: *mut Checker, e: HirExpr) -> usize {
    let base_ty: usize = check_expr(c, e.e1, HID_NONE());
    check_expr(c, e.e2, c.results.tys.usize_id);
    let r: usize = resolve(&c.results.tys, base_ty);
    let t: Ty = vec_get::<Ty>(&c.results.tys.tys, r);
    let inner: usize = peel_ptrs_to_array(&c.results.tys, r);
    if inner != HID_NONE() {
        let arr_t: Ty = vec_get::<Ty>(&c.results.tys.tys, inner);
        return arr_t.elem;
    }
    if t.kind == TYK_ARRAY() {
        return t.elem;
    }
    let mut err: TypeckError = tc_error_zero();
    err.kind       = TE_NOT_INDEXABLE();
    err.span_start = e.span_start;
    err.span_end   = e.span_end;
    vec_push::<TypeckError>(&mut c.results.errors, err);
    return c.results.tys.error_id;
}

// Walk through Ptr layers to find an underlying Array. Returns the
// Array's TyId, or HID_NONE on miss.
fn peel_ptrs_to_array(a: *const TyArena, ty_id: usize) -> usize {
    let mut cur: usize = resolve(a, ty_id);
    let mut peeled: bool = false;
    loop {
        let t: Ty = vec_get::<Ty>(&a.tys, cur);
        if t.kind == TYK_PTR() {
            cur = resolve(a, t.pointee);
            peeled = true;
        } else if t.kind == TYK_ARRAY() {
            if peeled { return cur; } else { return cur; }
        } else {
            return HID_NONE();
        }
    }
}

fn check_field(c: *mut Checker, e: HirExpr) -> usize {
    let base_ty: usize = check_expr(c, e.e1, HID_NONE());
    let cur: usize = peel_to_adt(&c.results.tys, base_ty);
    if cur == HID_NONE() {
        let mut err: TypeckError = tc_error_zero();
        err.kind       = TE_FIELD_NOT_FOUND();
        err.span_start = e.span_start;
        err.span_end   = e.span_end;
        vec_push::<TypeckError>(&mut c.results.errors, err);
        return c.results.tys.error_id;
    }
    let adt_ty: Ty = vec_get::<Ty>(&c.results.tys.tys, cur);
    let adt: HirAdt = vec_get::<HirAdt>(&c.program.adts, adt_ty.adt);
    let pool_ptr: *const [u8] = strbuf_as_ptr(&c.program.pool);
    let fc: usize = vec_len::<HirField>(&adt.fields);
    let mut i: usize = 0;
    while i < fc {
        let fd: HirField = vec_get::<HirField>(&adt.fields, i);
        if tc_name_eq_lit_pool(pool_ptr, fd.name.off, fd.name.len, e.name.off, e.name.len) {
            // Resolve field's type, substituting any Param tpid with
            // the adt_ty.type_args entries.
            let fld_ty: usize = resolve_hir_ty(c, fd.ty);
            let subst: usize = substitute_with(c, fld_ty, adt.generic_params, adt_ty.type_args);
            return subst;
        }
        i = i + 1;
    }
    let mut err: TypeckError = tc_error_zero();
    err.kind       = TE_FIELD_NOT_FOUND();
    err.span_start = e.span_start;
    err.span_end   = e.span_end;
    vec_push::<TypeckError>(&mut c.results.errors, err);
    return c.results.tys.error_id;
}

fn tc_name_eq_lit_pool(pool_ptr: *const [u8], a_off: usize, a_len: usize,
                    b_off: usize, b_len: usize) -> bool {
    if a_len != b_len { return false; }
    let mut i: usize = 0;
    while i < a_len {
        if pool_ptr[a_off + i] != pool_ptr[b_off + i] { return false; }
        i = i + 1;
    }
    true
}

fn peel_to_adt(a: *const TyArena, ty_id: usize) -> usize {
    let mut cur: usize = resolve(a, ty_id);
    loop {
        let t: Ty = vec_get::<Ty>(&a.tys, cur);
        if t.kind == TYK_PTR() {
            cur = resolve(a, t.pointee);
        } else if t.kind == TYK_ADT() {
            return cur;
        } else {
            return HID_NONE();
        }
    }
}

// Substitute Param(tpid) → tparams[i_of_tpid] / args[i_of_tpid].
// `params` is the ordered list of HTyParamId for the owner; `args`
// is the parallel list of TyId arguments. tparams.len() == args.len().
fn substitute_with(c: *mut Checker, ty_id: usize,
                   params: Vec<usize>, args: Vec<usize>) -> usize {
    // Set subst table from (params, args), substitute, restore.
    let n: usize = vec_len::<usize>(&params);
    if n == 0 { return ty_id; }
    let mut i: usize = 0;
    while i < n {
        let tpid: usize = vec_get::<usize>(&params, i);
        let arg:  usize = vec_get::<usize>(&args, i);
        vec_set::<usize>(&mut c.subst, tpid, arg);
        i = i + 1;
    }
    let result: usize = substitute(c, ty_id);
    let mut j: usize = 0;
    while j < n {
        let tpid: usize = vec_get::<usize>(&params, j);
        vec_set::<usize>(&mut c.subst, tpid, HID_NONE());
        j = j + 1;
    }
    return result;
}

fn check_struct_lit(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    let an: usize = vec_len::<usize>(&e.type_args);
    let mut args: Vec<usize> = vec_new::<usize>();

    // Resolve adt → expected number of type-args.
    let adt_id: usize = e.adt;
    if adt_id == HID_NONE() {
        // Type-name didn't resolve at HIR; lower already filed an error.
        let sn: usize = vec_len::<HirStructLitField>(&e.sl_fields);
        let mut i: usize = 0;
        while i < sn {
            let sf: HirStructLitField = vec_get::<HirStructLitField>(&e.sl_fields, i);
            check_expr(c, sf.value, HID_NONE());
            i = i + 1;
        }
        return c.results.tys.error_id;
    }
    let adt: HirAdt = vec_get::<HirAdt>(&c.program.adts, adt_id);
    let gpn: usize = vec_len::<usize>(&adt.generic_params);

    // Build type-args: turbofish if present, else fresh-infer slots.
    let mut i: usize = 0;
    while i < gpn {
        if i < an {
            let h_arg: usize = vec_get::<usize>(&e.type_args, i);
            vec_push::<usize>(&mut args, resolve_hir_ty(c, h_arg));
        } else {
            vec_push::<usize>(&mut args, mk_infer(&mut c.results.tys,
                                                   false, e.span_start, e.span_end));
        }
        i = i + 1;
    }

    // Type each field expression against its declared (substituted) type.
    let pool_ptr: *const [u8] = strbuf_as_ptr(&c.program.pool);
    let sn: usize = vec_len::<HirStructLitField>(&e.sl_fields);
    let fc: usize = vec_len::<HirField>(&adt.fields);
    let mut k: usize = 0;
    while k < sn {
        let sf: HirStructLitField = vec_get::<HirStructLitField>(&e.sl_fields, k);
        // Find matching field by name.
        let mut found_ty: usize = c.results.tys.error_id;
        let mut j: usize = 0;
        while j < fc {
            let fd: HirField = vec_get::<HirField>(&adt.fields, j);
            if tc_name_eq_lit_pool(pool_ptr, fd.name.off, fd.name.len, sf.name.off, sf.name.len) {
                let raw: usize = resolve_hir_ty(c, fd.ty);
                found_ty = substitute_with(c, raw, adt.generic_params, args);
                break;
            }
            j = j + 1;
        }
        check_expr(c, sf.value, found_ty);
        k = k + 1;
    }

    // Coerce expected if useful.
    let result: usize = mk_adt(&mut c.results.tys, adt_id, args);
    if expected != HID_NONE() {
        unify(c, result, expected);
    }
    return result;
}

fn check_array_lit(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    // Element expected: peel expected through ptr/array if any.
    let elem_expected: usize = if expected != HID_NONE() {
        let er: usize = resolve(&c.results.tys, expected);
        let et: Ty = vec_get::<Ty>(&c.results.tys.tys, er);
        if et.kind == TYK_ARRAY() { et.elem } else { HID_NONE() }
    } else {
        HID_NONE()
    };

    let an: usize = vec_len::<usize>(&e.args);
    let mut elem_ty: usize = if elem_expected != HID_NONE() {
        elem_expected
    } else if an > 0 {
        let first: usize = vec_get::<usize>(&e.args, 0);
        check_expr(c, first, HID_NONE())
    } else {
        c.results.tys.unit_id           // empty `[]` defaults to [(); 0]
    };
    if elem_expected == HID_NONE() && an > 0 {
        // We already checked args[0]; check the rest.
        let mut i: usize = 1;
        while i < an {
            let a: usize = vec_get::<usize>(&e.args, i);
            check_expr(c, a, elem_ty);
            i = i + 1;
        }
    } else {
        let mut i: usize = 0;
        while i < an {
            let a: usize = vec_get::<usize>(&e.args, i);
            check_expr(c, a, elem_ty);
            i = i + 1;
        }
    }
    return mk_array(&mut c.results.tys, elem_ty, true, an as u64);
}

fn check_array_rpt(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    let elem_expected: usize = if expected != HID_NONE() {
        let er: usize = resolve(&c.results.tys, expected);
        let et: Ty = vec_get::<Ty>(&c.results.tys.tys, er);
        if et.kind == TYK_ARRAY() { et.elem } else { HID_NONE() }
    } else {
        HID_NONE()
    };
    let elem_ty: usize = check_expr(c, e.e1, elem_expected);
    return mk_array(&mut c.results.tys, elem_ty, true, e.int_val);
}

fn check_if(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    check_expr(c, e.e1, c.results.tys.bool_id);
    let then_ty: usize = check_block(c, e.e3, expected);
    if e.e4 == HID_NONE() {
        return c.results.tys.unit_id;
    }
    if e.e4_is_block {
        let else_ty: usize = check_block(c, e.e4, expected);
        unify(c, then_ty, else_ty);
        return then_ty;
    } else {
        let else_ty: usize = check_expr(c, e.e4, expected);
        unify(c, then_ty, else_ty);
        return then_ty;
    }
}

fn check_loop(c: *mut Checker, e: HirExpr, expected: usize) -> usize {
    if e.loop_source == LS_WHILE() {
        check_expr(c, e.e1, c.results.tys.bool_id);
        check_block(c, e.e3, c.results.tys.unit_id);
        return c.results.tys.unit_id;
    }
    if e.loop_source == LS_FOR() {
        if e.e2 != HID_NONE() {
            check_expr(c, e.e2, HID_NONE());      // init
        }
        if e.e1 != HID_NONE() {
            check_expr(c, e.e1, c.results.tys.bool_id);
        }
        if e.t1 != HID_NONE() {
            check_expr(c, e.t1, HID_NONE());      // update
        }
        check_block(c, e.e3, c.results.tys.unit_id);
        return c.results.tys.unit_id;
    }
    // LS_LOOP — produces Never if no break, expected otherwise.
    check_block(c, e.e3, c.results.tys.unit_id);
    if e.has_break { return c.results.tys.unit_id; }
    return c.results.tys.never_id;
}

fn check_let(c: *mut Checker, e: HirExpr) -> usize {
    let local: HirLocal = vec_get::<HirLocal>(&c.program.locals, e.local_id);
    let ann_ty: usize = if local.ty == HID_NONE() {
        HID_NONE()
    } else {
        resolve_hir_ty(c, local.ty)
    };
    let init_ty: usize = if e.e1 != HID_NONE() {
        check_expr(c, e.e1, ann_ty)
    } else {
        HID_NONE()
    };
    let final_ty: usize = if ann_ty != HID_NONE() {
        ann_ty
    } else if init_ty != HID_NONE() {
        init_ty
    } else {
        c.results.tys.error_id
    };
    vec_set::<usize>(&mut c.results.local_tys, e.local_id, final_ty);
    return c.results.tys.unit_id;
}
