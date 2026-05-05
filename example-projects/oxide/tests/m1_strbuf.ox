// M1 strbuf.ox snapshot test.

import "stdio.ox";
import "../util/strbuf.ox";

fn main() -> i32 {
    // --- empty -------------------------------------------------------
    let mut s: StrBuf = strbuf_new();
    printf("empty: len=%zu is_empty=%d\n", strbuf_len(&s), strbuf_is_empty(&s) as i32);

    // --- push_byte ---------------------------------------------------
    strbuf_push_byte(&mut s, 104);    // 'h'
    strbuf_push_byte(&mut s, 105);    // 'i'
    printf("after push_byte hi: len=%zu\n", strbuf_len(&s));

    // --- push_str ----------------------------------------------------
    let extra = ", world";
    strbuf_push_str(&mut s, extra, 7);
    printf("after push_str: len=%zu\n", strbuf_len(&s));

    // --- push_cstr ---------------------------------------------------
    strbuf_push_cstr(&mut s, "!");
    printf("after push_cstr: len=%zu\n", strbuf_len(&s));

    // --- as_cstr (printable) ----------------------------------------
    let cstr: *const [u8] = strbuf_as_cstr(&mut s);
    printf("contents: %s\n", cstr);

    // --- push_u64 ----------------------------------------------------
    let mut n: StrBuf = strbuf_new();
    strbuf_push_u64(&mut n, 0);
    strbuf_push_byte(&mut n, 32);
    strbuf_push_u64(&mut n, 1);
    strbuf_push_byte(&mut n, 32);
    strbuf_push_u64(&mut n, 42);
    strbuf_push_byte(&mut n, 32);
    strbuf_push_u64(&mut n, 18446744073709551615);    // u64::MAX
    let nstr: *const [u8] = strbuf_as_cstr(&mut n);
    printf("u64 fmt: %s\n", nstr);

    // --- push_i64 ----------------------------------------------------
    let mut sn: StrBuf = strbuf_new();
    strbuf_push_i64(&mut sn, 0);
    strbuf_push_byte(&mut sn, 32);
    strbuf_push_i64(&mut sn, 42);
    strbuf_push_byte(&mut sn, 32);
    strbuf_push_i64(&mut sn, -7);
    strbuf_push_byte(&mut sn, 32);
    strbuf_push_i64(&mut sn, 9223372036854775807);    // i64::MAX
    strbuf_push_byte(&mut sn, 32);
    // i64::MIN — relies on wrapping unsigned negation
    strbuf_push_i64(&mut sn, -9223372036854775807 - 1);
    let snstr: *const [u8] = strbuf_as_cstr(&mut sn);
    printf("i64 fmt: %s\n", snstr);

    // --- push_hex_u64 ------------------------------------------------
    let mut h: StrBuf = strbuf_new();
    strbuf_push_hex_u64(&mut h, 0);
    strbuf_push_byte(&mut h, 32);
    strbuf_push_hex_u64(&mut h, 255);
    strbuf_push_byte(&mut h, 32);
    strbuf_push_hex_u64(&mut h, 4096);
    strbuf_push_byte(&mut h, 32);
    strbuf_push_hex_u64(&mut h, 18446744073709551615);
    let hstr: *const [u8] = strbuf_as_cstr(&mut h);
    printf("hex fmt: %s\n", hstr);

    // --- eq_bytes ----------------------------------------------------
    let mut e: StrBuf = strbuf_new();
    strbuf_push_str(&mut e, "abc", 3);
    printf("eq abc=%d eq abd=%d eq abcd=%d\n",
           strbuf_eq_bytes(&e, "abc", 3) as i32,
           strbuf_eq_bytes(&e, "abd", 3) as i32,
           strbuf_eq_bytes(&e, "abcd", 4) as i32);

    // --- clear -------------------------------------------------------
    strbuf_clear(&mut s);
    printf("after clear: len=%zu\n", strbuf_len(&s));

    0
}
