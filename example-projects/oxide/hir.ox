// hir.ox — name-resolved IR.
//
// AST identifiers are resolved into typed-index handles (FnId,
// LocalId, HAdtId). Type references are resolved into one of:
//   - Adt(HAdtId)        — user-defined struct
//   - Param(HTyParamId)  — fn/adt generic parameter
//   - Named(name)        — anything else (primitives, etc.) — typeck
//                          finishes the lookup
// Field names stay as strings until typeck.
//
// Same tagged-struct + per-node Vec design as ast.ox.

import "stdlib.ox";
import "intrinsics.ox";
import "./util/vec.ox";
import "./util/strbuf.ox";
import "./ast.ox";

// ---------------------------------------------------------------- //
// Sentinel                                                         //
// ---------------------------------------------------------------- //

fn HID_NONE() -> usize { 18446744073709551615 }     // u64::MAX

// ---------------------------------------------------------------- //
// HIR Expr kind tags                                               //
// ---------------------------------------------------------------- //

fn HEK_INT_LIT()    -> u8 { 0 }
fn HEK_BOOL_LIT()   -> u8 { 1 }
fn HEK_CHAR_LIT()   -> u8 { 2 }
fn HEK_STR_LIT()    -> u8 { 3 }
fn HEK_NULL()       -> u8 { 4 }
fn HEK_LOCAL()      -> u8 { 5 }
fn HEK_FN()         -> u8 { 6 }
fn HEK_UNRESOLVED() -> u8 { 7 }
fn HEK_UNARY()      -> u8 { 8 }
fn HEK_BINARY()     -> u8 { 9 }
fn HEK_ASSIGN()     -> u8 { 10 }
fn HEK_CALL()       -> u8 { 11 }
fn HEK_INDEX()      -> u8 { 12 }
fn HEK_FIELD()      -> u8 { 13 }
fn HEK_STRUCT_LIT() -> u8 { 14 }
fn HEK_ARRAY_LIT()  -> u8 { 15 }
fn HEK_ARRAY_RPT()  -> u8 { 16 }
fn HEK_ADDR_OF()    -> u8 { 17 }
fn HEK_CAST()       -> u8 { 18 }
fn HEK_IF()         -> u8 { 19 }
fn HEK_BLOCK()      -> u8 { 20 }
fn HEK_RETURN()     -> u8 { 21 }
fn HEK_LOOP()       -> u8 { 22 }
fn HEK_BREAK()      -> u8 { 23 }
fn HEK_CONTINUE()   -> u8 { 24 }
fn HEK_LET()        -> u8 { 25 }
fn HEK_POISON()     -> u8 { 26 }

// ---------------------------------------------------------------- //
// HirTy kind tags                                                  //
// ---------------------------------------------------------------- //

fn HTY_NAMED() -> u8 { 0 }      // Anything name-like that didn't resolve to Adt/Param
fn HTY_ADT()   -> u8 { 1 }
fn HTY_PARAM() -> u8 { 2 }
fn HTY_PTR()   -> u8 { 3 }
fn HTY_ARRAY() -> u8 { 4 }
fn HTY_ERROR() -> u8 { 5 }

// ---------------------------------------------------------------- //
// LoopSource tags                                                  //
// ---------------------------------------------------------------- //

fn LS_WHILE() -> u8 { 0 }
fn LS_LOOP()  -> u8 { 1 }
fn LS_FOR()   -> u8 { 2 }

// ---------------------------------------------------------------- //
// Intrinsic tags                                                   //
// ---------------------------------------------------------------- //

fn INTR_NONE()      -> u8 { 0 }
fn INTR_TRANSMUTE() -> u8 { 1 }
fn INTR_SIZE_OF()   -> u8 { 2 }

// ---------------------------------------------------------------- //
// TyParam owner tags                                               //
// ---------------------------------------------------------------- //

fn TPO_FN()  -> u8 { 0 }
fn TPO_ADT() -> u8 { 1 }

// ---------------------------------------------------------------- //
// HirError tags                                                    //
// ---------------------------------------------------------------- //

fn HE_DUPLICATE_GLOBAL()       -> u8 { 0 }
fn HE_DUPLICATE_TY_PARAM()     -> u8 { 1 }
fn HE_DUPLICATE_FIELD()        -> u8 { 2 }
fn HE_UNRESOLVED_NAME()        -> u8 { 3 }    // value-namespace lookup failed
fn HE_UNRESOLVED_TYPE_NAME()   -> u8 { 4 }    // type-namespace lookup failed (kept for typeck-time signal; v0 lower keeps Named)
fn HE_BREAK_OUTSIDE_LOOP()     -> u8 { 5 }
fn HE_CONTINUE_OUTSIDE_LOOP()  -> u8 { 6 }
fn HE_BODYLESS_FN_OUTSIDE_EXT()-> u8 { 7 }
fn HE_EXTERN_FN_HAS_BODY()     -> u8 { 8 }
fn HE_GENERIC_EXTERN_FN()      -> u8 { 9 }
fn HE_NON_PLACE_ADDR_OF()      -> u8 { 10 }
fn HE_NON_PLACE_ASSIGN()       -> u8 { 11 }

// ---------------------------------------------------------------- //
// Common payload structs                                           //
// ---------------------------------------------------------------- //

struct HName {
    off:        usize,    // pool offset
    len:        usize,    // pool byte length
    span_start: usize,
    span_end:   usize,
}

fn hname_zero() -> HName {
    HName { off: 0, len: 0, span_start: 0, span_end: 0 }
}

fn hname_from_ident(id: Ident) -> HName {
    HName {
        off:        id.name_off,
        len:        id.name_len,
        span_start: id.span_start,
        span_end:   id.span_end,
    }
}

// ---------------------------------------------------------------- //
// HirTy                                                            //
// ---------------------------------------------------------------- //

struct HirTy {
    kind: u8,

    // HTY_NAMED: name only (string)
    name: HName,

    // HTY_ADT: adt id + type_args
    adt: usize,                   // HAdtId
    type_args: Vec<usize>,        // HirTyId

    // HTY_PARAM: ty_param id
    ty_param: usize,              // HTyParamId

    // HTY_PTR: mutability + pointee
    mutability: u8,
    pointee:    usize,            // HirTyId

    // HTY_ARRAY: elem + len (i64; -1 = unsized, else u64)
    elem:    usize,               // HirTyId
    len_is_some: bool,
    len_val: u64,

    span_start: usize,
    span_end:   usize,
}

fn hir_ty_zero() -> HirTy {
    HirTy {
        kind: 0,
        name: hname_zero(),
        adt: HID_NONE(),
        type_args: vec_new::<usize>(),
        ty_param: HID_NONE(),
        mutability: 0,
        pointee: HID_NONE(),
        elem: HID_NONE(),
        len_is_some: false,
        len_val: 0,
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// HirField                                                         //
// ---------------------------------------------------------------- //

struct HirField {
    name:       HName,
    ty:         usize,            // HirTyId
    span_start: usize,
    span_end:   usize,
}

// ---------------------------------------------------------------- //
// HirAdt                                                           //
// ---------------------------------------------------------------- //

struct HirAdt {
    name: HName,
    generic_params: Vec<usize>,   // HTyParamId
    fields: Vec<HirField>,
    span_start: usize,
    span_end:   usize,
}

fn hir_adt_zero() -> HirAdt {
    HirAdt {
        name: hname_zero(),
        generic_params: vec_new::<usize>(),
        fields: vec_new::<HirField>(),
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// HirLocal                                                         //
// ---------------------------------------------------------------- //

struct HirLocal {
    name:       HName,
    mutable:    bool,
    ty:         usize,            // HirTyId or HID_NONE
    span_start: usize,
    span_end:   usize,
}

fn hir_local_zero() -> HirLocal {
    HirLocal {
        name: hname_zero(),
        mutable: false,
        ty: HID_NONE(),
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// HirFn                                                            //
// ---------------------------------------------------------------- //

struct HirFn {
    name: HName,
    generic_params: Vec<usize>,   // HTyParamId
    params: Vec<usize>,           // LocalId
    ret_ty: usize,                // HirTyId or HID_NONE
    body:   usize,                // HBlockId or HID_NONE
    is_extern:    bool,
    is_variadic:  bool,
    intrinsic:    u8,             // INTR_*
    span_start:   usize,
    span_end:     usize,
}

fn hir_fn_zero() -> HirFn {
    HirFn {
        name: hname_zero(),
        generic_params: vec_new::<usize>(),
        params: vec_new::<usize>(),
        ret_ty: HID_NONE(),
        body:   HID_NONE(),
        is_extern: false,
        is_variadic: false,
        intrinsic: INTR_NONE(),
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// HirBlock                                                         //
// ---------------------------------------------------------------- //

struct HirBlockItem {
    expr:     usize,              // HExprId
    has_semi: bool,
}

struct HirBlock {
    items:      Vec<HirBlockItem>,
    span_start: usize,
    span_end:   usize,
}

fn hir_block_zero() -> HirBlock {
    HirBlock {
        items: vec_new::<HirBlockItem>(),
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// HirExpr                                                          //
// ---------------------------------------------------------------- //

struct HirStructLitField {
    name:       HName,
    value:      usize,            // HExprId
    span_start: usize,
    span_end:   usize,
}

struct HirExpr {
    kind: u8,

    // Primitive payloads
    int_val:  u64,
    bool_val: bool,
    char_val: u8,

    // Name payload (StrLit decoded bytes; Field name; Unresolved)
    name: HName,

    // Resolved-id payloads (per kind)
    local_id: usize,              // HEK_LOCAL
    fn_id:    usize,              // HEK_FN
    adt:      usize,              // HEK_STRUCT_LIT (HAdtId)

    // Children
    e1: usize, e2: usize, e3: usize, e4: usize,
    e4_is_block: bool,
    t1: usize,                    // HirTyId

    // Lists
    args:      Vec<usize>,        // call args, array elems
    type_args: Vec<usize>,        // turbofish (HirTyId)
    sl_fields: Vec<HirStructLitField>,

    // Op codes / flags
    op:           u8,             // UnOp / BinOp / AssignOp / Mutability
    mutable:      bool,
    is_place:     bool,           // place expression?
    has_break:    bool,           // HEK_LOOP only
    loop_source:  u8,             // HEK_LOOP only

    span_start: usize,
    span_end:   usize,
}

fn hir_expr_zero() -> HirExpr {
    HirExpr {
        kind: 0,
        int_val: 0, bool_val: false, char_val: 0,
        name: hname_zero(),
        local_id: HID_NONE(),
        fn_id:    HID_NONE(),
        adt:      HID_NONE(),
        e1: HID_NONE(), e2: HID_NONE(), e3: HID_NONE(), e4: HID_NONE(),
        e4_is_block: false,
        t1: HID_NONE(),
        args:      vec_new::<usize>(),
        type_args: vec_new::<usize>(),
        sl_fields: vec_new::<HirStructLitField>(),
        op: 0,
        mutable: false,
        is_place: false,
        has_break: false,
        loop_source: 0,
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// TyParamInfo                                                      //
// ---------------------------------------------------------------- //

struct TyParamInfo {
    owner_kind:   u8,             // TPO_FN | TPO_ADT
    owner_id:     usize,          // FnId or HAdtId
    idx_in_owner: u32,
    name:         HName,
    span_start:   usize,
    span_end:     usize,
}

fn ty_param_zero() -> TyParamInfo {
    TyParamInfo {
        owner_kind: 0, owner_id: 0, idx_in_owner: 0,
        name: hname_zero(),
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// HirError                                                         //
// ---------------------------------------------------------------- //

struct HirError {
    kind:        u8,
    span_start:  usize,
    span_end:    usize,
    // Pool offsets/lengths for "name1" / "name2" context strings.
    name1:       HName,
    name2:       HName,
}

fn hir_error_zero() -> HirError {
    HirError {
        kind: 0, span_start: 0, span_end: 0,
        name1: hname_zero(), name2: hname_zero(),
    }
}

// ---------------------------------------------------------------- //
// HirProgram — root container                                      //
// ---------------------------------------------------------------- //

struct HirProgram {
    fns:       Vec<HirFn>,
    adts:      Vec<HirAdt>,
    locals:    Vec<HirLocal>,
    exprs:     Vec<HirExpr>,
    blocks:    Vec<HirBlock>,
    types:     Vec<HirTy>,        // HIR types are interned-ish (one per source occurrence)
    ty_params: Vec<TyParamInfo>,

    pool: StrBuf,                 // shared name pool

    root_fns:  Vec<usize>,
    root_adts: Vec<usize>,

    errors:    Vec<HirError>,
}

fn hir_program_new(pool: StrBuf) -> HirProgram {
    HirProgram {
        fns:       vec_new::<HirFn>(),
        adts:      vec_new::<HirAdt>(),
        locals:    vec_new::<HirLocal>(),
        exprs:     vec_new::<HirExpr>(),
        blocks:    vec_new::<HirBlock>(),
        types:     vec_new::<HirTy>(),
        ty_params: vec_new::<TyParamInfo>(),
        pool:      pool,
        root_fns:  vec_new::<usize>(),
        root_adts: vec_new::<usize>(),
        errors:    vec_new::<HirError>(),
    }
}
