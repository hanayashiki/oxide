//! Structural unit tests for the type checker.
//!
//! Behavioral / end-to-end cases live as `.ox` + `.snap` pairs under
//! `tests/snapshots/typeck/` and are exercised by
//! `tests/typeck_snapshot.rs`. The tests here check things that
//! rendered output cannot — `TyId` interning, `TyKind` destructuring,
//! direct identity equalities.

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
fn type_interning_makes_equal_types_share_id() {
    // Two `i32` annotations resolve to the same TyId via the pre-interned
    // primitive table.
    let (r, _) = typeck("fn f(a: i32, b: i32) -> i32 { a }");
    let sig = &r.fn_sigs[oxide::hir::FnId::from_raw(0)];
    assert_eq!(sig.params[0], sig.params[1]);
    assert_eq!(sig.params[0], r.tys.i32);
}

#[test]
fn int_literals_default_to_i32() {
    let (r, errs) = typeck("fn f() -> i32 { 1 + 2 }");
    assert!(errs.is_empty(), "{errs:#?}");
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
fn extern_fn_signature_resolves() {
    let (r, errs) = typeck(r#"extern "C" { fn print_int(x: i32) -> i32; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    let sig = r.fn_sig(oxide::hir::FnId::from_raw(0));
    assert_eq!(sig.params.len(), 1);
    assert_eq!(sig.params[0], r.tys.i32);
    assert_eq!(sig.ret, r.tys.i32);
}

#[test]
fn string_literal_infers_to_const_u8_ptr_via_type() {
    use oxide::parser::ast::Mutability;
    let (r, errs) = typeck(r#"fn f() { let s = "hi"; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    let mut found = false;
    for ty in &r.expr_tys {
        if let TyKind::Ptr(pointee, m) = r.tys.kind(*ty) {
            if *pointee == r.tys.u8 && *m == Mutability::Const {
                found = true;
                break;
            }
        }
    }
    assert!(found, "expected at least one *const u8 type, got: {r:#?}");
}
