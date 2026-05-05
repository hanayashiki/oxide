// M1 io.ox snapshot test.

import "../util/io.ox";
import "../util/strbuf.ox";

fn main() -> i32 {
    // --- io_print on raw bytes (explicit length) --------------------
    let m1: *const [u8] = "io_print: hi\n";
    io_print(m1, 13);

    // --- io_print_cstr (length-via-strlen) --------------------------
    io_print_cstr("io_print_cstr: also hi\n");

    // --- io_print_strbuf --------------------------------------------
    let mut s: StrBuf = strbuf_new();
    strbuf_push_str(&mut s, "io_print_strbuf: number ", 24);
    strbuf_push_i64(&mut s, -42);
    strbuf_push_byte(&mut s, 32);                            // ' '
    strbuf_push_str(&mut s, "hex ", 4);
    strbuf_push_hex_u64(&mut s, 48879);                      // 0xbeef
    strbuf_push_byte(&mut s, 10);                            // '\n'
    io_print_strbuf(&s);

    // Eprint paths: both go to stderr; we don't capture stderr in
    // the snapshot, but we can route a copy to stdout via raw print
    // to confirm the helpers run without side-issue.
    io_print_cstr("(eprint paths exercised below; output suppressed)\n");
    io_eprint("[stderr] one\n", 13);
    io_eprint_cstr("[stderr] two\n");
    let mut e: StrBuf = strbuf_new();
    strbuf_push_str(&mut e, "[stderr] three\n", 15);
    io_eprint_strbuf(&e);

    0
}
