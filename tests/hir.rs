use expect_test::{Expect, expect};

use oxide::hir::{HirError, HirExprKind, HirModule, HirTyKind, lower, pretty::pretty_print};
use oxide::lexer::lex;
use oxide::parser::parse;

fn lower_src(src: &str) -> (HirModule, Vec<HirError>) {
    let tokens = lex(src);
    let (ast, parse_errs) = parse(&tokens);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:#?}");
    lower(&ast)
}

fn check(src: &str, expected: Expect) {
    let (hir, errors) = lower_src(src);
    let mut out = pretty_print(&hir);
    if !errors.is_empty() {
        out.push_str("--- errors ---\n");
        for e in &errors {
            out.push_str(&format!("{e:?}\n"));
        }
    }
    expected.assert_eq(&out);
}

#[test]
fn worked_example_resolves_params() {
    check(
        "fn add(a: i32, b: i32) { a + b }",
        expect![[r#"
            HirModule
              Fn[0] add(a[Local(0)]: i32, b[Local(1)]: i32)
                Block
                  tail: Binary(Add, Local(0, "a"), Local(1, "b"))
        "#]],
    );
}

#[test]
fn empty_fn_lowers_to_empty_block() {
    check(
        "fn f() {}",
        expect![[r#"
            HirModule
              Fn[0] f()
                Block
        "#]],
    );
}

#[test]
fn return_type_is_passed_through_as_named() {
    check(
        "fn f() -> i32 { 0 }",
        expect![[r#"
            HirModule
              Fn[0] f() -> i32
                Block
                  tail: Int(0)
        "#]],
    );
}

#[test]
fn block_scoping_shadows_then_restores() {
    // Outer `x` is Local(0); inner `x` is Local(1); after inner block exits,
    // the use of `x` resolves back to the outer Local(0).
    check(
        "fn f() { let x = 1; { let x = 2; } x; }",
        expect![[r#"
            HirModule
              Fn[0] f()
                Block
                  Let x[Local(0)] = Int(1)
                  Block
                    Let x[Local(1)] = Int(2)
                  ExprStmt Local(0, "x")
        "#]],
    );
}

#[test]
fn forward_fn_reference_resolves() {
    // `a` calls `b` which is defined after it; HIR resolves it via the
    // module-level fn prescan.
    check(
        "fn a() { b() } fn b() {}",
        expect![[r#"
            HirModule
              Fn[0] a()
                Block
                  tail: Fn(1, "b")()
              Fn[1] b()
                Block
        "#]],
    );
}

#[test]
fn unresolved_name_emits_e0201() {
    let (_, errors) = lower_src("fn f() { undefined }");
    assert_eq!(errors.len(), 1);
    let HirError::UnresolvedName { name, .. } = &errors[0] else {
        panic!("expected UnresolvedName, got {:?}", errors[0]);
    };
    assert_eq!(name, "undefined");
}

#[test]
fn let_x_eq_x_does_not_see_new_binding() {
    // `let x = x;` — the rhs `x` must NOT resolve to the binding being
    // introduced. Here we have an outer `x = Local(0)`; the rhs in the
    // shadowing `let` should resolve to that outer x.
    check(
        "fn f() { let x: i32 = 1; let x = x; }",
        expect![[r#"
            HirModule
              Fn[0] f()
                Block
                  Let x[Local(0)]: i32 = Int(1)
                  Let x[Local(1)] = Local(0, "x")
        "#]],
    );
}

#[test]
fn duplicate_fn_emits_e0202() {
    let (_, errors) = lower_src("fn dup() {} fn dup() {}");
    assert_eq!(errors.len(), 1);
    let HirError::DuplicateFn { name, .. } = &errors[0] else {
        panic!("expected DuplicateFn, got {:?}", errors[0]);
    };
    assert_eq!(name, "dup");
}

#[test]
fn type_names_pass_through_untouched() {
    // HIR doesn't know what's a primitive vs a user-defined type.
    // Both `i32` and `blarg` lower to `HirTyKind::Named(...)`.
    let (hir, errors) = lower_src("fn f(x: i32) -> blarg { 0 }");
    assert!(errors.is_empty(), "HIR shouldn't error on unknown types: {errors:#?}");
    let f = &hir.fns[hir.root_fns[0]];
    let i32_ty = hir.locals[f.params[0]].ty.as_ref().unwrap();
    let blarg_ty = f.ret_ty.as_ref().unwrap();
    assert!(matches!(&i32_ty.kind, HirTyKind::Named(n) if n == "i32"));
    assert!(matches!(&blarg_ty.kind, HirTyKind::Named(n) if n == "blarg"));
}

#[test]
fn paren_is_dropped_in_lowering() {
    // `(a + b)` should lower the same as `a + b` — Paren wrapper is gone.
    check(
        "fn f(a: i32, b: i32) { (a + b) }",
        expect![[r#"
            HirModule
              Fn[0] f(a[Local(0)]: i32, b[Local(1)]: i32)
                Block
                  tail: Binary(Add, Local(0, "a"), Local(1, "b"))
        "#]],
    );
}

#[test]
fn return_lowers_as_expression() {
    // `return e` is an expression in HIR — `let b: i32 = return 1;` is
    // well-formed (typeck will give it type `!`).
    check(
        "fn f() -> i32 { let b: i32 = return 1; b }",
        expect![[r#"
            HirModule
              Fn[0] f() -> i32
                Block
                  Let b[Local(0)]: i32 = Return Int(1)
                  tail: Local(0, "b")
        "#]],
    );
}

#[test]
fn if_else_resolves_in_each_branch() {
    // `if … {} else {}` parses greedily as a block item (no trailing `;`
    // required), so it ends up in `block.items[0]` rather than `block.tail`.
    check(
        "fn f(c: bool, a: i32, b: i32) -> i32 { if c { a } else { b } }",
        expect![[r#"
            HirModule
              Fn[0] f(c[Local(0)]: bool, a[Local(1)]: i32, b[Local(2)]: i32) -> i32
                Block
                  If Local(0, "c")
                    then:
                      Block
                        tail: Local(1, "a")
                    else:
                      Block
                        tail: Local(2, "b")
        "#]],
    );

    // Sanity: assert the If's branches reference the right param locals.
    let tokens = lex("fn f(c: bool, a: i32, b: i32) -> i32 { if c { a } else { b } }");
    let (ast, _) = parse(&tokens);
    let (hir, _) = lower(&ast);
    let f = &hir.fns[hir.root_fns[0]];
    let body = &hir.blocks[f.body];
    let if_id = *body.items.first().expect("if as item");
    let HirExprKind::If { cond, then_block, else_arm } = &hir.exprs[if_id].kind else {
        panic!("first item should be If");
    };
    assert!(matches!(&hir.exprs[*cond].kind, HirExprKind::Local(lid) if lid.raw() == 0));
    let then_b = &hir.blocks[*then_block];
    let then_tail = then_b.tail.expect("then-tail expected");
    assert!(matches!(&hir.exprs[then_tail].kind, HirExprKind::Local(lid) if lid.raw() == 1));
    let Some(oxide::hir::HElseArm::Block(else_bid)) = else_arm else {
        panic!("else should be a block");
    };
    let else_b = &hir.blocks[*else_bid];
    let else_tail = else_b.tail.expect("else-tail expected");
    assert!(matches!(&hir.exprs[else_tail].kind, HirExprKind::Local(lid) if lid.raw() == 2));
}
