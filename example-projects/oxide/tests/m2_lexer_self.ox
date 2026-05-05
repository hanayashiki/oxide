// M2 self-lex acceptance: stage-1 lexer reads its own source from
// disk and confirms zero TK_ERROR tokens. Snapshot captures the
// "n_tokens / n_errors" line so a regression flips the digit.

import "stdio.ox";
import "stdlib.ox";
import "intrinsics.ox";
import "../lexer.ox";
import "../util/vec.ox";
import "../util/strbuf.ox";

fn slurp(path: *const [u8]) -> StrBuf {
    let f: *mut u8 = fopen(path, "rb");
    if ox_transmute::<*mut u8, usize>(f) == 0 {
        puts("self_lex: fopen failed");
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

fn main() -> i32 {
    // Path is relative to the host's cwd at invocation time. The
    // run.sh harness runs from the repo root, so this resolves to
    // example-projects/oxide/lexer.ox.
    let src: StrBuf = slurp("example-projects/oxide/lexer.ox");
    let lx: Lexer = lex(strbuf_as_ptr(&src), strbuf_len(&src));
    let n: usize = vec_len::<Token>(&lx.tokens);
    let mut n_err: usize = 0;
    let mut i: usize = 0;
    while i < n {
        let t: Token = vec_get::<Token>(&lx.tokens, i);
        if t.kind == TK_ERROR() {
            n_err = n_err + 1;
        }
        i = i + 1;
    }
    printf("self_lex: tokens=%zu errors=%zu\n", n, n_err);
    if n_err == 0 { 0 } else { 1 }
}
