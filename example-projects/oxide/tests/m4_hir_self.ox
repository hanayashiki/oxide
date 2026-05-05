// M4 self-host acceptance: lex + parse + lower each stage-1 source
// file. Reports HIR-stage error counts.

import "stdio.ox";
import "stdlib.ox";
import "intrinsics.ox";
import "../lexer.ox";
import "../parser.ox";
import "../ast.ox";
import "../hir.ox";
import "../hir_lower.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";

fn slurp(path: *const [u8]) -> StrBuf {
    let f: *mut u8 = fopen(path, "rb");
    if ox_transmute::<*mut u8, usize>(f) == 0 {
        printf("m4_hir_self: fopen failed for %s\n", path);
        abort();
    }
    let mut s: StrBuf = strbuf_with_capacity(65536);
    let mut chunk: [u8; 4096] = [0; 4096];
    loop {
        let buf_ptr: *mut u8 = ox_transmute::<*mut [u8; 4096], *mut u8>(&mut chunk);
        let n: usize = fread(buf_ptr, 1, 4096, f);
        if n == 0 { break; }
        strbuf_push_str(&mut s, &chunk, n);
        if n < 4096 { break; }
    }
    fclose(f);
    s
}

fn lower_file(path: *const [u8]) {
    let src: StrBuf = slurp(path);
    let lx: Lexer = lex(strbuf_as_ptr(&src), strbuf_len(&src));
    let ast: Module = parse_program(lx);
    if vec_len::<ParseError>(&ast.errors) > 0 {
        printf("%s: parse-errors=%zu (skipping HIR)\n",
               path, vec_len::<ParseError>(&ast.errors));
        return;
    }
    let hp: HirProgram = lower_program(ast);
    printf("%s: fns=%zu adts=%zu locals=%zu exprs=%zu errors=%zu\n",
           path,
           vec_len::<HirFn>(&hp.fns),
           vec_len::<HirAdt>(&hp.adts),
           vec_len::<HirLocal>(&hp.locals),
           vec_len::<HirExpr>(&hp.exprs),
           vec_len::<HirError>(&hp.errors));
}

fn main() -> i32 {
    lower_file("example-projects/oxide/util/vec.ox");
    lower_file("example-projects/oxide/util/strbuf.ox");
    lower_file("example-projects/oxide/util/strmap.ox");
    lower_file("example-projects/oxide/util/arena.ox");
    lower_file("example-projects/oxide/util/io.ox");
    lower_file("example-projects/oxide/lexer.ox");
    lower_file("example-projects/oxide/ast.ox");
    lower_file("example-projects/oxide/parser.ox");
    lower_file("example-projects/oxide/ast_pretty.ox");
    lower_file("example-projects/oxide/hir.ox");
    lower_file("example-projects/oxide/hir_lower.ox");
    0
}
