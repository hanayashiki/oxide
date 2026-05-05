// ast.ox — AST node types for the stage-1 parser.
//
// Lists owned per-node (`Vec<T>` field on the node) rather than as
// `(off, len)` slices into shared flat arrays — the shared-array
// approach silently interleaves when a child node (e.g. a nested
// block) pushes to the same array as its parent.
//
// Tagged-struct nodes carry the union of all payload fields; per-
// variant fields are commented at use sites. Per-node memory is
// generous (~200B), but a typical compiler input has O(K) nodes —
// trivial in practice.

import "stdlib.ox";
import "intrinsics.ox";
import "./util/vec.ox";
import "./util/strbuf.ox";

// ---------------------------------------------------------------- //
// Sentinels                                                        //
// ---------------------------------------------------------------- //

fn ID_NONE() -> usize { 18446744073709551615 }     // u64::MAX

// ---------------------------------------------------------------- //
// Item kind tags                                                   //
// ---------------------------------------------------------------- //

fn ITEM_FN()           -> u8 { 0 }
fn ITEM_STRUCT()       -> u8 { 1 }
fn ITEM_EXTERN_BLOCK() -> u8 { 2 }
fn ITEM_IMPORT()       -> u8 { 3 }

// ---------------------------------------------------------------- //
// Expr kind tags                                                   //
// ---------------------------------------------------------------- //

fn EX_INT_LIT()    -> u8 { 0 }
fn EX_BOOL_LIT()   -> u8 { 1 }
fn EX_CHAR_LIT()   -> u8 { 2 }
fn EX_STR_LIT()    -> u8 { 3 }
fn EX_NULL()       -> u8 { 4 }
fn EX_IDENT()      -> u8 { 5 }
fn EX_PAREN()      -> u8 { 6 }
fn EX_UNARY()      -> u8 { 7 }
fn EX_BINARY()     -> u8 { 8 }
fn EX_ASSIGN()     -> u8 { 9 }
fn EX_CALL()       -> u8 { 10 }
fn EX_INDEX()      -> u8 { 11 }
fn EX_FIELD()      -> u8 { 12 }
fn EX_STRUCT_LIT() -> u8 { 13 }
fn EX_ARRAY_LIT()  -> u8 { 14 }    // [a, b, c]
fn EX_ARRAY_RPT()  -> u8 { 15 }    // [init; N]
fn EX_ADDR_OF()    -> u8 { 16 }
fn EX_CAST()       -> u8 { 17 }
fn EX_IF()         -> u8 { 18 }
fn EX_WHILE()      -> u8 { 19 }
fn EX_LOOP()       -> u8 { 20 }
fn EX_FOR()        -> u8 { 21 }
fn EX_BREAK()      -> u8 { 22 }
fn EX_CONTINUE()   -> u8 { 23 }
fn EX_BLOCK()      -> u8 { 24 }
fn EX_RETURN()     -> u8 { 25 }
fn EX_LET()        -> u8 { 26 }
fn EX_POISON()     -> u8 { 27 }

// ---------------------------------------------------------------- //
// Type kind tags                                                   //
// ---------------------------------------------------------------- //

fn TY_NAMED() -> u8 { 0 }
fn TY_PTR()   -> u8 { 1 }
fn TY_ARRAY() -> u8 { 2 }

// ---------------------------------------------------------------- //
// Operator codes                                                   //
// ---------------------------------------------------------------- //

fn UN_NEG()    -> u8 { 0 }
fn UN_NOT()    -> u8 { 1 }
fn UN_BITNOT() -> u8 { 2 }
fn UN_DEREF()  -> u8 { 3 }

fn BIN_ADD()    -> u8 {  0 }
fn BIN_SUB()    -> u8 {  1 }
fn BIN_MUL()    -> u8 {  2 }
fn BIN_DIV()    -> u8 {  3 }
fn BIN_REM()    -> u8 {  4 }
fn BIN_EQ()     -> u8 {  5 }
fn BIN_NE()     -> u8 {  6 }
fn BIN_LT()     -> u8 {  7 }
fn BIN_LE()     -> u8 {  8 }
fn BIN_GT()     -> u8 {  9 }
fn BIN_GE()     -> u8 { 10 }
fn BIN_AND()    -> u8 { 11 }
fn BIN_OR()     -> u8 { 12 }
fn BIN_BITAND() -> u8 { 13 }
fn BIN_BITOR()  -> u8 { 14 }
fn BIN_BITXOR() -> u8 { 15 }
fn BIN_SHL()    -> u8 { 16 }
fn BIN_SHR()    -> u8 { 17 }

fn AS_EQ()     -> u8 {  0 }
fn AS_ADD()    -> u8 {  1 }
fn AS_SUB()    -> u8 {  2 }
fn AS_MUL()    -> u8 {  3 }
fn AS_DIV()    -> u8 {  4 }
fn AS_REM()    -> u8 {  5 }
fn AS_BITAND() -> u8 {  6 }
fn AS_BITOR()  -> u8 {  7 }
fn AS_BITXOR() -> u8 {  8 }
fn AS_SHL()    -> u8 {  9 }
fn AS_SHR()    -> u8 { 10 }

fn MUT_CONST() -> u8 { 0 }
fn MUT_MUT()   -> u8 { 1 }

// ---------------------------------------------------------------- //
// ParseError tags                                                  //
// ---------------------------------------------------------------- //

fn PE_UNEXPECTED_TOKEN()   -> u8 { 0 }
fn PE_UNEXPECTED_EOF()     -> u8 { 1 }
fn PE_LEX_ERROR()          -> u8 { 2 }
fn PE_DUPLICATE_VARIADIC() -> u8 { 3 }
fn PE_INT_LIT_REQUIRED()   -> u8 { 4 }

// ---------------------------------------------------------------- //
// Common payload structs                                           //
// ---------------------------------------------------------------- //

struct Ident {
    name_off:   usize,
    name_len:   usize,
    span_start: usize,
    span_end:   usize,
}

fn ident_zero() -> Ident {
    Ident { name_off: 0, name_len: 0, span_start: 0, span_end: 0 }
}

struct Param {
    mutable:    bool,
    name:       Ident,
    ty:         usize,
    span_start: usize,
    span_end:   usize,
}

struct FieldDecl {
    name:       Ident,
    ty:         usize,
    span_start: usize,
    span_end:   usize,
}

struct BlockItem {
    expr:     usize,
    has_semi: bool,
}

struct StructLitField {
    name:       Ident,
    value:      usize,
    span_start: usize,
    span_end:   usize,
}

// ---------------------------------------------------------------- //
// Item                                                             //
// ---------------------------------------------------------------- //

struct Item {
    kind: u8,

    // Fn / Struct / Import / ExternBlock-abi: name (Import reuses for path,
    //   ExternBlock reuses for "C" abi string)
    name: Ident,

    // Fn / Struct: generic_params
    generic_params: Vec<Ident>,

    // Fn: params, variadic, ret_ty, body
    params:       Vec<Param>,
    is_variadic:  bool,
    ret_ty:       usize,       // TypeId or ID_NONE
    body:         usize,       // BlockId or ID_NONE

    // Struct: fields
    fields: Vec<FieldDecl>,

    // ExternBlock: child item ids
    extern_items: Vec<usize>,

    span_start: usize,
    span_end:   usize,
}

fn item_zero() -> Item {
    Item {
        kind: 0,
        name: ident_zero(),
        generic_params: vec_new::<Ident>(),
        params:         vec_new::<Param>(),
        is_variadic:    false,
        ret_ty:         ID_NONE(),
        body:           ID_NONE(),
        fields:         vec_new::<FieldDecl>(),
        extern_items:   vec_new::<usize>(),
        span_start: 0,
        span_end:   0,
    }
}

// ---------------------------------------------------------------- //
// Expr                                                             //
// ---------------------------------------------------------------- //

struct Expr {
    kind: u8,

    // Primitive payloads
    int_val:  u64,
    bool_val: bool,
    char_val: u32,

    // Interned name / string payload
    name: Ident,        // Ident, Field name, StructLit name, Let name,
                        // StrLit (using only name_off/name_len)

    // Generic child slots (ExprId or BlockId; meaning per-variant)
    e1: usize,
    e2: usize,
    e3: usize,
    e4: usize,
    e4_is_block: bool,  // disambiguates e4 between Block/Expr (for If.else)

    // Type child (cast.ty, let.ty)
    t1: usize,

    // List payloads
    args:      Vec<usize>,             // Call args, ArrayLit elems
    type_args: Vec<usize>,             // turbofish args
    sl_fields: Vec<StructLitField>,    // StructLit fields

    // Operator code (UnOp / BinOp / AssignOp / AddrOf mutability)
    op: u8,

    mutable: bool,

    span_start: usize,
    span_end:   usize,
}

fn expr_zero() -> Expr {
    Expr {
        kind: 0,
        int_val: 0, bool_val: false, char_val: 0,
        name: ident_zero(),
        e1: ID_NONE(), e2: ID_NONE(), e3: ID_NONE(), e4: ID_NONE(),
        e4_is_block: false,
        t1: ID_NONE(),
        args:      vec_new::<usize>(),
        type_args: vec_new::<usize>(),
        sl_fields: vec_new::<StructLitField>(),
        op: 0,
        mutable: false,
        span_start: 0,
        span_end:   0,
    }
}

// ---------------------------------------------------------------- //
// Block                                                            //
// ---------------------------------------------------------------- //

struct Block {
    items:      Vec<BlockItem>,
    span_start: usize,
    span_end:   usize,
}

fn block_zero() -> Block {
    Block { items: vec_new::<BlockItem>(), span_start: 0, span_end: 0 }
}

// ---------------------------------------------------------------- //
// Type                                                             //
// ---------------------------------------------------------------- //

struct Type {
    kind: u8,

    // TY_NAMED: name + type_args
    name:      Ident,
    type_args: Vec<usize>,

    // TY_PTR: mutability + pointee
    mutability: u8,
    pointee:    usize,

    // TY_ARRAY: elem + len_expr
    elem:     usize,
    len_expr: usize,

    span_start: usize,
    span_end:   usize,
}

fn type_zero() -> Type {
    Type {
        kind: 0,
        name: ident_zero(),
        type_args: vec_new::<usize>(),
        mutability: 0, pointee: ID_NONE(),
        elem: ID_NONE(), len_expr: ID_NONE(),
        span_start: 0, span_end: 0,
    }
}

// ---------------------------------------------------------------- //
// ParseError                                                       //
// ---------------------------------------------------------------- //

struct ParseError {
    kind:        u8,
    span_start:  usize,
    span_end:    usize,
    found_kind:  u8,
    extra_off:   usize,
    extra_len:   usize,
}

fn parse_error_zero() -> ParseError {
    ParseError {
        kind: 0, span_start: 0, span_end: 0,
        found_kind: 0, extra_off: 0, extra_len: 0,
    }
}

// ---------------------------------------------------------------- //
// Module — root AST container                                      //
// ---------------------------------------------------------------- //

struct Module {
    items:  Vec<Item>,
    exprs:  Vec<Expr>,
    blocks: Vec<Block>,
    types:  Vec<Type>,
    pool:   StrBuf,

    root_items: Vec<usize>,
    errors:     Vec<ParseError>,

    span_start: usize,
    span_end:   usize,
}

fn module_new(pool: StrBuf) -> Module {
    Module {
        items:  vec_new::<Item>(),
        exprs:  vec_new::<Expr>(),
        blocks: vec_new::<Block>(),
        types:  vec_new::<Type>(),
        pool:   pool,
        root_items: vec_new::<usize>(),
        errors:     vec_new::<ParseError>(),
        span_start: 0,
        span_end:   0,
    }
}
