; ModuleID = 'hello'
source_filename = "hello"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128"
target triple = "arm64-apple-darwin25.2.0"

@.str.0 = private unnamed_addr constant [12 x i8] c"hello world\00"
@.str.1 = private unnamed_addr constant [8 x i8] c"goodbye\00"

declare i32 @puts(ptr)

define i32 @main() {
allocas:
  br label %body

body:                                             ; preds = %allocas
  %call = call i32 @puts(ptr @.str.0)
  %call1 = call i32 @puts(ptr @.str.1)
  ret i32 0
}
