// M5 self-host acceptance for typeck.
import "stdio.ox";
import "stdlib.ox";
import "intrinsics.ox";
import "../lexer.ox";
import "../parser.ox";
import "../ast.ox";
import "../hir.ox";
import "../hir_lower.ox";
import "../typeck.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";

fn slurp(path: *const [u8]) -> StrBuf {
    let f: *mut u8 = fopen(path, "rb");
    if ox_transmute::<*mut u8, usize>(f) == 0 {
        printf("m5_typeck_self: fopen failed for %s\n", path);
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

fn check_file(path: *const [u8]) {
    let src: StrBuf = slurp(path);
    let lx: Lexer = lex(strbuf_as_ptr(&src), strbuf_len(&src));
    let ast: Module = parse_program(lx);
    if vec_len::<ParseError>(&ast.errors) > 0 {
        printf("%s: parse-errors=%zu (skipping)\n",
               path, vec_len::<ParseError>(&ast.errors));
        return;
    }
    let hp: HirProgram = lower_program(ast);
    let c: Checker = typeck(hp);
    printf("%s: tcerrs=%zu calls=%zu\n",
           path,
           vec_len::<TypeckError>(&c.results.errors),
           vec_len::<CallTypeArgs>(&c.results.call_type_args));
}

fn main() -> i32 {
    check_file("example-projects/oxide/util/vec.ox");
    check_file("example-projects/oxide/util/strbuf.ox");
    check_file("example-projects/oxide/util/strmap.ox");
    check_file("example-projects/oxide/util/arena.ox");
    check_file("example-projects/oxide/util/io.ox");
    check_file("example-projects/oxide/lexer.ox");
    check_file("example-projects/oxide/ast.ox");
    check_file("example-projects/oxide/parser.ox");
    check_file("example-projects/oxide/ast_pretty.ox");
    check_file("example-projects/oxide/hir.ox");
    check_file("example-projects/oxide/hir_lower.ox");
    check_file("example-projects/oxide/typeck.ox");
    0
}
