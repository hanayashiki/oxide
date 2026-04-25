use std::path::PathBuf;

use expect_test::{Expect, expect};
use oxide::lexer::{TokenKind, lex};
use oxide::reporter::{Diagnostic, FileId, Severity, SourceMap, emit, from_lex_error};

// ---------- helpers ----------

fn diags_for(src: &str) -> (SourceMap, FileId, Vec<Diagnostic>) {
    let mut map = SourceMap::new();
    let file = map.add(PathBuf::from("test.ox"), src.to_string());
    let diags = lex(src)
        .into_iter()
        .filter_map(|t| match t.kind {
            TokenKind::Error(e) => Some(from_lex_error(&e, file, t.span)),
            _ => None,
        })
        .collect();
    (map, file, diags)
}

fn render(src: &str) -> String {
    let (map, _, diags) = diags_for(src);
    let mut out: Vec<u8> = Vec::new();
    for d in &diags {
        emit(d, &map, &mut out, false).unwrap();
    }
    String::from_utf8(out).unwrap()
}

fn check_render(src: &str, expected: Expect) {
    expected.assert_eq(&render(src));
}

// ---------- structured assertions ----------

#[test]
fn from_lex_error_sets_code_and_severity() {
    let (_, file, diags) = diags_for("@");
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.code, Some("E0001"));
    assert!(d.message.contains("unexpected character"));
    assert_eq!(d.labels.len(), 1);
    assert!(d.labels[0].primary);
    assert_eq!(d.labels[0].file, file);
}

#[test]
fn unterminated_block_comment_has_help() {
    let (_, _, diags) = diags_for("/* never ends");
    assert_eq!(diags[0].code, Some("E0002"));
    assert_eq!(diags[0].helps.len(), 1);
    assert!(diags[0].helps[0].contains("nest"));
}

#[test]
fn empty_char_has_null_byte_help() {
    let (_, _, diags) = diags_for("''");
    assert_eq!(diags[0].code, Some("E0005"));
    assert!(diags[0].helps[0].contains("\\0"));
}

#[test]
fn bad_escape_lists_valid_escapes() {
    let (_, _, diags) = diags_for(r"'\q'");
    assert_eq!(diags[0].code, Some("E0006"));
    assert!(diags[0].helps[0].contains("\\n"));
}

#[test]
fn no_diagnostic_for_clean_source() {
    let (_, _, diags) = diags_for("fn main() { return 0; }");
    assert!(diags.is_empty());
}

// ---------- snapshot tests of rendered output ----------

#[test]
fn snapshot_unexpected_char() {
    check_render(
        "let x = @ + 1;",
        expect![[r#"
            [E0001] Error: unexpected character '@'
               ╭─[ test.ox:1:9 ]
               │
             1 │ let x = @ + 1;
               │         ┬  
               │         ╰── not a valid token
            ───╯
        "#]],
    );
}

#[test]
fn snapshot_unterminated_block_comment() {
    check_render(
        "fn main() {\n  /* unfinished\n}",
        expect![[r#"
            [E0002] Error: unterminated block comment
               ╭─[ test.ox:2:3 ]
               │
             2 │ ╭─▶   /* unfinished
             3 │ ├─▶ }
               │ │      
               │ ╰────── comment starts here
               │     
               │     Help: block comments nest; check for an unmatched `/*`
            ───╯
        "#]],
    );
}

#[test]
fn snapshot_unterminated_string() {
    check_render(
        r#"let s = "oops"#,
        expect![[r#"
            [E0003] Error: unterminated string literal
               ╭─[ test.ox:1:9 ]
               │
             1 │ let s = "oops
               │         ──┬──  
               │           ╰──── string starts here
            ───╯
        "#]],
    );
}

#[test]
fn snapshot_empty_char() {
    check_render(
        "let c = '';",
        expect![[r#"
            [E0005] Error: empty char literal
               ╭─[ test.ox:1:9 ]
               │
             1 │ let c = '';
               │         ─┬  
               │          ╰── no character between quotes
               │ 
               │ Help: use '\0' for the null byte
            ───╯
        "#]],
    );
}

#[test]
fn snapshot_bad_escape() {
    check_render(
        r#"let s = "a\qb";"#,
        expect![[r#"
            [E0006] Error: invalid escape sequence
               ╭─[ test.ox:1:11 ]
               │
             1 │ let s = "a\qb";
               │           ─┬  
               │            ╰── this escape is not recognised
               │ 
               │ Help: valid escapes: \n \r \t \\ \' \" \0 \xHH
            ───╯
        "#]],
    );
}

#[test]
fn snapshot_int_overflow() {
    check_render(
        "let x = 99999999999999999999;",
        expect![[r#"
            [E0007] Error: integer literal overflows u64
               ╭─[ test.ox:1:9 ]
               │
             1 │ let x = 99999999999999999999;
               │         ──────────┬─────────  
               │                   ╰─────────── value exceeds 2^64 - 1
            ───╯
        "#]],
    );
}

#[test]
fn snapshot_invalid_digit() {
    check_render(
        "let x = 0b2;",
        expect![[r#"
            [E0008] Error: invalid digit for numeric base
               ╭─[ test.ox:1:9 ]
               │
             1 │ let x = 0b2;
               │         ─┬─  
               │          ╰─── this digit is not valid here
            ───╯
        "#]],
    );
}

#[test]
fn snapshot_unterminated_char() {
    check_render(
        "let c = 'a;",
        expect![[r#"
            [E0004] Error: unterminated char literal
               ╭─[ test.ox:1:9 ]
               │
             1 │ let c = 'a;
               │         ─┬─  
               │          ╰─── char literal starts here
            ───╯
        "#]],
    );
}
