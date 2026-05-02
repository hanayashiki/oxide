//! Structural / error-shape unit tests for the parser.
//!
//! Behavioral / pretty-print cases live as `.ox` + `.snap` pairs under
//! `tests/snapshots/parser/` and are exercised by
//! `tests/parser_snapshot.rs`. The tests here check things that rendered
//! output cannot — `ParseError` variants, recoverability, and bare
//! "this must not parse" assertions.

use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::reporter::FileId;

const FID: FileId = FileId(0);

#[test]
fn reserved_keyword_match_yields_e0104() {
    let tokens = lex("fn f() { match x { } }", FID);
    let (_, errors) = parse(&tokens, FID);
    let codes: Vec<_> = errors
        .iter()
        .map(|e| match e {
            oxide::parser::ParseError::ReservedKeyword { kw, .. } => *kw,
            _ => "OTHER",
        })
        .collect();
    assert!(codes.contains(&"match"), "expected match keyword error, got {codes:?}");
}

#[test]
fn lex_error_passes_through_as_e0105() {
    // BadEscape inside a char literal — lexer emits Error token, parser
    // should re-emit it as LexErrorToken.
    let tokens = lex("fn f() { let x = '\\q'; }", FID);
    let (_, errors) = parse(&tokens, FID);
    let saw_lex_err = errors.iter().any(|e| matches!(e, oxide::parser::ParseError::LexErrorToken { .. }));
    assert!(saw_lex_err, "expected at least one LexErrorToken, got {errors:#?}");
}

#[test]
fn second_function_still_parses_after_error_in_first() {
    // Load-bearing recoverability: a syntax error in `bad` must not prevent
    // `good` from parsing.
    let tokens = lex("fn bad() { let x = ; } fn good() { 1 }", FID);
    let (module, errors) = parse(&tokens, FID);
    assert!(!errors.is_empty(), "expected parse errors for `bad`");
    let names: Vec<&str> = module
        .root_items
        .iter()
        .filter_map(|id| match &module.items[*id].kind {
            oxide::parser::ItemKind::Fn(f) => Some(f.name.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"good"), "expected `good` to parse, got {names:?}");
}

#[test]
fn non_c_abi_is_a_parse_error() {
    let tokens = lex(r#"extern "Rust" { fn f(); }"#, FID);
    let (_, errors) = parse(&tokens, FID);
    assert!(!errors.is_empty(), "expected parse error for non-C ABI");
}

#[test]
fn bodyless_fn_outside_extern_block_is_a_parse_error() {
    let tokens = lex("fn f();", FID);
    let (_, errors) = parse(&tokens, FID);
    assert!(
        !errors.is_empty(),
        "bodyless fn outside extern block must not parse"
    );
}

#[test]
fn pointer_without_mutability_is_parse_error() {
    let tokens = lex("fn f(s: *u8) {}", FID);
    let (_, errors) = parse(&tokens, FID);
    assert!(
        !errors.is_empty(),
        "`*u8` without const/mut must not parse"
    );
}
