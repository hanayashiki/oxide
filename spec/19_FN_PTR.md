# Function Pointer

Goal: support a subset of Rust `fn(T1, T2) -> R`.

## Reference

https://doc.rust-lang.org/reference/types/function-pointer.html#grammar-FunctionTypeQualifiers

## Requirements

1. Functions can consume parameters of `Fn` as parameter. Reusing the currently defined `Fn` type 
2. `Fn` can be qualifies, defining their calling convention, e.g. `f: extern "C" fn(T1, T2) -> R`.
3. Subtype Rules (where we deviate from Rust to provide a more flexible model):
    1. Contravariant on parameters, covariant on return type.
    2. Invariant on `is_extern_c: bool`.
    3. Invariant on arity.
    4. Unrelated to plain pointers.
4. Sized obligations:
    1. Wherever you define a Function Pointer, its parameters and return type must be sized, just like current fn_decl. Consider reusage of logic here.
5. As rules:
    1. Only subtype casting is allowed. The current As rule can incorporated in the subtype rule.
6. Codegen rule:
    1. `Fn` is always expressed in pointer, just like arrays, place or value.
    2. Must be lowered to Function Type at call site
        1. https://llvm.org/docs/LangRef.html#function-type
    3. Converted to
        1. Calling side: `call i32 %add(i32 %arg1, i32 %arg2)` instead of `call ret @fn_name`
        2. Fns passed as pointer: `call void @print(i32 100, i32 200, i32 (i32, i32)* @add)`
7. Syntax:
    1. `fn(ok: i32) -> bool` parameter names are allowed but dismissed in type interning thus subtype rules and monomorphism.
    2. No `*` at fn ptr itself. `*mut fn(ok: &mut i32) -> bool` is a pointer-to-pointer.
