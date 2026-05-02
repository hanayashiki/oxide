	.section	__TEXT,__text,regular,pure_instructions
	.build_version macos, 16, 0
	.globl	_abs                            ; -- Begin function abs
	.p2align	2
_abs:                                   ; @abs
	.cfi_startproc
; %bb.0:                                ; %common.ret
	cmp	w0, #0
	cneg	w0, w0, lt
	ret
	.cfi_endproc
                                        ; -- End function
	.globl	_abs2                           ; -- Begin function abs2
	.p2align	2
_abs2:                                  ; @abs2
	.cfi_startproc
; %bb.0:                                ; %entry
	cmp	w0, #0
	cneg	w0, w0, lt
	ret
	.cfi_endproc
                                        ; -- End function
	.globl	_count_iters                    ; -- Begin function count_iters
	.p2align	2
_count_iters:                           ; @count_iters
	.cfi_startproc
; %bb.0:                                ; %entry
	mov	w8, wzr
LBB2_1:                                 ; %header
                                        ; =>This Inner Loop Header: Depth=1
	add	w9, w0, w8
	cmp	w9, #1
	b.lt	LBB2_3
; %bb.2:                                ; %body
                                        ;   in Loop: Header=BB2_1 Depth=1
	sub	w8, w8, #1
	b	LBB2_1
LBB2_3:                                 ; %exit
	neg	w0, w8
	ret
	.cfi_endproc
                                        ; -- End function
	.globl	_main                           ; -- Begin function main
	.p2align	2
_main:                                  ; @main
	.cfi_startproc
; %bb.0:
	stp	x29, x30, [sp, #-16]!           ; 16-byte Folded Spill
	.cfi_def_cfa_offset 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	mov	w0, #10                         ; =0xa
	bl	_count_iters
	ldp	x29, x30, [sp], #16             ; 16-byte Folded Reload
	ret
	.cfi_endproc
                                        ; -- End function
.subsections_via_symbols
