define i32 @abs(i32 %x) {
    %positive = icmp sge i32 %x, 0
    br i1 %positive, label %branch_positive, label %branch_negative
branch_positive:
    ret i32 %x
branch_negative:
    %negated = mul i32 %x, -1
    ret i32 %negated
}

define i32 @abs2(i32 %x) {
entry:
    %positive = icmp sge i32 %x, 0
    br i1 %positive, label %merge, label %branch_negative
branch_negative:
    %negated = mul i32 %x, -1
    br label %merge
merge:
    %merged = phi i32 [ %x, %entry], [ %negated, %branch_negative ]
    ret i32 %merged
}

define i32 @count_iters(i32 %n0) {
entry:
    br label %header

header:
    %k = phi i32 [ 0, %entry ], [%k.next, %body]
    %n = phi i32 [ %n0, %entry ], [ %n.next, %body ]
    %keep_going = icmp sgt i32 %n, 0
    br i1 %keep_going, label %body, label %exit

body:
    %n.next = sub i32 %n, 1
    %k.next = add i32 %k, 1
    br label %header

exit:
    ret i32 %k
}

define i32 @main() {
    %r = call i32 @count_iters(i32 10)
    ret i32 %r
}
