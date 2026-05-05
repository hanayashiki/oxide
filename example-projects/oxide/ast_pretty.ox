// ast_pretty.ox — debug dump of a parsed Module.

import "stdio.ox";
import "stdlib.ox";
import "string.ox";
import "intrinsics.ox";
import "./util/vec.ox";
import "./util/strbuf.ox";
import "./ast.ox";

fn pretty_module(m: *const Module) -> StrBuf {
    let mut out: StrBuf = strbuf_with_capacity(2048);
    let n: usize = vec_len::<usize>(&m.root_items);
    let mut i: usize = 0;
    while i < n {
        let item_id: usize = vec_get::<usize>(&m.root_items, i);
        pretty_item(m, &mut out, item_id, 0);
        i = i + 1;
    }
    let en: usize = vec_len::<ParseError>(&m.errors);
    let mut k: usize = 0;
    while k < en {
        let e: ParseError = vec_get::<ParseError>(&m.errors, k);
        indent(&mut out, 0);
        strbuf_push_str(&mut out, "(error kind=", 12);
        strbuf_push_u64(&mut out, e.kind as u64);
        strbuf_push_str(&mut out, " span=", 6);
        strbuf_push_u64(&mut out, e.span_start as u64);
        strbuf_push_byte(&mut out, 46);
        strbuf_push_byte(&mut out, 46);
        strbuf_push_u64(&mut out, e.span_end as u64);
        strbuf_push_byte(&mut out, 41);
        strbuf_push_byte(&mut out, 10);
        k = k + 1;
    }
    out
}

fn indent(out: *mut StrBuf, depth: u32) {
    let mut i: u32 = 0;
    while i < depth {
        strbuf_push_byte(out, 32);
        strbuf_push_byte(out, 32);
        i = i + 1;
    }
}

fn push_pool_slice(out: *mut StrBuf, m: *const Module, off: usize, len: usize) {
    let pool_ptr: *const [u8] = strbuf_as_ptr(&m.pool);
    let mut i: usize = 0;
    while i < len {
        strbuf_push_byte(out, pool_ptr[off + i]);
        i = i + 1;
    }
}

fn push_ident(out: *mut StrBuf, m: *const Module, id: Ident) {
    push_pool_slice(out, m, id.name_off, id.name_len);
}

fn pretty_item(m: *const Module, out: *mut StrBuf, item_id: usize, depth: u32) {
    let it: Item = vec_get::<Item>(&m.items, item_id);
    indent(out, depth);
    if it.kind == ITEM_FN() {
        strbuf_push_str(out, "(fn ", 4);
        push_ident(out, m, it.name);
        let gpn: usize = vec_len::<Ident>(&it.generic_params);
        if gpn > 0 {
            strbuf_push_str(out, "<", 1);
            let mut i: usize = 0;
            while i < gpn {
                if i > 0 { strbuf_push_str(out, ",", 1); }
                let g: Ident = vec_get::<Ident>(&it.generic_params, i);
                push_ident(out, m, g);
                i = i + 1;
            }
            strbuf_push_str(out, ">", 1);
        }
        strbuf_push_byte(out, 32);
        strbuf_push_byte(out, 40);
        let pn: usize = vec_len::<Param>(&it.params);
        let mut i: usize = 0;
        while i < pn {
            if i > 0 { strbuf_push_str(out, ", ", 2); }
            let pr: Param = vec_get::<Param>(&it.params, i);
            if pr.mutable { strbuf_push_str(out, "mut ", 4); }
            push_ident(out, m, pr.name);
            strbuf_push_str(out, ": ", 2);
            pretty_type(m, out, pr.ty);
            i = i + 1;
        }
        if it.is_variadic {
            if pn > 0 { strbuf_push_str(out, ", ", 2); }
            strbuf_push_str(out, "...", 3);
        }
        strbuf_push_byte(out, 41);
        if it.ret_ty != ID_NONE() {
            strbuf_push_str(out, " -> ", 4);
            pretty_type(m, out, it.ret_ty);
        }
        if it.body == ID_NONE() {
            strbuf_push_str(out, " (no-body)", 10);
        } else {
            strbuf_push_byte(out, 10);
            pretty_block(m, out, it.body, depth + 1);
        }
        strbuf_push_byte(out, 41);
        strbuf_push_byte(out, 10);
        return;
    }
    if it.kind == ITEM_STRUCT() {
        strbuf_push_str(out, "(struct ", 8);
        push_ident(out, m, it.name);
        let gpn: usize = vec_len::<Ident>(&it.generic_params);
        if gpn > 0 {
            strbuf_push_str(out, "<", 1);
            let mut i: usize = 0;
            while i < gpn {
                if i > 0 { strbuf_push_str(out, ",", 1); }
                let g: Ident = vec_get::<Ident>(&it.generic_params, i);
                push_ident(out, m, g);
                i = i + 1;
            }
            strbuf_push_str(out, ">", 1);
        }
        strbuf_push_byte(out, 10);
        let fn_count: usize = vec_len::<FieldDecl>(&it.fields);
        let mut i: usize = 0;
        while i < fn_count {
            indent(out, depth + 1);
            let fd: FieldDecl = vec_get::<FieldDecl>(&it.fields, i);
            push_ident(out, m, fd.name);
            strbuf_push_str(out, ": ", 2);
            pretty_type(m, out, fd.ty);
            strbuf_push_byte(out, 10);
            i = i + 1;
        }
        indent(out, depth);
        strbuf_push_byte(out, 41);
        strbuf_push_byte(out, 10);
        return;
    }
    if it.kind == ITEM_EXTERN_BLOCK() {
        strbuf_push_str(out, "(extern \"", 9);
        push_pool_slice(out, m, it.name.name_off, it.name.name_len);
        strbuf_push_str(out, "\"", 1);
        strbuf_push_byte(out, 10);
        let cn: usize = vec_len::<usize>(&it.extern_items);
        let mut i: usize = 0;
        while i < cn {
            let child_id: usize = vec_get::<usize>(&it.extern_items, i);
            pretty_item(m, out, child_id, depth + 1);
            i = i + 1;
        }
        indent(out, depth);
        strbuf_push_byte(out, 41);
        strbuf_push_byte(out, 10);
        return;
    }
    if it.kind == ITEM_IMPORT() {
        strbuf_push_str(out, "(import \"", 9);
        push_pool_slice(out, m, it.name.name_off, it.name.name_len);
        strbuf_push_str(out, "\")\n", 3);
        return;
    }
}

fn pretty_type(m: *const Module, out: *mut StrBuf, type_id: usize) {
    let t: Type = vec_get::<Type>(&m.types, type_id);
    if t.kind == TY_NAMED() {
        push_ident(out, m, t.name);
        let tan: usize = vec_len::<usize>(&t.type_args);
        if tan > 0 {
            strbuf_push_str(out, "<", 1);
            let mut i: usize = 0;
            while i < tan {
                if i > 0 { strbuf_push_str(out, ", ", 2); }
                let a: usize = vec_get::<usize>(&t.type_args, i);
                pretty_type(m, out, a);
                i = i + 1;
            }
            strbuf_push_str(out, ">", 1);
        }
        return;
    }
    if t.kind == TY_PTR() {
        if t.mutability == MUT_MUT() {
            strbuf_push_str(out, "*mut ", 5);
        } else {
            strbuf_push_str(out, "*const ", 7);
        }
        pretty_type(m, out, t.pointee);
        return;
    }
    if t.kind == TY_ARRAY() {
        strbuf_push_byte(out, 91);
        pretty_type(m, out, t.elem);
        if t.len_expr != ID_NONE() {
            strbuf_push_str(out, "; ", 2);
            let le: Expr = vec_get::<Expr>(&m.exprs, t.len_expr);
            strbuf_push_u64(out, le.int_val);
        }
        strbuf_push_byte(out, 93);
        return;
    }
}

fn pretty_block(m: *const Module, out: *mut StrBuf, block_id: usize, depth: u32) {
    let b: Block = vec_get::<Block>(&m.blocks, block_id);
    indent(out, depth);
    strbuf_push_str(out, "(block", 6);
    let bn: usize = vec_len::<BlockItem>(&b.items);
    if bn == 0 {
        strbuf_push_byte(out, 41);
        return;
    }
    strbuf_push_byte(out, 10);
    let mut i: usize = 0;
    while i < bn {
        let bi: BlockItem = vec_get::<BlockItem>(&b.items, i);
        pretty_expr(m, out, bi.expr, depth + 1);
        if bi.has_semi {
            strbuf_push_byte(out, 32);
            strbuf_push_byte(out, 59);
        }
        strbuf_push_byte(out, 10);
        i = i + 1;
    }
    indent(out, depth);
    strbuf_push_byte(out, 41);
}

fn pretty_expr(m: *const Module, out: *mut StrBuf, expr_id: usize, depth: u32) {
    let e: Expr = vec_get::<Expr>(&m.exprs, expr_id);
    indent(out, depth);
    let k: u8 = e.kind;

    if k == EX_INT_LIT() {
        strbuf_push_str(out, "(int ", 5);
        strbuf_push_u64(out, e.int_val);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_BOOL_LIT() {
        let lt: *const [u8] = "(bool true)";
        let lf: *const [u8] = "(bool false)";
        if e.bool_val {
            strbuf_push_str(out, lt, 11);
        } else {
            strbuf_push_str(out, lf, 12);
        }
        return;
    }
    if k == EX_CHAR_LIT() {
        strbuf_push_str(out, "(char ", 6);
        strbuf_push_u64(out, e.char_val as u64);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_STR_LIT() {
        strbuf_push_str(out, "(str \"", 6);
        push_pool_slice(out, m, e.name.name_off, e.name.name_len);
        strbuf_push_str(out, "\")", 2);
        return;
    }
    if k == EX_NULL() {
        strbuf_push_str(out, "(null)", 6);
        return;
    }
    if k == EX_IDENT() {
        strbuf_push_str(out, "(id ", 4);
        push_ident(out, m, e.name);
        let tan: usize = vec_len::<usize>(&e.type_args);
        if tan > 0 {
            strbuf_push_str(out, "::<", 3);
            let mut i: usize = 0;
            while i < tan {
                if i > 0 { strbuf_push_str(out, ",", 1); }
                let a: usize = vec_get::<usize>(&e.type_args, i);
                pretty_type(m, out, a);
                i = i + 1;
            }
            strbuf_push_str(out, ">", 1);
        }
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_PAREN() {
        strbuf_push_str(out, "(paren\n", 7);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_UNARY() {
        strbuf_push_str(out, "(unary ", 7);
        push_unop(out, e.op);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_BINARY() {
        strbuf_push_str(out, "(binary ", 8);
        push_binop(out, e.op);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e2, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_ASSIGN() {
        strbuf_push_str(out, "(assign ", 8);
        push_assign(out, e.op);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e2, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_CALL() {
        strbuf_push_str(out, "(call", 5);
        let tan: usize = vec_len::<usize>(&e.type_args);
        if tan > 0 {
            strbuf_push_str(out, "::<", 3);
            let mut i: usize = 0;
            while i < tan {
                if i > 0 { strbuf_push_str(out, ",", 1); }
                let a: usize = vec_get::<usize>(&e.type_args, i);
                pretty_type(m, out, a);
                i = i + 1;
            }
            strbuf_push_str(out, ">", 1);
        }
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e1, depth + 1);
        let an: usize = vec_len::<usize>(&e.args);
        let mut i: usize = 0;
        while i < an {
            strbuf_push_byte(out, 10);
            let a: usize = vec_get::<usize>(&e.args, i);
            pretty_expr(m, out, a, depth + 1);
            i = i + 1;
        }
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_INDEX() {
        strbuf_push_str(out, "(index\n", 7);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e2, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_FIELD() {
        strbuf_push_str(out, "(field .", 8);
        push_ident(out, m, e.name);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_STRUCT_LIT() {
        strbuf_push_str(out, "(struct-lit ", 12);
        push_ident(out, m, e.name);
        let tan: usize = vec_len::<usize>(&e.type_args);
        if tan > 0 {
            strbuf_push_str(out, "::<", 3);
            let mut i: usize = 0;
            while i < tan {
                if i > 0 { strbuf_push_str(out, ",", 1); }
                let a: usize = vec_get::<usize>(&e.type_args, i);
                pretty_type(m, out, a);
                i = i + 1;
            }
            strbuf_push_str(out, ">", 1);
        }
        strbuf_push_byte(out, 10);
        let sln: usize = vec_len::<StructLitField>(&e.sl_fields);
        let mut i: usize = 0;
        while i < sln {
            indent(out, depth + 1);
            let f: StructLitField = vec_get::<StructLitField>(&e.sl_fields, i);
            strbuf_push_str(out, "(.", 2);
            push_ident(out, m, f.name);
            strbuf_push_byte(out, 10);
            pretty_expr(m, out, f.value, depth + 2);
            strbuf_push_byte(out, 41);
            strbuf_push_byte(out, 10);
            i = i + 1;
        }
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_ARRAY_LIT() {
        strbuf_push_str(out, "(array-lit", 10);
        let an: usize = vec_len::<usize>(&e.args);
        let mut i: usize = 0;
        while i < an {
            strbuf_push_byte(out, 10);
            let a: usize = vec_get::<usize>(&e.args, i);
            pretty_expr(m, out, a, depth + 1);
            i = i + 1;
        }
        if an > 0 { strbuf_push_byte(out, 10); indent(out, depth); }
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_ARRAY_RPT() {
        strbuf_push_str(out, "(array-rpt\n", 11);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e2, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_ADDR_OF() {
        strbuf_push_str(out, "(addr-of ", 9);
        if e.mutable { strbuf_push_str(out, "mut", 3); } else { strbuf_push_str(out, "const", 5); }
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_CAST() {
        strbuf_push_str(out, "(cast ", 6);
        pretty_type(m, out, e.t1);
        strbuf_push_byte(out, 10);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_IF() {
        strbuf_push_str(out, "(if\n", 4);
        indent(out, depth + 1);
        strbuf_push_str(out, "(cond\n", 6);
        pretty_expr(m, out, e.e1, depth + 2);
        strbuf_push_byte(out, 10);
        indent(out, depth + 1);
        strbuf_push_byte(out, 41);
        strbuf_push_byte(out, 10);
        pretty_block(m, out, e.e3, depth + 1);
        if e.e4 != ID_NONE() {
            strbuf_push_byte(out, 10);
            indent(out, depth + 1);
            strbuf_push_str(out, "(else\n", 6);
            if e.e4_is_block {
                pretty_block(m, out, e.e4, depth + 2);
            } else {
                pretty_expr(m, out, e.e4, depth + 2);
            }
            strbuf_push_byte(out, 10);
            indent(out, depth + 1);
            strbuf_push_byte(out, 41);
        }
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_WHILE() {
        strbuf_push_str(out, "(while\n", 7);
        pretty_expr(m, out, e.e1, depth + 1);
        strbuf_push_byte(out, 10);
        pretty_block(m, out, e.e3, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_LOOP() {
        strbuf_push_str(out, "(loop\n", 6);
        pretty_block(m, out, e.e3, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_FOR() {
        strbuf_push_str(out, "(for", 4);
        strbuf_push_byte(out, 10);
        indent(out, depth + 1);
        strbuf_push_str(out, "(init", 5);
        if e.e1 != ID_NONE() {
            strbuf_push_byte(out, 10);
            pretty_expr(m, out, e.e1, depth + 2);
            strbuf_push_byte(out, 10);
            indent(out, depth + 1);
        }
        strbuf_push_byte(out, 41);
        strbuf_push_byte(out, 10);
        indent(out, depth + 1);
        strbuf_push_str(out, "(cond", 5);
        if e.e2 != ID_NONE() {
            strbuf_push_byte(out, 10);
            pretty_expr(m, out, e.e2, depth + 2);
            strbuf_push_byte(out, 10);
            indent(out, depth + 1);
        }
        strbuf_push_byte(out, 41);
        strbuf_push_byte(out, 10);
        indent(out, depth + 1);
        strbuf_push_str(out, "(update", 7);
        if e.t1 != ID_NONE() {
            strbuf_push_byte(out, 10);
            pretty_expr(m, out, e.t1, depth + 2);
            strbuf_push_byte(out, 10);
            indent(out, depth + 1);
        }
        strbuf_push_byte(out, 41);
        strbuf_push_byte(out, 10);
        pretty_block(m, out, e.e3, depth + 1);
        strbuf_push_byte(out, 10);
        indent(out, depth);
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_BREAK() {
        if e.e1 == ID_NONE() {
            strbuf_push_str(out, "(break)", 7);
        } else {
            strbuf_push_str(out, "(break\n", 7);
            pretty_expr(m, out, e.e1, depth + 1);
            strbuf_push_byte(out, 10);
            indent(out, depth);
            strbuf_push_byte(out, 41);
        }
        return;
    }
    if k == EX_CONTINUE() {
        strbuf_push_str(out, "(continue)", 10);
        return;
    }
    if k == EX_RETURN() {
        if e.e1 == ID_NONE() {
            strbuf_push_str(out, "(return)", 8);
        } else {
            strbuf_push_str(out, "(return\n", 8);
            pretty_expr(m, out, e.e1, depth + 1);
            strbuf_push_byte(out, 10);
            indent(out, depth);
            strbuf_push_byte(out, 41);
        }
        return;
    }
    if k == EX_LET() {
        strbuf_push_str(out, "(let", 4);
        if e.mutable { strbuf_push_str(out, " mut", 4); }
        strbuf_push_byte(out, 32);
        push_ident(out, m, e.name);
        if e.t1 != ID_NONE() {
            strbuf_push_str(out, ": ", 2);
            pretty_type(m, out, e.t1);
        }
        if e.e1 != ID_NONE() {
            strbuf_push_byte(out, 10);
            pretty_expr(m, out, e.e1, depth + 1);
            strbuf_push_byte(out, 10);
            indent(out, depth);
        }
        strbuf_push_byte(out, 41);
        return;
    }
    if k == EX_BLOCK() {
        pretty_block(m, out, e.e3, depth);
        return;
    }
    if k == EX_POISON() {
        strbuf_push_str(out, "(poison)", 8);
        return;
    }
    strbuf_push_str(out, "(?unhandled-expr-kind ", 22);
    strbuf_push_u64(out, e.kind as u64);
    strbuf_push_byte(out, 41);
}

fn push_unop(out: *mut StrBuf, op: u8) {
    if op == UN_NEG()    { strbuf_push_str(out, "neg",    3); return; }
    if op == UN_NOT()    { strbuf_push_str(out, "not",    3); return; }
    if op == UN_BITNOT() { strbuf_push_str(out, "bitnot", 6); return; }
    if op == UN_DEREF()  { strbuf_push_str(out, "deref",  5); return; }
    strbuf_push_str(out, "?unop", 5);
}

fn push_binop(out: *mut StrBuf, op: u8) {
    if op == BIN_ADD()    { strbuf_push_str(out, "+",   1); return; }
    if op == BIN_SUB()    { strbuf_push_str(out, "-",   1); return; }
    if op == BIN_MUL()    { strbuf_push_str(out, "*",   1); return; }
    if op == BIN_DIV()    { strbuf_push_str(out, "/",   1); return; }
    if op == BIN_REM()    { strbuf_push_str(out, "%",   1); return; }
    if op == BIN_EQ()     { strbuf_push_str(out, "==",  2); return; }
    if op == BIN_NE()     { strbuf_push_str(out, "!=",  2); return; }
    if op == BIN_LT()     { strbuf_push_str(out, "<",   1); return; }
    if op == BIN_LE()     { strbuf_push_str(out, "<=",  2); return; }
    if op == BIN_GT()     { strbuf_push_str(out, ">",   1); return; }
    if op == BIN_GE()     { strbuf_push_str(out, ">=",  2); return; }
    if op == BIN_AND()    { strbuf_push_str(out, "&&",  2); return; }
    if op == BIN_OR()     { strbuf_push_str(out, "||",  2); return; }
    if op == BIN_BITAND() { strbuf_push_str(out, "&",   1); return; }
    if op == BIN_BITOR()  { strbuf_push_str(out, "|",   1); return; }
    if op == BIN_BITXOR() { strbuf_push_str(out, "^",   1); return; }
    if op == BIN_SHL()    { strbuf_push_str(out, "<<",  2); return; }
    if op == BIN_SHR()    { strbuf_push_str(out, ">>",  2); return; }
    strbuf_push_str(out, "?binop", 6);
}

fn push_assign(out: *mut StrBuf, op: u8) {
    if op == AS_EQ()     { strbuf_push_str(out, "=",   1); return; }
    if op == AS_ADD()    { strbuf_push_str(out, "+=",  2); return; }
    if op == AS_SUB()    { strbuf_push_str(out, "-=",  2); return; }
    if op == AS_MUL()    { strbuf_push_str(out, "*=",  2); return; }
    if op == AS_DIV()    { strbuf_push_str(out, "/=",  2); return; }
    if op == AS_REM()    { strbuf_push_str(out, "%=",  2); return; }
    if op == AS_BITAND() { strbuf_push_str(out, "&=",  2); return; }
    if op == AS_BITOR()  { strbuf_push_str(out, "|=",  2); return; }
    if op == AS_BITXOR() { strbuf_push_str(out, "^=",  2); return; }
    if op == AS_SHL()    { strbuf_push_str(out, "<<=", 3); return; }
    if op == AS_SHR()    { strbuf_push_str(out, ">>=", 3); return; }
    strbuf_push_str(out, "?assignop", 9);
}
