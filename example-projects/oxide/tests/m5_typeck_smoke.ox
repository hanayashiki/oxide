// M5 typeck smoke test.
import "stdio.ox";
import "string.ox";
import "../lexer.ox";
import "../parser.ox";
import "../ast.ox";
import "../hir.ox";
import "../hir_lower.ox";
import "../typeck.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";

fn check_src(s: *const [u8]) -> Checker {
    let n: usize = strlen(s);
    let lx: Lexer = lex(s, n);
    let ast: Module = parse_program(lx);
    let hp: HirProgram = lower_program(ast);
    typeck(hp)
}

fn main() -> i32 {
    let c: Checker = check_src(
        "struct Point { x: i32, y: i32 } fn f(p: *mut Point) -> i32 { p.x = 7; p.x + p.y }"
    );
    let n_exprs: usize = vec_len::<usize>(&c.results.expr_tys);
    let n_ty: usize = vec_len::<Ty>(&c.results.tys.tys);
    let n_err: usize = vec_len::<TypeckError>(&c.results.errors);
    printf("expr_tys=%zu tys=%zu errors=%zu\n", n_exprs, n_ty, n_err);
    0
}
