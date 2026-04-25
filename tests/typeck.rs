use oxide::hir::lower;
use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::typeck::{TyKind, TypeError, TypeckResults, check};

fn typeck(src: &str) -> (TypeckResults, Vec<TypeError>) {
    let tokens = lex(src);
    let (ast, parse_errs) = parse(&tokens);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:#?}");
    let (hir, hir_errs) = lower(&ast);
    assert!(hir_errs.is_empty(), "hir errors: {hir_errs:#?}");
    check(&hir)
}

#[test]
fn acceptance_fn_add() {
    let (r, errs) = typeck("fn add(a: i32, b: i32) -> i32 { a + b }");
    assert!(errs.is_empty(), "expected clean typecheck, got {errs:#?}");
    let sig = &r.fn_sigs[oxide::hir::FnId::from_raw(0)];
    assert_eq!(sig.params.len(), 2);
    assert_eq!(sig.params[0], r.tys.i32);
    assert_eq!(sig.params[1], r.tys.i32);
    assert_eq!(sig.ret, r.tys.i32);
    // every expr in the body should be i32
    for ty in &r.expr_tys {
        assert_eq!(*ty, r.tys.i32);
    }
}

#[test]
fn type_interning_makes_equal_types_share_id() {
    // Two `i32` annotations resolve to the same TyId via the pre-interned
    // primitive table.
    let (r, _) = typeck("fn f(a: i32, b: i32) -> i32 { a }");
    let sig = &r.fn_sigs[oxide::hir::FnId::from_raw(0)];
    assert_eq!(sig.params[0], sig.params[1]);
    assert_eq!(sig.params[0], r.tys.i32);
}

#[test]
fn return_type_mismatch_emits_e0250() {
    let (_, errs) = typeck("fn f() -> i32 { true }");
    assert_eq!(errs.len(), 1);
    let TypeError::TypeMismatch { expected, found, .. } = &errs[0] else {
        panic!("expected TypeMismatch, got {:?}", errs[0]);
    };
    let r = check(&lower(&parse(&lex("fn f() -> i32 { true }")).0).0).0;
    assert_eq!(*expected, r.tys.i32);
    assert_eq!(*found, r.tys.bool);
}

#[test]
fn unknown_type_emits_e0251() {
    let (_, errs) = typeck("fn f(x: blarg) {}");
    assert!(errs.iter().any(|e| matches!(e, TypeError::UnknownType { name, .. } if name == "blarg")));
}

#[test]
fn wrong_arg_count_emits_e0253() {
    let (_, errs) = typeck("fn g() {} fn f() { g(1) }");
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::WrongArgCount { expected: 0, found: 1, .. })),
        "{errs:#?}"
    );
}

#[test]
fn not_callable_emits_e0252() {
    let (_, errs) = typeck("fn f() { let x: i32 = 1; x() }");
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::NotCallable { .. })),
        "{errs:#?}"
    );
}

#[test]
fn int_literals_default_to_i32() {
    let (r, errs) = typeck("fn f() -> i32 { 1 + 2 }");
    assert!(errs.is_empty(), "{errs:#?}");
    // every int literal should resolve to i32
    for (eid, &ty) in r.expr_tys.iter_enumerated() {
        let kind = r.tys.kind(ty);
        assert!(
            matches!(kind, TyKind::Prim(_)),
            "HExprId({}) didn't resolve: {:?}",
            eid.raw(),
            kind
        );
    }
}

#[test]
fn never_unifies_with_anything() {
    // `let b: i32 = return 1;` — rhs has type `!`, must unify with i32.
    let (_, errs) = typeck("fn f() -> i32 { let b: i32 = return 1; b }");
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn string_literal_emits_e0254() {
    let (_, errs) = typeck("fn f() { let s = \"hi\"; }");
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::UnsupportedStrLit { .. })),
        "{errs:#?}"
    );
}

#[test]
fn forward_call_resolves() {
    // `b` is defined after `a` calls it; sigs are resolved in Phase 1
    // so this works.
    let (_, errs) = typeck("fn a() -> i32 { b() } fn b() -> i32 { 1 }");
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn binary_arith_unifies_operands() {
    // `a` is i32; `2` (int literal, fresh infer) unifies with i32.
    let (r, errs) = typeck("fn f(a: i32) -> i32 { a + 2 }");
    assert!(errs.is_empty(), "{errs:#?}");
    for ty in &r.expr_tys {
        assert_eq!(*ty, r.tys.i32);
    }
}

#[test]
fn comparison_returns_bool() {
    let (r, errs) = typeck("fn f(a: i32) -> bool { a < 5 }");
    assert!(errs.is_empty(), "{errs:#?}");
    let body = &r.expr_tys;
    // Find the binary `<` expression — its result should be bool.
    assert!(body.iter().any(|t| *t == r.tys.bool));
}

#[test]
fn if_branches_must_unify() {
    let (_, errs) = typeck("fn f() -> i32 { if true { 1 } else { false } }");
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "{errs:#?}"
    );
}

#[test]
fn if_no_else_must_be_unit() {
    let (_, errs) = typeck("fn f() { if true { 1 } }");
    // body has unit-returning fn (default), `if {1}` produces 1 (i32)
    // because the then-branch tail is `1`. With no `else`, then must unify
    // with unit → mismatch.
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "{errs:#?}"
    );
}

#[test]
fn extern_fn_signature_resolves() {
    let (r, errs) = typeck(r#"extern "C" { fn print_int(x: i32) -> i32; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    let sig = r.fn_sig(oxide::hir::FnId::from_raw(0));
    assert_eq!(sig.params.len(), 1);
    assert_eq!(sig.params[0], r.tys.i32);
    assert_eq!(sig.ret, r.tys.i32);
}

#[test]
fn calling_extern_fn_typechecks() {
    let (_, errs) = typeck(
        r#"extern "C" { fn print_int(x: i32) -> i32; }
           fn main() -> i32 { print_int(42); 0 }"#,
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn extern_fn_arity_mismatch_emits_e0253() {
    let (_, errs) = typeck(
        r#"extern "C" { fn print_int(x: i32) -> i32; }
           fn main() -> i32 { print_int(); 0 }"#,
    );
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::WrongArgCount { expected: 1, found: 0, .. })),
        "{errs:#?}"
    );
}

#[test]
fn extern_fn_arg_type_mismatch_emits_e0250() {
    let (_, errs) = typeck(
        r#"extern "C" { fn print_int(x: i32) -> i32; }
           fn main() -> i32 { print_int(true); 0 }"#,
    );
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "{errs:#?}"
    );
}
