//! Drift guard for `oxide::typeck::size_of` / `align_of` against LLVM's
//! data-layout. The size-of helper hardcodes byte-width constants for
//! every primitive (1/2/4/8) and assumes 8-byte pointers; this test
//! exercises a real `TargetMachine`, asks LLVM for `get_store_size_of`
//! on the lowered LLVM type, and compares the answers.
//!
//! If the helper's table ever drifts from LLVM's view of the target
//! (e.g. someone hardcodes 4-byte pointers, or LLVM's `i32` stops being
//! 4 bytes for some bizarre reason), this test fires.
//!
//! Scope: primitives + sized arrays. ADT drift is covered by the
//! field-by-field algorithm in `src/typeck/layout.rs` unit tests; if
//! every primitive and array element matches the data-layout, the
//! struct-layout walker composes correctly.

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::BasicType;

use oxide::typeck::layout::{align_of, size_of};
use oxide::typeck::{TyArena, TyKind, TypeckResults};

fn build_target_machine() -> TargetMachine {
    Target::initialize_native(&InitializationConfig::default()).expect("native target init");
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).expect("Target::from_triple");
    target
        .create_target_machine(
            &triple,
            &TargetMachine::get_host_cpu_name().to_string(),
            &TargetMachine::get_host_cpu_features().to_string(),
            OptimizationLevel::None,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .expect("TargetMachine creation")
}

fn empty_typeck() -> TypeckResults {
    TypeckResults {
        tys: TyArena::new(),
        adts: index_vec::IndexVec::new(),
        fn_sigs: index_vec::IndexVec::new(),
        local_tys: index_vec::IndexVec::new(),
        expr_tys: index_vec::IndexVec::new(),
        call_type_args: std::collections::HashMap::new(),
    }
}

/// Every primitive width matches `TargetData::get_store_size` on the
/// corresponding LLVM int type. The helper's hardcoded constants are
/// checked end-to-end against LLVM's view of the target.
#[test]
fn primitive_sizes_match_data_layout() {
    let ctx = Context::create();
    let machine = build_target_machine();
    let td = machine.get_target_data();
    let mut t = empty_typeck();

    let cases: &[(oxide::typeck::TyId, inkwell::types::BasicTypeEnum)] = &[
        (t.tys.i8, ctx.i8_type().as_basic_type_enum()),
        (t.tys.u8, ctx.i8_type().as_basic_type_enum()),
        (t.tys.bool, ctx.bool_type().as_basic_type_enum()),
        (t.tys.i16, ctx.i16_type().as_basic_type_enum()),
        (t.tys.u16, ctx.i16_type().as_basic_type_enum()),
        (t.tys.i32, ctx.i32_type().as_basic_type_enum()),
        (t.tys.u32, ctx.i32_type().as_basic_type_enum()),
        (t.tys.i64, ctx.i64_type().as_basic_type_enum()),
        (t.tys.u64, ctx.i64_type().as_basic_type_enum()),
        (t.tys.usize, ctx.i64_type().as_basic_type_enum()),
        (t.tys.isize, ctx.i64_type().as_basic_type_enum()),
    ];

    for (ty, ll) in cases.iter().copied() {
        let helper = size_of(&mut t, ty).expect("primitive has size");
        let llvm = td.get_store_size(&ll);
        assert_eq!(
            helper, llvm,
            "size_of drift: TyId({ty:?}) helper={helper}, llvm={llvm}",
        );
    }
}

/// Pointers are 8 bytes on every supported v0 target. Verify against
/// the data-layout's view of an opaque LLVM `ptr`.
#[test]
fn pointer_size_is_eight_bytes() {
    let ctx = Context::create();
    let machine = build_target_machine();
    let td = machine.get_target_data();
    let mut t = empty_typeck();

    let i32 = t.tys.i32;
    let ptr_ty = t.tys.intern(TyKind::Ptr(i32, oxide::parser::ast::Mutability::Mut));
    let ll_ptr = ctx
        .ptr_type(inkwell::AddressSpace::default())
        .as_basic_type_enum();

    let helper = size_of(&mut t, ptr_ty).expect("pointer has size");
    let llvm = td.get_store_size(&ll_ptr);
    assert_eq!(
        helper, 8,
        "pointer size_of helper expected 8, got {helper}"
    );
    assert_eq!(
        helper, llvm,
        "pointer size_of drift: helper={helper}, llvm={llvm}"
    );
}

/// Sized arrays inherit their elem alignment and have size = n * elem_size.
/// Verify the composed result matches LLVM's `get_store_size` on the
/// corresponding `[N x T]` type.
#[test]
fn sized_array_size_matches_data_layout() {
    let ctx = Context::create();
    let machine = build_target_machine();
    let td = machine.get_target_data();
    let mut t = empty_typeck();

    let i32 = t.tys.i32;
    let u8 = t.tys.u8;

    let arr_3_i32 = t.tys.intern(TyKind::Array(i32, Some(3)));
    let ll_arr_3_i32 = ctx.i32_type().array_type(3).as_basic_type_enum();
    assert_eq!(
        size_of(&mut t, arr_3_i32).unwrap(),
        td.get_store_size(&ll_arr_3_i32)
    );

    let arr_8_u8 = t.tys.intern(TyKind::Array(u8, Some(8)));
    let ll_arr_8_u8 = ctx.i8_type().array_type(8).as_basic_type_enum();
    assert_eq!(
        size_of(&mut t, arr_8_u8).unwrap(),
        td.get_store_size(&ll_arr_8_u8)
    );
}

/// Sanity: align_of matches LLVM's preferred alignment for primitives.
/// We use `get_abi_alignment` (LLVM's "ABI alignment") rather than the
/// preferred alignment because ABI alignment is what affects layout
/// choices and matches our natural-alignment table.
#[test]
fn primitive_aligns_match_data_layout() {
    let ctx = Context::create();
    let machine = build_target_machine();
    let td = machine.get_target_data();
    let mut t = empty_typeck();

    let cases: &[(oxide::typeck::TyId, inkwell::types::BasicTypeEnum)] = &[
        (t.tys.i8, ctx.i8_type().as_basic_type_enum()),
        (t.tys.i16, ctx.i16_type().as_basic_type_enum()),
        (t.tys.i32, ctx.i32_type().as_basic_type_enum()),
        (t.tys.i64, ctx.i64_type().as_basic_type_enum()),
        (t.tys.usize, ctx.i64_type().as_basic_type_enum()),
    ];

    for (ty, ll) in cases.iter().copied() {
        let helper = align_of(&mut t, ty).expect("primitive has align");
        let llvm = td.get_abi_alignment(&ll) as u64;
        assert_eq!(
            helper, llvm,
            "align_of drift: TyId({ty:?}) helper={helper}, llvm={llvm}",
        );
    }
}
