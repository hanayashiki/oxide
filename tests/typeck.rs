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
fn string_literal_infers_to_const_array_u8_ptr_via_type() {
    use oxide::parser::ast::Mutability;
    let (r, errs) = typeck(r#"fn f() { let s = "hi"; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    // `"hi"` is 2 bytes + trailing NUL = 3.
    let mut found = false;
    for ty in &r.expr_tys {
        if let TyKind::Ptr(pointee, Mutability::Const) = r.tys.kind(*ty) {
            if let TyKind::Array(elem, Some(3)) = r.tys.kind(*pointee) {
                if *elem == r.tys.u8 {
                    found = true;
                    break;
                }
            }
        }
    }
    assert!(
        found,
        "expected at least one *const [u8; 3] type, got: {r:#?}"
    );
}

#[test]
fn string_literal_passes_to_const_byte_seq_param() {
    // FFI flow: StrLit `*const [u8; N]` flows into a `*const [u8]`
    // parameter via the existing length-erasure coercion.
    let (_, errs) = typeck(
        r#"
            fn puts(s: *const [u8]) -> i32 { 0 }
            fn main() -> i32 { puts("hi"); 0 }
        "#,
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn string_literal_to_old_const_u8_param_errors() {
    // The pre-migration extern spelling no longer accepts a string
    // literal: `*const u8` strictly means "pointer to a single u8" now,
    // and there is no array-layer-drop decay.
    let (_, errs) = typeck(
        r#"
            fn puts(s: *const u8) -> i32 { 0 }
            fn main() -> i32 { puts("hi"); 0 }
        "#,
    );
    assert!(
        !errs.is_empty(),
        "expected an error: passing *const [u8; 3] to *const u8 must be rejected"
    );
}

#[test]
fn length_fabrication_in_arm_coalesce_errors() {
    // `unify_arms` calls `unify(then, else)`; with the gated rule,
    // None→Some is rejected even under pointee=true. The arm with the
    // unsized pointer in the THEN position can't be silently widened
    // into the SIZED expected slot.
    let (_, errs) = typeck(
        r#"
            fn unsized_ptr() -> *const [u8] { "hi" }
            fn main() -> i32 {
                let s = if true { unsized_ptr() } else { "hi" };
                0
            }
        "#,
    );
    assert!(
        !errs.is_empty(),
        "expected an error: None→Some arm coalesce must be rejected"
    );
}

#[test]
fn length_erasure_in_arm_coalesce_succeeds() {
    // The forward direction (Some→None) at arm coalesce is the
    // residual sloppy-subtyping path documented in spec/09. It still
    // typechecks because the legitimate length-erasure fires under
    // pointee=true. Result type is `*const [u8; 3]` (then-arm wins
    // via join_never).
    let (_, errs) = typeck(
        r#"
            fn unsized_ptr() -> *const [u8] { "hi" }
            fn main() -> i32 {
                let s = if true { "hi" } else { unsized_ptr() };
                0
            }
        "#,
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn mismatched_length_strs_in_if_arms_error() {
    // Both arms are `Some(_)` with different lengths — strict
    // ArrayLengthMismatch (existing E0265, surfaces now that StrLit
    // carries length info). Workaround for the user is to bind each
    // literal to a `*const [u8]` local first, but we don't exercise
    // the workaround in this test.
    let (_, errs) = typeck(r#"fn f() -> i32 { let s = if true { "hi" } else { "bye" }; 0 }"#);
    assert!(
        !errs.is_empty(),
        "expected an error: mixed-length string arms must be rejected"
    );
}
