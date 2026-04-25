; ModuleID = 'oxide'
source_filename = "oxide"

declare i32 @print_int(i32)

define i32 @main() {
allocas:
  br label %body

body:                                             ; preds = %allocas
  %call = call i32 @print_int(i32 42)
  %call1 = call i32 @print_int(i32 7)
  ret i32 0
}
