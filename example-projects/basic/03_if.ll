; ModuleID = 'oxide'
source_filename = "oxide"

define i32 @max(i32 %a, i32 %b) {
allocas:
  %a.0.slot = alloca i32, align 4
  %b.1.slot = alloca i32, align 4
  br label %body

body:                                             ; preds = %allocas
  store i32 %a, ptr %a.0.slot, align 4
  store i32 %b, ptr %b.1.slot, align 4
  %load = load i32, ptr %a.0.slot, align 4
  %load1 = load i32, ptr %b.1.slot, align 4
  %gt = icmp sgt i32 %load, %load1
  br i1 %gt, label %if.then, label %if.else

if.then:                                          ; preds = %body
  %load2 = load i32, ptr %a.0.slot, align 4
  ret i32 %load2

if.else:                                          ; preds = %body
  br label %if.end

if.end:                                           ; preds = %if.else
  %load3 = load i32, ptr %b.1.slot, align 4
  ret i32 %load3
}
