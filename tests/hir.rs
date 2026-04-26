//! Structural / error-shape unit tests for HIR lowering.
//!
//! Behavioral / pretty-print cases live as `.ox` + `.snap` pairs under
//! `tests/snapshots/hir/` and are exercised by `tests/hir_snapshot.rs`.
//! The tests here check things that rendered output cannot — `HirError`
//! variants, specific `HirExprKind` / `HirTyKind` destructuring, and
//! literal-payload identity.

use oxide::hir::{HirError, HirExprKind, HirModule, HirTyKind, lower};
use oxide::lexer::lex;
use oxide::parser::parse;

fn lower_src(src: &str) -> (HirModule, Vec<HirError>) {
    let tokens = lex(src);
    let (ast, parse_errs) = parse(&tokens);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:#?}");
    lower(&ast)
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
fn if_else_branches_resolve_to_correct_locals() {
    // Companion to the `if_else_resolves_in_each_branch` snapshot — the
    // snapshot covers the rendered shape; this asserts the structural
    // invariants the snapshot can't see (`has_semi == false` for the
    // value-producing item, exact local indices in each branch).
    let tokens = lex("fn f(c: bool, a: i32, b: i32) -> i32 { if c { a } else { b } }");
    let (ast, _) = parse(&tokens);
    let (hir, _) = lower(&ast);
    let f = &hir.fns[hir.root_fns[0]];
    let body = &hir.blocks[f.body.expect("local fn has body")];
    let if_item = body.items.first().expect("if as item");
    assert!(!if_item.has_semi, "if/else without `;` should be the value-producing item");
    let HirExprKind::If { cond, then_block, else_arm } = &hir.exprs[if_item.expr].kind else {
        panic!("first item should be If");
    };
    assert!(matches!(&hir.exprs[*cond].kind, HirExprKind::Local(lid) if lid.raw() == 0));
    let then_b = &hir.blocks[*then_block];
    let then_last = then_b.items.last().expect("then-arm last item");
    assert!(!then_last.has_semi);
    assert!(matches!(&hir.exprs[then_last.expr].kind, HirExprKind::Local(lid) if lid.raw() == 1));
    let Some(oxide::hir::HElseArm::Block(else_bid)) = else_arm else {
        panic!("else should be a block");
    };
    let else_b = &hir.blocks[*else_bid];
    let else_last = else_b.items.last().expect("else-arm last item");
    assert!(!else_last.has_semi);
    assert!(matches!(&hir.exprs[else_last.expr].kind, HirExprKind::Local(lid) if lid.raw() == 2));
}

#[test]
fn extern_block_lowers_to_bodyless_hir_fn() {
    let (hir, errs) = lower_src(r#"extern "C" { fn print_int(x: i32) -> i32; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    assert_eq!(hir.fns.len(), 1);
    let f = &hir.fns[hir.root_fns[0]];
    assert_eq!(f.name, "print_int");
    assert!(f.is_extern, "extern block child must have is_extern = true");
    assert!(f.body.is_none(), "extern fn must have no body");
    assert_eq!(f.params.len(), 1);
    assert!(matches!(&f.ret_ty.as_ref().unwrap().kind, HirTyKind::Named(s) if s == "i32"));
}

#[test]
fn extern_fn_call_resolves_to_fn() {
    let (hir, errs) = lower_src(
        r#"extern "C" { fn print_int(x: i32) -> i32; }
           fn main() -> i32 { print_int(42); 0 }"#,
    );
    assert!(errs.is_empty(), "{errs:#?}");
    let main_fid = hir.root_fns.iter().find(|&&fid| hir.fns[fid].name == "main").copied().expect("main fn");
    let main = &hir.fns[main_fid];
    let body_id = main.body.expect("main has body");
    let body = &hir.blocks[body_id];
    let call_item = body.items.first().expect("call as first item");
    let HirExprKind::Call { callee, .. } = &hir.exprs[call_item.expr].kind else {
        panic!("first item should be a Call");
    };
    let HirExprKind::Fn(callee_fid) = &hir.exprs[*callee].kind else {
        panic!("callee should resolve to Fn");
    };
    assert!(hir.fns[*callee_fid].is_extern, "callee should be extern fn");
}

#[test]
fn local_fn_marks_is_extern_false() {
    let (hir, _) = lower_src("fn add(a: i32, b: i32) -> i32 { a + b }");
    let f = &hir.fns[hir.root_fns[0]];
    assert!(!f.is_extern);
    assert!(f.body.is_some());
}

#[test]
fn pointer_type_lowers_with_mutability() {
    use oxide::parser::ast::Mutability;
    let (hir, errs) = lower_src(r#"extern "C" { fn puts(s: *const u8) -> i32; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    let f = &hir.fns[hir.root_fns[0]];
    let param = &hir.locals[f.params[0]];
    let HirTyKind::Ptr { mutability, pointee } = &param.ty.as_ref().unwrap().kind else {
        panic!("expected Ptr type");
    };
    assert_eq!(*mutability, Mutability::Const);
    assert!(matches!(&pointee.kind, HirTyKind::Named(s) if s == "u8"));
}

#[test]
fn nested_pointer_type_preserves_each_layer() {
    use oxide::parser::ast::Mutability;
    let (hir, errs) = lower_src(r#"extern "C" { fn f(s: *const *mut u8); }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    let f = &hir.fns[hir.root_fns[0]];
    let param = &hir.locals[f.params[0]];
    let HirTyKind::Ptr { mutability, pointee } = &param.ty.as_ref().unwrap().kind else {
        panic!("expected Ptr at outer layer");
    };
    assert_eq!(*mutability, Mutability::Const);
    let HirTyKind::Ptr { mutability: inner_mut, pointee: inner_pointee } = &pointee.kind else {
        panic!("expected Ptr at inner layer");
    };
    assert_eq!(*inner_mut, Mutability::Mut);
    assert!(matches!(&inner_pointee.kind, HirTyKind::Named(s) if s == "u8"));
}

#[test]
fn string_literal_lowers_through() {
    // The HIR keeps the source string verbatim — the `\0` terminator is
    // appended only at codegen time (spec/07_POINTER.md, point 4).
    let (hir, errs) = lower_src(r#"fn f() { let s = "hello"; }"#);
    assert!(errs.is_empty(), "{errs:#?}");
    let f = &hir.fns[hir.root_fns[0]];
    let body = &hir.blocks[f.body.unwrap()];
    let let_item = &body.items[0];
    assert!(let_item.has_semi, "let always carries `;`");
    let HirExprKind::Let { init: Some(init), .. } = &hir.exprs[let_item.expr].kind else {
        panic!("expected Let with init");
    };
    let HirExprKind::StrLit(s) = &hir.exprs[*init].kind else {
        panic!("expected StrLit");
    };
    assert_eq!(s, "hello");
    assert_eq!(s.len(), 5, "HIR payload must be source length, no \\0");
}
