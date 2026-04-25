# Type Checker

## Requirements

Goal: minimal typeck algorithm based on HM.

Target: just to type check primitive types & functions so we can get to llvm fast.

API style: query-based `get_type_by_expr_id(...)`. cache known results.

Acceptance:

```
fn add(a: i32, b: i32) { a +     b }
//                       ^ i32   ^ i32
```
