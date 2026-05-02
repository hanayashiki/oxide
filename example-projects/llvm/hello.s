	.section	__TEXT,__text,regular,pure_instructions
	.build_version macos, 16, 0
	.globl	_main                           ; -- Begin function main
	.p2align	2
_main:                                  ; @main
	.cfi_startproc
; %bb.0:
	adrp	x8, _variable@PAGE
	ldr	w9, [x8, _variable@PAGEOFF]
	mov	w10, #3                         ; =0x3
	mul	w0, w9, w10
	str	w0, [x8, _variable@PAGEOFF]
	ret
	.cfi_endproc
                                        ; -- End function
	.section	__DATA,__data
	.globl	_variable                       ; @variable
	.p2align	2, 0x0
_variable:
	.long	21                              ; 0x15

.subsections_via_symbols
