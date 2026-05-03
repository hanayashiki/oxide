; ModuleID = '01_add'
source_filename = "01_add"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128"
target triple = "arm64-apple-darwin25.2.0"

define i32 @add(i32 %a, i32 %b) {
allocas:
  %a.0.slot = alloca i32, align 4
  %b.1.slot = alloca i32, align 4
  br label %body

body:                                             ; preds = %allocas
  store i32 %a, ptr %a.0.slot, align 4
  store i32 %b, ptr %b.1.slot, align 4
  %load = load i32, ptr %a.0.slot, align 4
  %load1 = load i32, ptr %b.1.slot, align 4
  %add = add i32 %load, %load1
  ret i32 %add
}
