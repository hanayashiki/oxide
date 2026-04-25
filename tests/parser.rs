use expect_test::{Expect, expect};

use oxide::lexer::lex;
use oxide::parser::{parse, pretty::pretty_print};

fn check(src: &str, expected: Expect) {
    let tokens = lex(src);
    let (module, errors) = parse(&tokens);
    let mut out = pretty_print(&module);
    if !errors.is_empty() {
        out.push_str("--- errors ---\n");
        for e in &errors {
            out.push_str(&format!("{:?}\n", e));
        }
    }
    expected.assert_eq(&out);
}

#[test]
fn worked_example_simplest_fn() {
    check(
        "fn add(a: i32, b: i32) { a + b }",
        expect![[r#"
            Module
              Fn add(a: i32, b: i32)
                Block
                  tail: Binary(Add, Ident("a"), Ident("b"))
        "#]],
    );
}

#[test]
fn empty_input_yields_empty_module() {
    check("", expect!["Module\n"]);
}

#[test]
fn empty_function_body() {
    check(
        "fn f() {}",
        expect![[r#"
            Module
              Fn f()
                Block
        "#]],
    );
}

#[test]
fn return_type_arrow() {
    check(
        "fn f() -> bool { true }",
        expect![[r#"
            Module
              Fn f() -> bool
                Block
                  tail: Bool(true)
        "#]],
    );
}

#[test]
fn let_with_type_and_init() {
    check(
        "fn f() { let mut x: i32 = 1 + 2; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  Let mut x: i32 = Binary(Add, Int(1), Int(2))
        "#]],
    );
}

#[test]
fn precedence_mul_binds_tighter_than_add() {
    check(
        "fn f() { 1 + 2 * 3; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  ExprStmt Binary(Add, Int(1), Binary(Mul, Int(2), Int(3)))
        "#]],
    );
}

#[test]
fn assignment_is_right_associative() {
    check(
        "fn f() { x = y = 1; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  ExprStmt Assign(Eq, Ident("x"), Assign(Eq, Ident("y"), Int(1)))
        "#]],
    );
}

#[test]
fn as_cast_chains_left() {
    check(
        "fn f() { x as i64 as i32; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  ExprStmt Ident("x") as i64 as i32
        "#]],
    );
}

#[test]
fn postfix_call_index_field() {
    check(
        "fn f() { g(1, 2)[0].name; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  ExprStmt Ident("g")(Int(1), Int(2))[Int(0)].name
        "#]],
    );
}

#[test]
fn if_else_as_statement_with_return() {
    check(
        "fn f() -> i32 { if x > 0 { return x; } else { return 0; } }",
        expect![[r#"
            Module
              Fn f() -> i32
                Block
                  tail:
                    If Binary(Gt, Ident("x"), Int(0))
                      then:
                        Block
                          Return Ident("x")
                      else:
                        Block
                          Return Int(0)
        "#]],
    );
}

#[test]
fn else_if_chain_as_statement() {
    check(
        "fn f() { if a { 1 } else if b { 2 } else { 3 } }",
        expect![[r#"
            Module
              Fn f()
                Block
                  tail:
                    If Ident("a")
                      then:
                        Block
                          tail: Int(1)
                      else:
                        If Ident("b")
                          then:
                            Block
                              tail: Int(2)
                          else:
                            Block
                              tail: Int(3)
        "#]],
    );
}

#[test]
fn unary_prefixes() {
    check(
        "fn f() { -!~x; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  ExprStmt Unary(Neg, Unary(Not, Unary(BitNot, Ident("x"))))
        "#]],
    );
}

#[test]
fn paren_grouping_overrides_precedence() {
    check(
        "fn f() { (1 + 2) * 3; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  ExprStmt Binary(Mul, (Binary(Add, Int(1), Int(2))), Int(3))
        "#]],
    );
}

#[test]
fn reserved_keyword_match_yields_e0104() {
    let tokens = lex("fn f() { match x { } }");
    let (_, errors) = parse(&tokens);
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
    let tokens = lex("fn f() { let x = '\\q'; }");
    let (_, errors) = parse(&tokens);
    let saw_lex_err = errors.iter().any(|e| matches!(e, oxide::parser::ParseError::LexErrorToken { .. }));
    assert!(saw_lex_err, "expected at least one LexErrorToken, got {errors:#?}");
}

#[test]
fn return_in_let_init() {
    // `return e` is an expression of type `!`, so `let b: i32 = return 1;`
    // is well-typed (the binding never executes). This test exercises the
    // design intent: `return` parses in any expression position.
    check(
        "fn f() -> i32 { let b: i32 = return 1; b }",
        expect![[r#"
            Module
              Fn f() -> i32
                Block
                  Let b: i32 = Return Int(1)
                  tail: Ident("b")
        "#]],
    );
}

#[test]
fn second_function_still_parses_after_error_in_first() {
    // Load-bearing recoverability: a syntax error in `bad` must not prevent
    // `good` from parsing.
    let tokens = lex("fn bad() { let x = ; } fn good() { 1 }");
    let (module, errors) = parse(&tokens);
    assert!(!errors.is_empty(), "expected parse errors for `bad`");
    let names: Vec<&str> = module
        .root_items
        .iter()
        .filter_map(|id| match &module.items[*id].kind {
            oxide::parser::ItemKind::Fn(f) => Some(f.name.name.as_str()),
            oxide::parser::ItemKind::ExternBlock(_) => None,
        })
        .collect();
    assert!(names.contains(&"good"), "expected `good` to parse, got {names:?}");
}

#[test]
fn extern_block_with_one_fn() {
    check(
        r#"extern "C" { fn print_int(x: i32) -> i32; }"#,
        expect![[r#"
            Module
              ExternBlock "C"
                Fn print_int(x: i32) -> i32;
        "#]],
    );
}

#[test]
fn extern_block_with_multiple_fns() {
    check(
        r#"extern "C" { fn a(x: i32) -> i32; fn b() -> i32; fn c(); }"#,
        expect![[r#"
            Module
              ExternBlock "C"
                Fn a(x: i32) -> i32;
                Fn b() -> i32;
                Fn c();
        "#]],
    );
}

#[test]
fn extern_block_alongside_local_fn() {
    check(
        r#"extern "C" { fn print_int(x: i32) -> i32; } fn main() -> i32 { 0 }"#,
        expect![[r#"
            Module
              ExternBlock "C"
                Fn print_int(x: i32) -> i32;
              Fn main() -> i32
                Block
                  tail: Int(0)
        "#]],
    );
}

#[test]
fn non_c_abi_is_a_parse_error() {
    let tokens = lex(r#"extern "Rust" { fn f(); }"#);
    let (_, errors) = parse(&tokens);
    assert!(!errors.is_empty(), "expected parse error for non-C ABI");
}

#[test]
fn bodyless_fn_outside_extern_block_is_a_parse_error() {
    let tokens = lex("fn f();");
    let (_, errors) = parse(&tokens);
    assert!(
        !errors.is_empty(),
        "bodyless fn outside extern block must not parse"
    );
}

#[test]
fn pointer_type_const() {
    check(
        "fn f(s: *const u8) {}",
        expect![[r#"
            Module
              Fn f(s: *const u8)
                Block
        "#]],
    );
}

#[test]
fn pointer_type_mut() {
    check(
        "fn f(p: *mut i32) {}",
        expect![[r#"
            Module
              Fn f(p: *mut i32)
                Block
        "#]],
    );
}

#[test]
fn pointer_type_nested() {
    check(
        "fn f(s: *const *const *mut u8) {}",
        expect![[r#"
            Module
              Fn f(s: *const *const *mut u8)
                Block
        "#]],
    );
}

#[test]
fn string_literal_in_call_position() {
    check(
        r#"fn f() { puts("hi"); }"#,
        expect![[r#"
            Module
              Fn f()
                Block
                  ExprStmt Ident("puts")(Str("hi"))
        "#]],
    );
}

#[test]
fn pointer_without_mutability_is_parse_error() {
    let tokens = lex("fn f(s: *u8) {}");
    let (_, errors) = parse(&tokens);
    assert!(
        !errors.is_empty(),
        "`*u8` without const/mut must not parse"
    );
}

#[test]
fn last_expr_without_semi_is_block_value() {
    check(
        "fn f() -> i32 { 1 + 2 }",
        expect![[r#"
            Module
              Fn f() -> i32
                Block
                  tail: Binary(Add, Int(1), Int(2))
        "#]],
    );
}

#[test]
fn bare_semicolons_parse_to_no_block_items() {
    check(
        "fn f() { ;; let x: i32 = 1; ;; }",
        expect![[r#"
            Module
              Fn f()
                Block
                  Let x: i32 = Int(1)
        "#]],
    );
}

#[test]
fn if_as_tail_renders_with_full_structure() {
    // Regression for the fib bug: tail-position if/else now carries the
    // block's value type. Pretty printer should render it under `tail:`.
    check(
        "fn fib(n: u32) -> u32 { if n <= 1 { 1 } else { 0 } }",
        expect![[r#"
            Module
              Fn fib(n: u32) -> u32
                Block
                  tail:
                    If Binary(Le, Ident("n"), Int(1))
                      then:
                        Block
                          tail: Int(1)
                      else:
                        Block
                          tail: Int(0)
        "#]],
    );
}
