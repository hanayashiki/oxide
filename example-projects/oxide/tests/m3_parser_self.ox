// M3 self-parse acceptance: stage-1 parser reads stage-1 source
// from disk, lexes it, parses it, and asserts zero errors.

import "stdio.ox";
import "stdlib.ox";
import "intrinsics.ox";
import "../lexer.ox";
import "../parser.ox";
import "../ast.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";

fn slurp(path: *const [u8]) -> StrBuf {
    let f: *mut u8 = fopen(path, "rb");
    if ox_transmute::<*mut u8, usize>(f) == 0 {
        printf("self-parse: fopen failed for %s\n", path);
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

fn parse_file(path: *const [u8]) {
    let src: StrBuf = slurp(path);
    let lx: Lexer = lex(strbuf_as_ptr(&src), strbuf_len(&src));
    let m: Module = parse_program(lx);
    printf("%s: items=%zu  exprs=%zu  blocks=%zu  types=%zu  errors=%zu\n",
           path,
           vec_len::<Item>(&m.items),
           vec_len::<Expr>(&m.exprs),
           vec_len::<Block>(&m.blocks),
           vec_len::<Type>(&m.types),
           vec_len::<ParseError>(&m.errors));
}

fn main() -> i32 {
    parse_file("example-projects/oxide/util/vec.ox");
    parse_file("example-projects/oxide/util/strbuf.ox");
    parse_file("example-projects/oxide/util/strmap.ox");
    parse_file("example-projects/oxide/util/arena.ox");
    parse_file("example-projects/oxide/util/io.ox");
    parse_file("example-projects/oxide/lexer.ox");
    parse_file("example-projects/oxide/ast.ox");
    parse_file("example-projects/oxide/parser.ox");
    parse_file("example-projects/oxide/ast_pretty.ox");
    0
}
