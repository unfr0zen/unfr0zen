/* Boot wrapper: arm the requested watchdog for OCRAM marker readback, then
 * enter recovery U-Boot in DDR. Loaded at 0x90B000 by the ROM bootstrap.
 * No flash writes.
 */
.arm
.global _start
_start:
    ldr r0, =0x00938000
    mov r1, #0
    mov r2, #64
1:  str r1, [r0], #4
    subs r2, r2, #1
    bne 1b
    ldr r0, =0x00938000
    ldr r1, =0x0090BF00
    ldr r1, [r1]
    str r1, [r0, #0x58]         /* host nonce proves this run executed */
    ldr r1, =0x5B4C0001
    str r1, [r0]
    ldr r0, =0x020BC000
    ldr r1, =0x0090BF04
    ldr r1, [r1]                /* host-provided WT, WDE, SRS, WDA */
    strh r1, [r0]
    ldr r1, =0x5555
    strh r1, [r0, #2]
    ldr r1, =0xAAAA
    strh r1, [r0, #2]
    mov r0, #0
    mcr p15, 0, r0, c7, c5, 0   /* invalidate instruction cache */
    dsb
    isb
    ldr pc, =0x87800000         /* recovery U-Boot, loaded in DDR */
