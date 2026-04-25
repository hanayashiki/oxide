; ModuleID = '05_struct.c'
source_filename = "05_struct.c"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx26.0.0"

%struct.Point = type { i32, i32 }

@__const.origin_distance_sq.p = private unnamed_addr constant %struct.Point { i32 3, i32 4 }, align 4

; Function Attrs: noinline nounwind ssp uwtable(sync)
define i32 @distance_sq(ptr noundef %p) #0 {
entry:
  %p.addr = alloca ptr, align 8
  store ptr %p, ptr %p.addr, align 8
  %0 = load ptr, ptr %p.addr, align 8
  %x = getelementptr inbounds %struct.Point, ptr %0, i32 0, i32 0
  %1 = load i32, ptr %x, align 4
  %2 = load ptr, ptr %p.addr, align 8
  %x1 = getelementptr inbounds %struct.Point, ptr %2, i32 0, i32 0
  %3 = load i32, ptr %x1, align 4
  %mul = mul nsw i32 %1, %3
  %4 = load ptr, ptr %p.addr, align 8
  %y = getelementptr inbounds %struct.Point, ptr %4, i32 0, i32 1
  %5 = load i32, ptr %y, align 4
  %6 = load ptr, ptr %p.addr, align 8
  %y2 = getelementptr inbounds %struct.Point, ptr %6, i32 0, i32 1
  %7 = load i32, ptr %y2, align 4
  %mul3 = mul nsw i32 %5, %7
  %add = add nsw i32 %mul, %mul3
  ret i32 %add
}

; Function Attrs: noinline nounwind ssp uwtable(sync)
define i32 @origin_distance_sq() #0 {
entry:
  %p = alloca %struct.Point, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %p, ptr align 4 @__const.origin_distance_sq.p, i64 8, i1 false)
  %call = call i32 @distance_sq(ptr noundef %p)
  ret i32 %call
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #1

attributes #0 = { noinline nounwind ssp uwtable(sync) "frame-pointer"="non-leaf" "no-trapping-math"="true" "probe-stack"="__chkstk_darwin" "stack-protector-buffer-size"="8" "target-cpu"="apple-m1" "target-features"="+aes,+altnzcv,+bti,+ccdp,+ccidx,+complxnum,+crc,+dit,+dotprod,+flagm,+fp-armv8,+fp16fml,+fptoint,+fullfp16,+jsconv,+lse,+neon,+pauth,+perfmon,+predres,+ras,+rcpc,+rdm,+sb,+sha2,+sha3,+specrestrict,+ssbs,+v8.1a,+v8.2a,+v8.3a,+v8.4a,+v8.5a,+v8a,+zcm,+zcz" }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }

!llvm.module.flags = !{!0, !1, !2, !3, !4}
!llvm.ident = !{!5}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 26, i32 1]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 8, !"PIC Level", i32 2}
!3 = !{i32 7, !"uwtable", i32 1}
!4 = !{i32 7, !"frame-pointer", i32 1}
!5 = !{!"Apple clang version 17.0.0 (clang-1700.4.4.1)"}
