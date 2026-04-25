use inkwell::context::Context;

use oxide::codegen::codegen;
use oxide::hir::lower as hir_lower;
use oxide::lexer::lex;
use oxide::parser::parse;
use oxide::typeck::check;

/// Compile a source string through the full pipeline and hand back the
/// LLVM IR as a string.
fn compile_to_ir(src: &str) -> String {
    let ctx = Context::create();
    let tokens = lex(src);
    let (ast, parse_errs) = parse(&tokens);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:#?}");
    let (hir, hir_errs) = hir_lower(&ast);
    assert!(hir_errs.is_empty(), "hir errors: {hir_errs:#?}");
    let (results, type_errs) = check(&hir);
    assert!(type_errs.is_empty(), "type errors: {type_errs:#?}");
    let module = codegen(&ctx, &hir, &results, "test");
    module.print_to_string().to_string()
}

fn assert_contains(ir: &str, needle: &str) {
    assert!(
        ir.contains(needle),
        "expected IR to contain `{needle}`\nfull IR:\n{ir}"
    );
}

#[test]
fn acceptance_fn_add() {
    let ir = compile_to_ir("fn add(a: i32, b: i32) -> i32 { a + b }");
    assert_contains(&ir, "define i32 @add(i32 %a, i32 %b)");
    assert_contains(&ir, "alloca i32");
    assert_contains(&ir, "store i32 %a");
    assert_contains(&ir, "store i32 %b");
    assert_contains(&ir, "= add i32");
    assert_contains(&ir, "ret i32");
}

#[test]
fn unsigned_div_uses_udiv() {
    let ir = compile_to_ir("fn f(a: u32, b: u32) -> u32 { a / b }");
    assert_contains(&ir, "udiv i32");
    assert!(!ir.contains("sdiv"), "should not emit sdiv for u32:\n{ir}");
}

#[test]
fn signed_div_uses_sdiv() {
    let ir = compile_to_ir("fn f(a: i32, b: i32) -> i32 { a / b }");
    assert_contains(&ir, "sdiv i32");
    assert!(!ir.contains("udiv"), "should not emit udiv for i32:\n{ir}");
}

#[test]
fn signed_compare_uses_slt() {
    let ir = compile_to_ir("fn f(a: i32, b: i32) -> bool { a < b }");
    assert_contains(&ir, "icmp slt");
}

#[test]
fn unsigned_compare_uses_ult() {
    let ir = compile_to_ir("fn f(a: u32, b: u32) -> bool { a < b }");
    assert_contains(&ir, "icmp ult");
}

#[test]
fn equality_uses_icmp_eq() {
    let ir = compile_to_ir("fn f(a: i32, b: i32) -> bool { a == b }");
    assert_contains(&ir, "icmp eq");
}

#[test]
fn unit_fn_emits_void_ret() {
    let ir = compile_to_ir("fn f() {}");
    assert_contains(&ir, "define void @f()");
    assert_contains(&ir, "ret void");
}

#[test]
fn return_emits_ret() {
    let ir = compile_to_ir("fn f() -> i32 { return 42; 0 }");
    assert_contains(&ir, "ret i32 42");
}

#[test]
fn forward_call_emits_call_inst() {
    let ir = compile_to_ir("fn a() -> i32 { b() } fn b() -> i32 { 1 }");
    assert_contains(&ir, "call i32 @b()");
}

#[test]
fn if_with_value_branches() {
    // The parser commits a tail-less `if` as a block item, so we
    // keep the `if` in expression-position via `let`.
    let ir = compile_to_ir(
        "fn f(c: bool) -> i32 { let r: i32 = if c { 1 } else { 2 }; r }",
    );
    assert_contains(&ir, "if.then");
    assert_contains(&ir, "if.else");
    assert_contains(&ir, "if.end");
    // The result slot is alloca'd in entry; both arms store into it,
    // merge loads.
    assert!(
        ir.matches("store i32").count() >= 2,
        "expected at least two stores for if arms:\n{ir}"
    );
}

#[test]
fn if_without_else_is_unit() {
    // No else, body type unit. No slot, just branches.
    let ir = compile_to_ir("fn f(c: bool) { if c { let x: i32 = 1; } }");
    assert_contains(&ir, "if.then");
    // No `if.val` load since there's no value.
    assert!(!ir.contains("if.val"), "expected no load for unit if:\n{ir}");
}

#[test]
fn let_emits_alloca_and_store() {
    let ir = compile_to_ir("fn f() -> i32 { let x: i32 = 7; x }");
    assert_contains(&ir, "x.0.slot");
    assert_contains(&ir, "store i32 7");
    assert_contains(&ir, "load i32");
}

#[test]
fn assign_emits_store() {
    // `let mut x = ...; x = 5; x`
    let ir = compile_to_ir("fn f() -> i32 { let mut x: i32 = 1; x = 5; x }");
    let stores = ir.matches("store i32").count();
    // one for the let init, one for the assignment.
    assert!(stores >= 2, "expected ≥2 stores, got {stores}:\n{ir}");
}

#[test]
fn compound_assign_loads_then_stores() {
    let ir = compile_to_ir("fn f() -> i32 { let mut x: i32 = 1; x += 2; x }");
    assert_contains(&ir, "asgn.add");
    assert_contains(&ir, "store i32");
}

#[test]
fn cast_widens_signed_with_sext() {
    let ir = compile_to_ir("fn f(x: i32) -> i64 { x as i64 }");
    assert_contains(&ir, "sext i32");
}

#[test]
fn cast_widens_unsigned_with_zext() {
    let ir = compile_to_ir("fn f(x: u32) -> u64 { x as u64 }");
    assert_contains(&ir, "zext i32");
}

#[test]
fn cast_narrows_with_trunc() {
    let ir = compile_to_ir("fn f(x: i64) -> i32 { x as i32 }");
    assert_contains(&ir, "trunc i64");
}

#[test]
fn logical_and_short_circuits_via_phi() {
    let ir = compile_to_ir("fn f(a: bool, b: bool) -> bool { a && b }");
    assert_contains(&ir, "logic.rhs");
    assert_contains(&ir, "logic.end");
    assert_contains(&ir, "phi i1");
}

#[test]
fn logical_or_short_circuits_via_phi() {
    let ir = compile_to_ir("fn f(a: bool, b: bool) -> bool { a || b }");
    assert_contains(&ir, "logic.rhs");
    assert_contains(&ir, "phi i1");
}

#[test]
fn shift_lhs_signed_uses_ashr() {
    let ir = compile_to_ir("fn f(a: i32, b: i32) -> i32 { a >> b }");
    assert_contains(&ir, "ashr i32");
}

#[test]
fn shift_lhs_unsigned_uses_lshr() {
    let ir = compile_to_ir("fn f(a: u32, b: u32) -> u32 { a >> b }");
    assert_contains(&ir, "lshr i32");
}

#[test]
fn never_type_in_let_does_not_break_verifier() {
    // `let b: i32 = return 1;` — rhs is divergent, so the surrounding
    // body's emission terminates after the `ret`. No verifier failure.
    let ir = compile_to_ir("fn f() -> i32 { let b: i32 = return 1; b }");
    assert_contains(&ir, "ret i32 1");
}

#[test]
fn neg_emits_int_neg() {
    let ir = compile_to_ir("fn f(x: i32) -> i32 { -x }");
    assert_contains(&ir, "sub i32 0");
}

#[test]
fn bitnot_emits_xor_with_all_ones() {
    let ir = compile_to_ir("fn f(x: i32) -> i32 { ~x }");
    assert_contains(&ir, "xor i32");
    assert_contains(&ir, "-1");
}

#[test]
fn logical_not_emits_xor_i1() {
    let ir = compile_to_ir("fn f(c: bool) -> bool { !c }");
    assert_contains(&ir, "xor i1");
}
