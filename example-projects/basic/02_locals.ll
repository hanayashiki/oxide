; ModuleID = 'oxide'
source_filename = "oxide"

define i32 @compute(i32 %x) {
allocas:
  %x.0.slot = alloca i32, align 4
  %a.1.slot = alloca i32, align 4
  %b.2.slot = alloca i32, align 4
  %c.3.slot = alloca i32, align 4
  br label %body

body:                                             ; preds = %allocas
  store i32 %x, ptr %x.0.slot, align 4
  %load = load i32, ptr %x.0.slot, align 4
  %add = add i32 %load, 1
  store i32 %add, ptr %a.1.slot, align 4
  %load1 = load i32, ptr %a.1.slot, align 4
  %mul = mul i32 %load1, 2
  store i32 %mul, ptr %b.2.slot, align 4
  %load2 = load i32, ptr %b.2.slot, align 4
  %load3 = load i32, ptr %x.0.slot, align 4
  %sub = sub i32 %load2, %load3
  store i32 %sub, ptr %c.3.slot, align 4
  %load4 = load i32, ptr %c.3.slot, align 4
  ret i32 %load4
}
