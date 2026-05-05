// M3 parser snapshot test — parse various source forms and dump
// the resulting AST. Captures a stable view per case.

import "stdio.ox";
import "string.ox";
import "../lexer.ox";
import "../parser.ox";
import "../ast.ox";
import "../ast_pretty.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";
import "../util/io.ox";

fn parse_src(s: *const [u8]) -> Module {
    let n: usize = strlen(s);
    let lx: Lexer = lex(s, n);
    parse_program(lx)
}

fn run_case(label: *const [u8], src: *const [u8]) {
    printf("=== %s ===\n", label);
    let mut m: Module = parse_src(src);
    let mut dump: StrBuf = pretty_module(&m);
    let cstr: *const [u8] = strbuf_as_cstr(&mut dump);
    printf("%s", cstr);
    if vec_len::<ParseError>(&m.errors) > 0 {
        printf("(%zu parse errors)\n", vec_len::<ParseError>(&m.errors));
    }
}

fn main() -> i32 {
    run_case("hello-world",
             "fn main() -> i32 { 42 }");

    run_case("import + extern",
             "import \"stdio.ox\";\nextern \"C\" {\n  fn puts(s: *const [u8]) -> i32;\n}\nfn main() -> i32 { puts(\"hi\"); 0 }");

    run_case("struct + generic struct",
             "struct Point { x: i32, y: i32 } struct Vec<T> { data: *mut [T], len: usize }");

    run_case("generic fn + turbofish call",
             "fn id<T>(x: T) -> T { x } fn main() -> i32 { id::<i32>(42) }");

    run_case("arithmetic precedence",
             "fn f() -> i32 { 1 + 2 * 3 - 4 / 2 % 5 }");

    run_case("short-circuit + comparisons",
             "fn f(a: i32, b: i32) -> bool { a < b && a + 1 != b || a == 0 }");

    run_case("bitwise + shifts",
             "fn f(a: u32, b: u32) -> u32 { (a & b) | (a ^ b) << 2 }");

    run_case("if/else expression",
             "fn f(x: i32) -> i32 { if x > 0 { x } else if x == 0 { 0 } else { -x } }");

    run_case("while + for + loop + break/continue",
             "fn f() -> i32 {\n  let mut i: i32 = 0;\n  while i < 10 { i = i + 1; }\n  for (let mut j: i32 = 0; j < 4; j = j + 1) { if j == 2 { continue; } }\n  loop { break 7; }\n}");

    run_case("address-of, deref, cast",
             "fn f(p: *mut i32) -> i32 { *p = 1; let q: *const i32 = &mut *p as *const i32; *q }");

    run_case("array literal + repeat + index",
             "fn f() -> i32 { let a: [i32; 3] = [1, 2, 3]; let b: [u8; 4] = [0; 4]; a[0] + (b[1] as i32) }");

    run_case("struct literal + field",
             "struct P { x: i32, y: i32 } fn f() -> i32 { let p = P { x: 1, y: 2 }; p.x + p.y }");

    run_case("variadic extern",
             "extern \"C\" { fn printf(fmt: *const [u8], ...) -> i32; }");

    run_case("error: stray token",
             "fn f() -> i32 { @ }");

    0
}
