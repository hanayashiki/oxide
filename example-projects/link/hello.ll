; ModuleID = 'hello'
source_filename = "hello"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128"
target triple = "arm64-apple-darwin25.2.0"

declare i32 @print_int(i32)

define i32 @main() {
allocas:
  br label %body

body:                                             ; preds = %allocas
  %call = call i32 @print_int(i32 42)
  %call1 = call i32 @print_int(i32 7)
  ret i32 0
}
