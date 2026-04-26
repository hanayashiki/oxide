; ModuleID = 'oxide'
source_filename = "oxide"

declare i32 @print_int(i32)

define i32 @fib(i32 %n) {
allocas:
  %n.1.slot = alloca i32, align 4
  %if.slot = alloca i32, align 4
  br label %body

body:                                             ; preds = %allocas
  store i32 %n, ptr %n.1.slot, align 4
  %load = load i32, ptr %n.1.slot, align 4
  %le = icmp ule i32 %load, 1
  br i1 %le, label %if.then, label %if.else

if.then:                                          ; preds = %body
  store i32 1, ptr %if.slot, align 4
  br label %if.end

if.else:                                          ; preds = %body
  %load1 = load i32, ptr %n.1.slot, align 4
  %sub = sub i32 %load1, 1
  %call = call i32 @fib(i32 %sub)
  %load2 = load i32, ptr %n.1.slot, align 4
  %sub3 = sub i32 %load2, 2
  %call4 = call i32 @fib(i32 %sub3)
  %add = add i32 %call, %call4
  store i32 %add, ptr %if.slot, align 4
  br label %if.end

if.end:                                           ; preds = %if.else, %if.then
  %if.val = load i32, ptr %if.slot, align 4
  ret i32 %if.val
}

define i32 @main() {
allocas:
  br label %body

body:                                             ; preds = %allocas
  %call = call i32 @fib(i32 12)
  %call1 = call i32 @print_int(i32 %call)
  ret i32 0
}
