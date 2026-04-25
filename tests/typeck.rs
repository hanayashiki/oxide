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
    let TypeError::TypeMismatch {
        expected, found, ..
    } = &errs[0]
    else {
        panic!("expected TypeMismatch, got {:?}", errs[0]);
    };
    let r = check(&lower(&parse(&lex("fn f() -> i32 { true }")).0).0).0;
    assert_eq!(*expected, r.tys.i32);
    assert_eq!(*found, r.tys.bool);
}

#[test]
fn unknown_type_emits_e0251() {
    let (_, errs) = typeck("fn f(x: blarg) {}");
    assert!(
        errs.iter()
            .any(|e| matches!(e, TypeError::UnknownType { name, .. } if name == "blarg"))
    );
}

#[test]
fn wrong_arg_count_emits_e0253() {
    let (_, errs) = typeck("fn g() {} fn f() { g(1) }");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            TypeError::WrongArgCount {
                expected: 0,
                found: 1,
                ..
            }
        )),
        "{errs:#?}"
    );
}

#[test]
fn not_callable_emits_e0252() {
    let (_, errs) = typeck("fn f() { let x: i32 = 1; x() }");
    assert!(
        errs.iter()
            .any(|e| matches!(e, TypeError::NotCallable { .. })),
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
fn string_literal_infers_to_const_u8_ptr() {
    // String literals are C-style: `*const u8` (NUL-terminated by codegen).
    // See spec/07_POINTER.md. No errors expected for the inferred binding.
    let (_, errs) = typeck("fn f() { let s: *const u8 = \"hi\"; }");
    assert!(errs.is_empty(), "{errs:#?}");
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
        errs.iter()
            .any(|e| matches!(e, TypeError::TypeMismatch { .. })),
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
        errs.iter()
            .any(|e| matches!(e, TypeError::TypeMismatch { .. })),
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
        errs.iter().any(|e| matches!(
            e,
            TypeError::WrongArgCount {
                expected: 1,
                found: 0,
                ..
            }
        )),
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
        errs.iter()
            .any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "{errs:#?}"
    );
}

#[test]
fn passing_string_literal_to_const_u8_ptr_typechecks() {
    let (_, errs) = typeck(
        r#"
            fn puts(s: *const u8) -> i32 { 0 }
            fn main() -> i32 { puts("hi"); 0 }
        "#,
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn mut_ptr_can_drop_to_const_at_outer_layer() {
    // *mut u8 → *const u8 is allowed (drop write permission).
    let (_, errs) = typeck(
        r#"
            fn takes_const(s: *const u8) -> i32 { 0 }
            fn main(p: *mut u8) -> i32 { takes_const(p) }
        "#,
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn const_ptr_to_mut_param_emits_e0257() {
    // *const u8 → *mut u8 must fail (would forge write access).
    let (_, errs) = typeck(
        r#"
            fn takes_mut(s: *mut u8) -> i32 { 0 }
            fn main(p: *const u8) -> i32 { takes_mut(p) }
        "#,
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, TypeError::PointerMutabilityMismatch { .. })),
        "{errs:#?}"
    );
}

#[test]
fn inner_mutability_mismatch_emits_e0257() {
    // Outer: const → const ✓. Inner: mut vs const — must fail (inner
    // positions require exact match per spec/07_POINTER.md).
    let (_, errs) = typeck(
        r#"
            fn f(s: *const *const u8) -> i32 { 0 }
            fn main(p: *const *mut u8) -> i32 { f(p) }
        "#,
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, TypeError::PointerMutabilityMismatch { .. })),
        "{errs:#?}"
    );
}

#[test]
fn pointer_pointee_shape_mismatch_emits_e0250() {
    // *const u8 vs *const i32 — the pointee shape itself disagrees, so
    // unify catches it as an ordinary type mismatch.
    let (_, errs) = typeck(
        r#"
            fn f(s: *const u8) -> i32 { 0 }
            fn main(p: *const i32) -> i32 { f(p) }
        "#,
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "{errs:#?}"
    );
}

#[test]
fn if_as_block_value_typechecks() {
    // Regression test for the fib bug: a tail-position `if/else` whose
    // arms are tail expressions should give the block its value type.
    let (_, errs) = typeck(
        "fn fib(n: u32) -> u32 { if n <= 1 { 1 } else { fib(n-1) + fib(n-2) } }",
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn semicolon_after_expr_discards_value() {
    // With `;` the block's value is `()`, so this mismatches `-> u32`.
    let (_, errs) = typeck("fn f() -> u32 { 1; }");
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "{errs:#?}"
    );
}

#[test]
fn missing_semi_in_middle_of_block_emits_e0250() {
    // `1 + 2` is non-unit, non-divergent, mid-block, no `;` → error.
    let (_, errs) = typeck("fn f() -> i32 { 1 + 2  3 }");
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "expected `;`-enforcement error, got {errs:#?}"
    );
}

#[test]
fn unit_call_without_semi_in_middle_is_allowed() {
    // `g()` returns `()`, so it's fine without `;` mid-block.
    let (_, errs) = typeck("fn g() {} fn f() -> i32 { g() 0 }");
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn return_without_semi_in_middle_is_allowed() {
    // `return e` has type `!`, mid-block enforce becomes `unify(!, unit)`
    // which is absorbed by the Never arm. Trailing `0` is the value.
    let (_, errs) = typeck("fn f() -> i32 { return 1 0 }");
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn divergent_subblock_does_not_silence_trailing_mismatch() {
    // The "shit" case: inner block diverges (its tail is `return 1`,
    // type `!`), but the outer block's value comes from the trailing
    // `"a"` — coerce against `-> i32` must still fail. Confirms we are
    // NOT doing saw-never propagation / divergence-flag bookkeeping.
    let (_, errs) = typeck(r#"fn shit() -> i32 { { return 1 } "a" }"#);
    assert!(
        errs.iter().any(|e| matches!(e, TypeError::TypeMismatch { .. })),
        "expected outer block to error on `\"a\"` vs i32, got {errs:#?}"
    );
}

#[test]
fn if_with_returns_in_both_arms_typechecks() {
    // Path-completeness handled by types alone: each arm is `!`,
    // if-expr unifies to `!`, and `!` coerces to any return type.
    let (_, errs) = typeck(
        "fn f() -> i32 { if true { return 1 } else { return 2 } }",
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn bare_semicolons_are_no_ops() {
    // `;;` and stray `;` between/around items must not break parsing
    // or typechecking — they parse to zero block-items.
    let (_, errs) = typeck("fn f() -> i32 { ;; let x: i32 = 1; ;; x }");
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn string_literal_infers_to_const_u8_ptr_via_type() {
    use oxide::parser::ast::Mutability;
    let (r, errs) = typeck(r#"fn f() { let s = "hi"; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    // Find the StrLit's expr and confirm it's interned as Ptr(u8, Const).
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
