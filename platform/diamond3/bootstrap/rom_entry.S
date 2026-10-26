/* First-stage entry for this board's ROM bootstrap.
 * Linked at 0x00907400; branch to the boot wrapper at 0x0090b000.
 * Preserve the tested instruction/literal layout for the HAB input fixture.
 */
.arm
.global _start
_start:
    ldr r0, =0x00938000
    ldr r1, =0xC0DEBEEF
    str r1, [r0]
    ldr r1, =0x12345678
    str r1, [r0, #4]
    ldr r1, =0xABCD0003
    str r1, [r0, #8]
    ldr r0, =0x020BC000
    mcr p15, 0, r0, c7, c5, 0
    isb
    b 0x0090B000
    .ltorg
