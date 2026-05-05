// parser.ox — recursive-descent parser for stage-1 Oxide.
//
// Token stream → AST. Errors accumulate in `module.errors`; we
// poison the current expression on failure and try to sync to the
// next `;` / `}` / `)` to keep diagnostics flowing.
//
// Per-node Vec lists; no shared flat side-arrays (those silently
// interleaved on nested constructs).

import "stdlib.ox";
import "stdio.ox";
import "string.ox";
import "intrinsics.ox";
import "./util/vec.ox";
import "./util/strbuf.ox";
import "./lexer.ox";
import "./ast.ox";

struct Parser {
    tokens: Vec<Token>,
    pos:    usize,
    module: Module,
}

fn parse_program(lx: Lexer) -> Module {
    let mut p: Parser = Parser {
        tokens: lx.tokens,
        pos:    0,
        module: module_new(lx.pool),
    };
    parse_run(&mut p);
    p.module
}

// ---------------------------------------------------------------- //
// Token stream helpers                                             //
// ---------------------------------------------------------------- //

fn p_at_eof(p: *const Parser) -> bool {
    let n: usize = vec_len::<Token>(&p.tokens);
    if p.pos >= n { return true; }
    let t: Token = vec_get::<Token>(&p.tokens, p.pos);
    t.kind == TK_EOF()
}

fn p_peek(p: *const Parser) -> Token {
    vec_get::<Token>(&p.tokens, p.pos)
}

fn p_peek_kind(p: *const Parser) -> u8 {
    let t: Token = vec_get::<Token>(&p.tokens, p.pos);
    t.kind
}

fn p_peek_kind_n(p: *const Parser, off: usize) -> u8 {
    let n: usize = vec_len::<Token>(&p.tokens);
    if p.pos + off >= n { return TK_EOF(); }
    let t: Token = vec_get::<Token>(&p.tokens, p.pos + off);
    t.kind
}

fn p_bump(p: *mut Parser) -> Token {
    let t: Token = vec_get::<Token>(&p.tokens, p.pos);
    p.pos = p.pos + 1;
    t
}

fn p_eat(p: *mut Parser, kind: u8) -> bool {
    if p_peek_kind(p) == kind {
        p_bump(p);
        return true;
    }
    return false;
}

fn p_expect(p: *mut Parser, kind: u8) -> Token {
    let t: Token = p_peek(p);
    if t.kind == kind {
        p.pos = p.pos + 1;
        return t;
    }
    err_unexpected(p, t.span_start, t.span_end, t.kind);
    return t;
}

// ---------------------------------------------------------------- //
// Error reporting                                                  //
// ---------------------------------------------------------------- //

fn err_unexpected(p: *mut Parser, span_start: usize, span_end: usize, found: u8) {
    let mut e: ParseError = parse_error_zero();
    e.kind       = PE_UNEXPECTED_TOKEN();
    e.span_start = span_start;
    e.span_end   = span_end;
    e.found_kind = found;
    vec_push::<ParseError>(&mut p.module.errors, e);
}

fn err_at_token(p: *mut Parser, kind: u8) {
    let t: Token = p_peek(p);
    let mut e: ParseError = parse_error_zero();
    e.kind       = kind;
    e.span_start = t.span_start;
    e.span_end   = t.span_end;
    e.found_kind = t.kind;
    vec_push::<ParseError>(&mut p.module.errors, e);
}

fn sync_to_semi_or_brace(p: *mut Parser) {
    loop {
        if p_at_eof(p) { return; }
        let k: u8 = p_peek_kind(p);
        if k == TK_SEMI() {
            p_bump(p);
            return;
        }
        if k == TK_RBRACE() { return; }
        p_bump(p);
    }
}

// ---------------------------------------------------------------- //
// Top-level                                                        //
// ---------------------------------------------------------------- //

fn parse_run(p: *mut Parser) {
    let module_start: usize = if vec_len::<Token>(&p.tokens) > 0 {
        let t0: Token = vec_get::<Token>(&p.tokens, 0);
        t0.span_start
    } else {
        0
    };
    p.module.span_start = module_start;

    while !p_at_eof(p) {
        if p_peek_kind(p) == TK_ERROR() {
            err_at_token(p, PE_LEX_ERROR());
            p_bump(p);
            continue;
        }
        let item_id: usize = parse_item(p);
        if item_id == ID_NONE() {
            sync_to_semi_or_brace(p);
            continue;
        }
        vec_push::<usize>(&mut p.module.root_items, item_id);
    }

    let last_pos: usize = if p.pos > 0 { p.pos - 1 } else { 0 };
    if vec_len::<Token>(&p.tokens) > 0 {
        let t: Token = vec_get::<Token>(&p.tokens, last_pos);
        p.module.span_end = t.span_end;
    }
}

// ---------------------------------------------------------------- //
// Items                                                            //
// ---------------------------------------------------------------- //

fn parse_item(p: *mut Parser) -> usize {
    let k: u8 = p_peek_kind(p);
    if k == TK_KW_FN()     { return parse_fn_item(p, false); }
    if k == TK_KW_STRUCT() { return parse_struct_item(p); }
    if k == TK_KW_EXTERN() { return parse_extern_block(p); }
    if k == TK_KW_IMPORT() { return parse_import_item(p); }

    let t: Token = p_peek(p);
    err_unexpected(p, t.span_start, t.span_end, t.kind);
    return ID_NONE();
}

fn parse_fn_item(p: *mut Parser, extern_decl: bool) -> usize {
    let kw: Token = p_expect(p, TK_KW_FN());
    let span_start: usize = kw.span_start;

    let mut item: Item = item_zero();
    item.kind = ITEM_FN();
    item.name = parse_ident(p);

    // Generic params
    if p_peek_kind(p) == TK_LT() {
        p_bump(p);
        if p_peek_kind(p) != TK_GT() && p_peek_kind(p) != TK_JOINT_GT() {
            loop {
                let g: Ident = parse_ident(p);
                vec_push::<Ident>(&mut item.generic_params, g);
                if !p_eat(p, TK_COMMA()) { break; }
            }
        }
        if !(p_eat(p, TK_GT()) || p_eat(p, TK_JOINT_GT())) {
            err_at_token(p, PE_UNEXPECTED_TOKEN());
        }
    }

    // Params
    p_expect(p, TK_LPAREN());
    let mut variadic: bool = false;
    if p_peek_kind(p) != TK_RPAREN() {
        loop {
            if p_peek_kind(p) == TK_DOTDOTDOT() {
                if variadic {
                    err_at_token(p, PE_DUPLICATE_VARIADIC());
                }
                p_bump(p);
                variadic = true;
                break;
            }
            let prm: Param = parse_param(p);
            vec_push::<Param>(&mut item.params, prm);
            if !p_eat(p, TK_COMMA()) { break; }
        }
    }
    p_expect(p, TK_RPAREN());
    item.is_variadic = variadic;

    if p_eat(p, TK_ARROW()) {
        item.ret_ty = parse_type(p);
    }

    if extern_decl {
        p_expect(p, TK_SEMI());
    } else {
        item.body = parse_block(p);
    }

    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    item.span_start = span_start;
    item.span_end   = last.span_end;

    let id: usize = vec_len::<Item>(&p.module.items);
    vec_push::<Item>(&mut p.module.items, item);
    return id;
}

fn parse_param(p: *mut Parser) -> Param {
    let mutable: bool = p_eat(p, TK_KW_MUT());
    let name: Ident = parse_ident(p);
    p_expect(p, TK_COLON());
    let ty: usize = parse_type(p);
    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    return Param {
        mutable:    mutable,
        name:       name,
        ty:         ty,
        span_start: name.span_start,
        span_end:   last.span_end,
    };
}

fn parse_struct_item(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_STRUCT());
    let mut item: Item = item_zero();
    item.kind = ITEM_STRUCT();
    item.name = parse_ident(p);

    if p_peek_kind(p) == TK_LT() {
        p_bump(p);
        if p_peek_kind(p) != TK_GT() && p_peek_kind(p) != TK_JOINT_GT() {
            loop {
                let g: Ident = parse_ident(p);
                vec_push::<Ident>(&mut item.generic_params, g);
                if !p_eat(p, TK_COMMA()) { break; }
            }
        }
        if !(p_eat(p, TK_GT()) || p_eat(p, TK_JOINT_GT())) {
            err_at_token(p, PE_UNEXPECTED_TOKEN());
        }
    }

    p_expect(p, TK_LBRACE());
    if p_peek_kind(p) != TK_RBRACE() {
        loop {
            let name: Ident = parse_ident(p);
            p_expect(p, TK_COLON());
            let ty: usize = parse_type(p);
            let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
            vec_push::<FieldDecl>(&mut item.fields, FieldDecl {
                name:       name,
                ty:         ty,
                span_start: name.span_start,
                span_end:   last.span_end,
            });
            if !p_eat(p, TK_COMMA()) { break; }
            if p_peek_kind(p) == TK_RBRACE() { break; }
        }
    }
    p_expect(p, TK_RBRACE());

    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    item.span_start = kw.span_start;
    item.span_end   = last.span_end;

    let id: usize = vec_len::<Item>(&p.module.items);
    vec_push::<Item>(&mut p.module.items, item);
    return id;
}

fn parse_extern_block(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_EXTERN());
    let mut item: Item = item_zero();
    item.kind = ITEM_EXTERN_BLOCK();

    let abi_tok: Token = p_expect(p, TK_STR());
    item.name = Ident {
        name_off:   abi_tok.str_off,
        name_len:   abi_tok.str_len,
        span_start: abi_tok.span_start,
        span_end:   abi_tok.span_end,
    };

    p_expect(p, TK_LBRACE());
    while p_peek_kind(p) != TK_RBRACE() && !p_at_eof(p) {
        if p_peek_kind(p) != TK_KW_FN() {
            err_at_token(p, PE_UNEXPECTED_TOKEN());
            sync_to_semi_or_brace(p);
            continue;
        }
        let child: usize = parse_fn_item(p, true);
        if child != ID_NONE() {
            vec_push::<usize>(&mut item.extern_items, child);
        }
    }
    p_expect(p, TK_RBRACE());

    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    item.span_start = kw.span_start;
    item.span_end   = last.span_end;

    let id: usize = vec_len::<Item>(&p.module.items);
    vec_push::<Item>(&mut p.module.items, item);
    return id;
}

fn parse_import_item(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_IMPORT());
    let mut item: Item = item_zero();
    item.kind = ITEM_IMPORT();

    let path_tok: Token = p_expect(p, TK_STR());
    item.name = Ident {
        name_off:   path_tok.str_off,
        name_len:   path_tok.str_len,
        span_start: path_tok.span_start,
        span_end:   path_tok.span_end,
    };
    p_expect(p, TK_SEMI());

    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    item.span_start = kw.span_start;
    item.span_end   = last.span_end;

    let id: usize = vec_len::<Item>(&p.module.items);
    vec_push::<Item>(&mut p.module.items, item);
    return id;
}

// ---------------------------------------------------------------- //
// Idents                                                           //
// ---------------------------------------------------------------- //

fn parse_ident(p: *mut Parser) -> Ident {
    let t: Token = p_peek(p);
    if t.kind != TK_IDENT() {
        err_unexpected(p, t.span_start, t.span_end, t.kind);
        return ident_zero();
    }
    p.pos = p.pos + 1;
    return Ident {
        name_off:   t.str_off,
        name_len:   t.str_len,
        span_start: t.span_start,
        span_end:   t.span_end,
    };
}

// ---------------------------------------------------------------- //
// Types                                                            //
// ---------------------------------------------------------------- //

fn parse_type(p: *mut Parser) -> usize {
    let k: u8 = p_peek_kind(p);

    if k == TK_STAR() {
        let star: Token = p_bump(p);
        let muta: u8 = if p_eat(p, TK_KW_MUT()) {
            MUT_MUT()
        } else if p_eat(p, TK_KW_CONST()) {
            MUT_CONST()
        } else {
            err_at_token(p, PE_UNEXPECTED_TOKEN());
            MUT_CONST()
        };
        let pointee: usize = parse_type(p);
        let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
        let mut t: Type = type_zero();
        t.kind        = TY_PTR();
        t.mutability  = muta;
        t.pointee     = pointee;
        t.span_start  = star.span_start;
        t.span_end    = last.span_end;
        let id: usize = vec_len::<Type>(&p.module.types);
        vec_push::<Type>(&mut p.module.types, t);
        return id;
    }

    if k == TK_LBRACKET() {
        let lb: Token = p_bump(p);
        let elem: usize = parse_type(p);
        let mut len_expr: usize = ID_NONE();
        if p_eat(p, TK_SEMI()) {
            let nt: Token = p_peek(p);
            if nt.kind != TK_INT() {
                err_at_token(p, PE_INT_LIT_REQUIRED());
            }
            let int_tok: Token = if nt.kind == TK_INT() { p_bump(p) } else { nt };
            let mut e: Expr = expr_zero();
            e.kind        = EX_INT_LIT();
            e.int_val     = int_tok.int_val;
            e.span_start  = int_tok.span_start;
            e.span_end    = int_tok.span_end;
            let eid: usize = vec_len::<Expr>(&p.module.exprs);
            vec_push::<Expr>(&mut p.module.exprs, e);
            len_expr = eid;
        }
        p_expect(p, TK_RBRACKET());
        let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
        let mut t: Type = type_zero();
        t.kind       = TY_ARRAY();
        t.elem       = elem;
        t.len_expr   = len_expr;
        t.span_start = lb.span_start;
        t.span_end   = last.span_end;
        let id: usize = vec_len::<Type>(&p.module.types);
        vec_push::<Type>(&mut p.module.types, t);
        return id;
    }

    if k == TK_IDENT() {
        let nm: Ident = parse_ident(p);
        let mut t: Type = type_zero();
        t.kind       = TY_NAMED();
        t.name       = nm;
        t.span_start = nm.span_start;

        if p_peek_kind(p) == TK_LT() {
            p_bump(p);
            if p_peek_kind(p) != TK_GT() && p_peek_kind(p) != TK_JOINT_GT() {
                loop {
                    let arg: usize = parse_type(p);
                    vec_push::<usize>(&mut t.type_args, arg);
                    if !p_eat(p, TK_COMMA()) { break; }
                }
            }
            if !(p_eat(p, TK_GT()) || p_eat(p, TK_JOINT_GT())) {
                err_at_token(p, PE_UNEXPECTED_TOKEN());
            }
        }

        let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
        t.span_end = last.span_end;
        let id: usize = vec_len::<Type>(&p.module.types);
        vec_push::<Type>(&mut p.module.types, t);
        return id;
    }

    err_at_token(p, PE_UNEXPECTED_TOKEN());
    let mut t: Type = type_zero();
    t.kind = TY_NAMED();
    let id: usize = vec_len::<Type>(&p.module.types);
    vec_push::<Type>(&mut p.module.types, t);
    return id;
}

// ---------------------------------------------------------------- //
// Expressions (Pratt parser)                                       //
// ---------------------------------------------------------------- //

fn parse_expr(p: *mut Parser) -> usize {
    parse_expr_with(p, 0, false)
}

fn parse_expr_no_struct_lit(p: *mut Parser) -> usize {
    parse_expr_with(p, 0, true)
}

fn parse_expr_with(p: *mut Parser, min_prec: u32, no_struct_lit: bool) -> usize {
    let lhs: usize = parse_prefix(p, no_struct_lit);
    return parse_infix(p, lhs, min_prec, no_struct_lit);
}

fn parse_prefix(p: *mut Parser, no_struct_lit: bool) -> usize {
    let t: Token = p_peek(p);
    let k: u8 = t.kind;

    if k == TK_INT() {
        p_bump(p);
        let mut e: Expr = expr_zero();
        e.kind       = EX_INT_LIT();
        e.int_val    = t.int_val;
        e.span_start = t.span_start;
        e.span_end   = t.span_end;
        return push_expr(p, e);
    }
    if k == TK_BOOL() {
        p_bump(p);
        let mut e: Expr = expr_zero();
        e.kind       = EX_BOOL_LIT();
        e.bool_val   = t.bool_val;
        e.span_start = t.span_start;
        e.span_end   = t.span_end;
        return push_expr(p, e);
    }
    if k == TK_CHAR() {
        p_bump(p);
        let mut e: Expr = expr_zero();
        e.kind       = EX_CHAR_LIT();
        e.char_val   = t.char_val;
        e.span_start = t.span_start;
        e.span_end   = t.span_end;
        return push_expr(p, e);
    }
    if k == TK_STR() {
        p_bump(p);
        let mut e: Expr = expr_zero();
        e.kind            = EX_STR_LIT();
        e.name.name_off   = t.str_off;
        e.name.name_len   = t.str_len;
        e.span_start      = t.span_start;
        e.span_end        = t.span_end;
        return push_expr(p, e);
    }
    if k == TK_KW_NULL() {
        p_bump(p);
        let mut e: Expr = expr_zero();
        e.kind       = EX_NULL();
        e.span_start = t.span_start;
        e.span_end   = t.span_end;
        return push_expr(p, e);
    }

    if k == TK_LPAREN() {
        p_bump(p);
        let inner: usize = parse_expr(p);
        p_expect(p, TK_RPAREN());
        let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
        let mut e: Expr = expr_zero();
        e.kind       = EX_PAREN();
        e.e1         = inner;
        e.span_start = t.span_start;
        e.span_end   = last.span_end;
        return push_expr(p, e);
    }

    if k == TK_LBRACE() {
        let bid: usize = parse_block(p);
        let blk: Block = vec_get::<Block>(&p.module.blocks, bid);
        let mut e: Expr = expr_zero();
        e.kind       = EX_BLOCK();
        e.e3         = bid;
        e.span_start = blk.span_start;
        e.span_end   = blk.span_end;
        return push_expr(p, e);
    }

    if k == TK_LBRACKET() {
        return parse_array_lit(p, t);
    }

    if k == TK_MINUS() { return parse_unary(p, t, UN_NEG(), no_struct_lit); }
    if k == TK_BANG()  { return parse_unary(p, t, UN_NOT(), no_struct_lit); }
    if k == TK_TILDE() { return parse_unary(p, t, UN_BITNOT(), no_struct_lit); }
    if k == TK_STAR()  { return parse_unary(p, t, UN_DEREF(), no_struct_lit); }
    if k == TK_AMP()   { return parse_addr_of(p, t, no_struct_lit); }

    if k == TK_KW_IF()       { return parse_if_expr(p); }
    if k == TK_KW_WHILE()    { return parse_while_expr(p); }
    if k == TK_KW_LOOP()     { return parse_loop_expr(p); }
    if k == TK_KW_FOR()      { return parse_for_expr(p); }
    if k == TK_KW_BREAK()    { return parse_break_expr(p); }
    if k == TK_KW_CONTINUE() { return parse_continue_expr(p); }
    if k == TK_KW_RETURN()   { return parse_return_expr(p); }
    if k == TK_KW_LET()      { return parse_let_expr(p); }

    if k == TK_IDENT() {
        return parse_ident_or_struct_lit(p, no_struct_lit);
    }

    err_unexpected(p, t.span_start, t.span_end, t.kind);
    p_bump(p);
    let mut e: Expr = expr_zero();
    e.kind       = EX_POISON();
    e.span_start = t.span_start;
    e.span_end   = t.span_end;
    return push_expr(p, e);
}

fn push_expr(p: *mut Parser, e: Expr) -> usize {
    let id: usize = vec_len::<Expr>(&p.module.exprs);
    vec_push::<Expr>(&mut p.module.exprs, e);
    return id;
}

fn parse_unary(p: *mut Parser, op_tok: Token, op: u8, no_struct_lit: bool) -> usize {
    p_bump(p);
    let inner: usize = parse_prefix(p, no_struct_lit);
    let inner_e: Expr = vec_get::<Expr>(&p.module.exprs, inner);
    let mut e: Expr = expr_zero();
    e.kind       = EX_UNARY();
    e.op         = op;
    e.e1         = inner;
    e.span_start = op_tok.span_start;
    e.span_end   = inner_e.span_end;
    return push_expr(p, e);
}

fn parse_addr_of(p: *mut Parser, amp: Token, no_struct_lit: bool) -> usize {
    p_bump(p);
    let mutable: bool = p_eat(p, TK_KW_MUT());
    let inner: usize = parse_prefix(p, no_struct_lit);
    let inner_e: Expr = vec_get::<Expr>(&p.module.exprs, inner);
    let mut e: Expr = expr_zero();
    e.kind       = EX_ADDR_OF();
    e.e1         = inner;
    e.mutable    = mutable;
    e.op         = if mutable { MUT_MUT() } else { MUT_CONST() };
    e.span_start = amp.span_start;
    e.span_end   = inner_e.span_end;
    return push_expr(p, e);
}

fn parse_array_lit(p: *mut Parser, lb: Token) -> usize {
    p_bump(p);

    if p_peek_kind(p) == TK_RBRACKET() {
        p_bump(p);
        let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
        let mut e: Expr = expr_zero();
        e.kind       = EX_ARRAY_LIT();
        e.span_start = lb.span_start;
        e.span_end   = last.span_end;
        return push_expr(p, e);
    }

    let first: usize = parse_expr(p);

    if p_eat(p, TK_SEMI()) {
        let nt: Token = p_peek(p);
        if nt.kind != TK_INT() {
            err_at_token(p, PE_INT_LIT_REQUIRED());
        }
        let int_tok: Token = if nt.kind == TK_INT() { p_bump(p) } else { nt };
        let mut len_e: Expr = expr_zero();
        len_e.kind       = EX_INT_LIT();
        len_e.int_val    = int_tok.int_val;
        len_e.span_start = int_tok.span_start;
        len_e.span_end   = int_tok.span_end;
        let len_id: usize = push_expr(p, len_e);
        p_expect(p, TK_RBRACKET());
        let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
        let mut e: Expr = expr_zero();
        e.kind       = EX_ARRAY_RPT();
        e.e1         = first;
        e.e2         = len_id;
        e.span_start = lb.span_start;
        e.span_end   = last.span_end;
        return push_expr(p, e);
    }

    let mut e: Expr = expr_zero();
    e.kind = EX_ARRAY_LIT();
    vec_push::<usize>(&mut e.args, first);
    while p_eat(p, TK_COMMA()) {
        if p_peek_kind(p) == TK_RBRACKET() { break; }
        let nx: usize = parse_expr(p);
        vec_push::<usize>(&mut e.args, nx);
    }
    p_expect(p, TK_RBRACKET());
    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    e.span_start = lb.span_start;
    e.span_end   = last.span_end;
    return push_expr(p, e);
}

fn parse_ident_or_struct_lit(p: *mut Parser, no_struct_lit: bool) -> usize {
    let nm: Ident = parse_ident(p);

    let mut type_args: Vec<usize> = vec_new::<usize>();
    let mut had_turbofish: bool = false;
    if p_peek_kind(p) == TK_COLONCOLON() && p_peek_kind_n(p, 1) == TK_LT() {
        p_bump(p);                              // ::
        p_bump(p);                              // <
        if p_peek_kind(p) != TK_GT() && p_peek_kind(p) != TK_JOINT_GT() {
            loop {
                let a: usize = parse_type(p);
                vec_push::<usize>(&mut type_args, a);
                if !p_eat(p, TK_COMMA()) { break; }
            }
        }
        if !(p_eat(p, TK_GT()) || p_eat(p, TK_JOINT_GT())) {
            err_at_token(p, PE_UNEXPECTED_TOKEN());
        }
        had_turbofish = true;
    }

    if !no_struct_lit && p_peek_kind(p) == TK_LBRACE() {
        return parse_struct_lit(p, nm, type_args);
    }

    let mut e: Expr = expr_zero();
    e.kind       = EX_IDENT();
    e.name       = nm;
    e.type_args  = type_args;
    e.span_start = nm.span_start;
    if had_turbofish {
        let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
        e.span_end = last.span_end;
    } else {
        e.span_end = nm.span_end;
    }
    return push_expr(p, e);
}

fn parse_struct_lit(p: *mut Parser, name: Ident, type_args: Vec<usize>) -> usize {
    p_expect(p, TK_LBRACE());
    let mut e: Expr = expr_zero();
    e.kind      = EX_STRUCT_LIT();
    e.name      = name;
    e.type_args = type_args;

    if p_peek_kind(p) != TK_RBRACE() {
        loop {
            let fname: Ident = parse_ident(p);
            p_expect(p, TK_COLON());
            let val: usize = parse_expr(p);
            let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
            vec_push::<StructLitField>(&mut e.sl_fields, StructLitField {
                name:       fname,
                value:      val,
                span_start: fname.span_start,
                span_end:   last.span_end,
            });
            if !p_eat(p, TK_COMMA()) { break; }
            if p_peek_kind(p) == TK_RBRACE() { break; }
        }
    }
    p_expect(p, TK_RBRACE());
    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    e.span_start = name.span_start;
    e.span_end   = last.span_end;
    return push_expr(p, e);
}

fn parse_if_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_IF());
    let cond: usize = parse_expr_no_struct_lit(p);
    let then_id: usize = parse_block(p);
    let then_b: Block = vec_get::<Block>(&p.module.blocks, then_id);
    let mut span_end: usize = then_b.span_end;
    let mut else_id: usize = ID_NONE();
    let mut else_is_block: bool = true;
    if p_eat(p, TK_KW_ELSE()) {
        if p_peek_kind(p) == TK_KW_IF() {
            let inner: usize = parse_if_expr(p);
            let inner_e: Expr = vec_get::<Expr>(&p.module.exprs, inner);
            else_id = inner;
            else_is_block = false;
            span_end = inner_e.span_end;
        } else {
            let blk: usize = parse_block(p);
            let blk_b: Block = vec_get::<Block>(&p.module.blocks, blk);
            else_id = blk;
            else_is_block = true;
            span_end = blk_b.span_end;
        }
    }
    let mut e: Expr = expr_zero();
    e.kind        = EX_IF();
    e.e1          = cond;
    e.e3          = then_id;
    e.e4          = else_id;
    e.e4_is_block = else_is_block;
    e.span_start  = kw.span_start;
    e.span_end    = span_end;
    return push_expr(p, e);
}

fn parse_while_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_WHILE());
    let cond: usize = parse_expr_no_struct_lit(p);
    let body: usize = parse_block(p);
    let body_b: Block = vec_get::<Block>(&p.module.blocks, body);
    let mut e: Expr = expr_zero();
    e.kind       = EX_WHILE();
    e.e1         = cond;
    e.e3         = body;
    e.span_start = kw.span_start;
    e.span_end   = body_b.span_end;
    return push_expr(p, e);
}

fn parse_loop_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_LOOP());
    let body: usize = parse_block(p);
    let body_b: Block = vec_get::<Block>(&p.module.blocks, body);
    let mut e: Expr = expr_zero();
    e.kind       = EX_LOOP();
    e.e3         = body;
    e.span_start = kw.span_start;
    e.span_end   = body_b.span_end;
    return push_expr(p, e);
}

fn parse_for_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_FOR());
    p_expect(p, TK_LPAREN());

    let init: usize = if p_peek_kind(p) == TK_SEMI() { ID_NONE() } else { parse_expr(p) };
    p_expect(p, TK_SEMI());
    let cond: usize = if p_peek_kind(p) == TK_SEMI() { ID_NONE() } else { parse_expr(p) };
    p_expect(p, TK_SEMI());
    let update: usize = if p_peek_kind(p) == TK_RPAREN() { ID_NONE() } else { parse_expr(p) };
    p_expect(p, TK_RPAREN());

    let body: usize = parse_block(p);
    let body_b: Block = vec_get::<Block>(&p.module.blocks, body);
    let mut e: Expr = expr_zero();
    e.kind       = EX_FOR();
    e.e1         = init;
    e.e2         = cond;
    e.t1         = update;
    e.e3         = body;
    e.span_start = kw.span_start;
    e.span_end   = body_b.span_end;
    return push_expr(p, e);
}

fn parse_break_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_BREAK());
    let mut e: Expr = expr_zero();
    e.kind       = EX_BREAK();
    e.span_start = kw.span_start;
    e.span_end   = kw.span_end;
    let k: u8 = p_peek_kind(p);
    if !(k == TK_SEMI() || k == TK_RBRACE() || k == TK_RPAREN() || k == TK_COMMA()) {
        let val: usize = parse_expr(p);
        let val_e: Expr = vec_get::<Expr>(&p.module.exprs, val);
        e.e1       = val;
        e.span_end = val_e.span_end;
    }
    return push_expr(p, e);
}

fn parse_continue_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_CONTINUE());
    let mut e: Expr = expr_zero();
    e.kind       = EX_CONTINUE();
    e.span_start = kw.span_start;
    e.span_end   = kw.span_end;
    return push_expr(p, e);
}

fn parse_return_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_RETURN());
    let mut e: Expr = expr_zero();
    e.kind       = EX_RETURN();
    e.span_start = kw.span_start;
    e.span_end   = kw.span_end;
    let k: u8 = p_peek_kind(p);
    if !(k == TK_SEMI() || k == TK_RBRACE() || k == TK_RPAREN() || k == TK_COMMA()) {
        let val: usize = parse_expr(p);
        let val_e: Expr = vec_get::<Expr>(&p.module.exprs, val);
        e.e1       = val;
        e.span_end = val_e.span_end;
    }
    return push_expr(p, e);
}

fn parse_let_expr(p: *mut Parser) -> usize {
    let kw: Token = p_expect(p, TK_KW_LET());
    let mutable: bool = p_eat(p, TK_KW_MUT());
    let name: Ident = parse_ident(p);
    let mut ty: usize = ID_NONE();
    if p_eat(p, TK_COLON()) {
        ty = parse_type(p);
    }
    let mut init: usize = ID_NONE();
    if p_eat(p, TK_EQ()) {
        init = parse_expr(p);
    }
    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    let mut e: Expr = expr_zero();
    e.kind       = EX_LET();
    e.mutable    = mutable;
    e.name       = name;
    e.t1         = ty;
    e.e1         = init;
    e.span_start = kw.span_start;
    e.span_end   = last.span_end;
    return push_expr(p, e);
}

// ---------------------------------------------------------------- //
// Infix / postfix loop                                             //
// ---------------------------------------------------------------- //

fn parse_infix(p: *mut Parser, lhs0: usize, min_prec: u32, no_struct_lit: bool) -> usize {
    let mut lhs: usize = lhs0;
    loop {
        let k: u8 = p_peek_kind(p);

        if k == TK_LPAREN()   { lhs = parse_call(p, lhs); continue; }
        if k == TK_LBRACKET() { lhs = parse_index(p, lhs); continue; }
        if k == TK_DOT()      { lhs = parse_field(p, lhs); continue; }

        if k == TK_KW_AS() && 10 >= min_prec {
            lhs = parse_cast(p, lhs);
            continue;
        }

        // `>>=` reassembly: JointGt + JointGt + Eq → ShrEq (assign,
        // min_prec 0). Three-token lookahead — must come before the
        // generic assign and binary checks below.
        if min_prec == 0
            && k == TK_JOINT_GT()
            && p_peek_kind_n(p, 1) == TK_JOINT_GT()
            && p_peek_kind_n(p, 2) == TK_EQ()
        {
            p_bump(p); p_bump(p); p_bump(p);
            let rhs: usize = parse_expr(p);
            let rhs_e: Expr = vec_get::<Expr>(&p.module.exprs, rhs);
            let lhs_e: Expr = vec_get::<Expr>(&p.module.exprs, lhs);
            let mut e: Expr = expr_zero();
            e.kind       = EX_ASSIGN();
            e.op         = AS_SHR();
            e.e1         = lhs;
            e.e2         = rhs;
            e.span_start = lhs_e.span_start;
            e.span_end   = rhs_e.span_end;
            lhs = push_expr(p, e);
            continue;
        }

        if min_prec == 0 {
            let aop: i32 = assign_op_for(k);
            if aop >= 0 {
                p_bump(p);
                let rhs: usize = parse_expr(p);
                let rhs_e: Expr = vec_get::<Expr>(&p.module.exprs, rhs);
                let lhs_e: Expr = vec_get::<Expr>(&p.module.exprs, lhs);
                let mut e: Expr = expr_zero();
                e.kind       = EX_ASSIGN();
                e.op         = aop as u8;
                e.e1         = lhs;
                e.e2         = rhs;
                e.span_start = lhs_e.span_start;
                e.span_end   = rhs_e.span_end;
                lhs = push_expr(p, e);
                continue;
            }
        }

        let bp: i32 = bin_prec(k);
        if bp < 0 { break; }
        let prec: u32 = bp as u32;
        if prec < min_prec { break; }

        let bop: u8 = bin_op_for(k);
        let op_tok: Token = p_bump(p);

        // Reassemble multi-token `>` forms:
        //   JointGt + Eq                 → Ge
        //   JointGt + Gt | JointGt       → Shr
        // The single-token `>` cases (Gt alone, JointGt alone with
        // a non-`=`/`>` follower) keep `bop = BIN_GT()`.
        let actual_bop: u8 = if op_tok.kind == TK_JOINT_GT() {
            let nk: u8 = p_peek_kind(p);
            if nk == TK_EQ() {
                p_bump(p);
                BIN_GE()
            } else if nk == TK_GT() || nk == TK_JOINT_GT() {
                p_bump(p);
                BIN_SHR()
            } else {
                bop
            }
        } else {
            bop
        };

        let rhs: usize = parse_expr_with(p, prec + 1, no_struct_lit);
        let rhs_e: Expr = vec_get::<Expr>(&p.module.exprs, rhs);
        let lhs_e: Expr = vec_get::<Expr>(&p.module.exprs, lhs);
        let mut e: Expr = expr_zero();
        e.kind       = EX_BINARY();
        e.op         = actual_bop;
        e.e1         = lhs;
        e.e2         = rhs;
        e.span_start = lhs_e.span_start;
        e.span_end   = rhs_e.span_end;
        lhs = push_expr(p, e);
    }
    return lhs;
}

fn parse_call(p: *mut Parser, callee: usize) -> usize {
    p_expect(p, TK_LPAREN());
    let callee_e: Expr = vec_get::<Expr>(&p.module.exprs, callee);

    let mut e: Expr = expr_zero();
    e.kind = EX_CALL();
    e.e1   = callee;
    // Lift turbofish from Ident-callee onto the Call node. We move
    // the type_args list out of the callee and put it on the Call.
    if callee_e.kind == EX_IDENT() && vec_len::<usize>(&callee_e.type_args) > 0 {
        e.type_args = callee_e.type_args;
        // The callee's expression record still has its old (shared
        // backing) type_args field too; it's unused after the lift.
    }

    if p_peek_kind(p) != TK_RPAREN() {
        loop {
            let a: usize = parse_expr(p);
            vec_push::<usize>(&mut e.args, a);
            if !p_eat(p, TK_COMMA()) { break; }
        }
    }
    p_expect(p, TK_RPAREN());
    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    e.span_start = callee_e.span_start;
    e.span_end   = last.span_end;
    return push_expr(p, e);
}

fn parse_index(p: *mut Parser, base: usize) -> usize {
    p_expect(p, TK_LBRACKET());
    let idx: usize = parse_expr(p);
    p_expect(p, TK_RBRACKET());
    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    let base_e: Expr = vec_get::<Expr>(&p.module.exprs, base);
    let mut e: Expr = expr_zero();
    e.kind       = EX_INDEX();
    e.e1         = base;
    e.e2         = idx;
    e.span_start = base_e.span_start;
    e.span_end   = last.span_end;
    return push_expr(p, e);
}

fn parse_field(p: *mut Parser, base: usize) -> usize {
    p_expect(p, TK_DOT());
    let nm: Ident = parse_ident(p);
    let base_e: Expr = vec_get::<Expr>(&p.module.exprs, base);
    let mut e: Expr = expr_zero();
    e.kind       = EX_FIELD();
    e.e1         = base;
    e.name       = nm;
    e.span_start = base_e.span_start;
    e.span_end   = nm.span_end;
    return push_expr(p, e);
}

fn parse_cast(p: *mut Parser, expr: usize) -> usize {
    p_expect(p, TK_KW_AS());
    let ty: usize = parse_type(p);
    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    let base_e: Expr = vec_get::<Expr>(&p.module.exprs, expr);
    let mut e: Expr = expr_zero();
    e.kind       = EX_CAST();
    e.e1         = expr;
    e.t1         = ty;
    e.span_start = base_e.span_start;
    e.span_end   = last.span_end;
    return push_expr(p, e);
}

fn bin_op_for(k: u8) -> u8 {
    if k == TK_PLUS()    { return BIN_ADD(); }
    if k == TK_MINUS()   { return BIN_SUB(); }
    if k == TK_STAR()    { return BIN_MUL(); }
    if k == TK_SLASH()   { return BIN_DIV(); }
    if k == TK_PERCENT() { return BIN_REM(); }
    if k == TK_EQEQ()    { return BIN_EQ(); }
    if k == TK_NE()      { return BIN_NE(); }
    if k == TK_LT()      { return BIN_LT(); }
    if k == TK_LE()      { return BIN_LE(); }
    if k == TK_GT()      { return BIN_GT(); }
    if k == TK_JOINT_GT(){ return BIN_GT(); }
    if k == TK_ANDAND()  { return BIN_AND(); }
    if k == TK_OROR()    { return BIN_OR(); }
    if k == TK_AMP()     { return BIN_BITAND(); }
    if k == TK_PIPE()    { return BIN_BITOR(); }
    if k == TK_CARET()   { return BIN_BITXOR(); }
    if k == TK_SHL()     { return BIN_SHL(); }
    return 255;
}

fn bin_prec(k: u8) -> i32 {
    if k == TK_OROR()    { return 1; }
    if k == TK_ANDAND()  { return 2; }
    if k == TK_EQEQ() || k == TK_NE() || k == TK_LT() || k == TK_LE()
        || k == TK_GT() || k == TK_JOINT_GT()
    { return 3; }
    if k == TK_PIPE()    { return 4; }
    if k == TK_CARET()   { return 5; }
    if k == TK_AMP()     { return 6; }
    if k == TK_SHL()     { return 7; }
    if k == TK_PLUS() || k == TK_MINUS()    { return 8; }
    if k == TK_STAR() || k == TK_SLASH() || k == TK_PERCENT() { return 9; }
    return -1;
}

fn assign_op_for(k: u8) -> i32 {
    if k == TK_EQ()        { return AS_EQ()     as i32; }
    if k == TK_PLUSEQ()    { return AS_ADD()    as i32; }
    if k == TK_MINUSEQ()   { return AS_SUB()    as i32; }
    if k == TK_STAREQ()    { return AS_MUL()    as i32; }
    if k == TK_SLASHEQ()   { return AS_DIV()    as i32; }
    if k == TK_PERCENTEQ() { return AS_REM()    as i32; }
    if k == TK_AMPEQ()     { return AS_BITAND() as i32; }
    if k == TK_PIPEEQ()    { return AS_BITOR()  as i32; }
    if k == TK_CARETEQ()   { return AS_BITXOR() as i32; }
    if k == TK_SHLEQ()     { return AS_SHL()    as i32; }
    return -1;
}

// ---------------------------------------------------------------- //
// Block                                                            //
// ---------------------------------------------------------------- //

fn parse_block(p: *mut Parser) -> usize {
    let lb: Token = p_expect(p, TK_LBRACE());
    let mut b: Block = block_zero();

    while p_peek_kind(p) != TK_RBRACE() && !p_at_eof(p) {
        let e_id: usize = parse_expr(p);
        let has_semi: bool = p_eat(p, TK_SEMI());
        vec_push::<BlockItem>(&mut b.items, BlockItem {
            expr: e_id, has_semi: has_semi,
        });
    }
    p_expect(p, TK_RBRACE());

    let last: Token = vec_get::<Token>(&p.tokens, p.pos - 1);
    b.span_start = lb.span_start;
    b.span_end   = last.span_end;
    let id: usize = vec_len::<Block>(&p.module.blocks);
    vec_push::<Block>(&mut p.module.blocks, b);
    return id;
}
