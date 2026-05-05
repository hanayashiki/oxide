// M4 HIR smoke test — lower a simple program and print stats.
import "stdio.ox";
import "string.ox";
import "../lexer.ox";
import "../parser.ox";
import "../ast.ox";
import "../hir.ox";
import "../hir_lower.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";

fn lower_src(s: *const [u8]) -> HirProgram {
    let n: usize = strlen(s);
    let lx: Lexer = lex(s, n);
    let ast: Module = parse_program(lx);
    lower_program(ast)
}

fn main() -> i32 {
    let p: HirProgram = lower_src(
        "struct Point { x: i32, y: i32 } fn f(p: *mut Point) -> i32 { p.x = 7; p.x + p.y }"
    );
    printf("fns=%zu adts=%zu locals=%zu exprs=%zu blocks=%zu types=%zu errors=%zu\n",
           vec_len::<HirFn>(&p.fns),
           vec_len::<HirAdt>(&p.adts),
           vec_len::<HirLocal>(&p.locals),
           vec_len::<HirExpr>(&p.exprs),
           vec_len::<HirBlock>(&p.blocks),
           vec_len::<HirTy>(&p.types),
           vec_len::<HirError>(&p.errors));
    0
}
