/* Independent assembler reference for the Rust emitter. Link at the golden's
 * entry address; native symbols are absolute bindings from the reviewed ELF. */
.syntax unified
.arm
.text
.global _start
_start:
    push {r0-r3, r12, lr}
    sub sp, sp, #16
    ldr r0, [r4, #0x3d4]
    cmp r0, #0
    bne startup_done
    mov r0, #4
    bl operator_new
    cmp r0, #0
    beq startup_done
    str r0, [r4, #0x3d4]
    ldr r1, [r4, #0x70]
    bl testaccess_constructor
    ldr r2, [r4, #0x3d4]
    mov r0, #1
    str r0, [r2]
    mov r0, #0
    movw r1, #0x1e7c
    movt r1, #0x14
    mov r3, #1
    str r3, [sp]
    mov r12, #0
    str r12, [sp, #4]
    str r12, [sp, #8]
    bl create_thread
    str r0, [r4, #0xa8]
startup_done:
    add sp, sp, #16
    pop {r0-r3, r12, lr}
    movw r1, #0x5140
    b resume_client_init
.global bounded_read
bounded_read:
    ldr r12, command_reply_pointer
    push {r12, lr}
    mov r3, r8
    bl rcs_read_command
    pop {r12, pc}
command_reply_pointer:
    .word native_reply
.global reusable_socket
reusable_socket:
    push {r4, lr}
    sub sp, sp, #16
    bl open_socket
    movs r4, r0
    beq socket_done
    bl native_handle
    mov r1, #1
    mov r2, #2
    mov r3, #1
    str r3, [sp, #4]
    mov r3, #4
    str r3, [sp]
    add r3, sp, #4
    bl set_socket_option
    mov r0, r4
socket_done:
    add sp, sp, #16
    pop {r4, pc}
.global control_reading
control_reading:
    push {r4, lr}
    sub sp, sp, #8
    mov r4, r0
    ldr r0, [r0, #0x18]
    cmp r0, #0
    beq invalid_temperature
    mov r1, sp
    bl latest_temperature
    cmp r0, #0
    bne invalid_temperature
    ldr r0, [sp]
    mov r1, #1
    b temperature_done
invalid_temperature:
    mov r0, r4
    bl remote_temperature
    mov r1, #0
temperature_done:
    add sp, sp, #8
    pop {r4, lr}
    bx lr
.global control_result
control_result:
    cmp r1, #0
    beq fallback_temperature
    b return_temperature
.global screen_guard
screen_guard:
    cmp r0, #0
    bxeq lr
    ldr r0, [r0, #0xec]
    b resume_rcs_configure
.global sample_reply
sample_reply:
    cmp r0, #0
    ldrne r1, done_pointer
    ldreq r1, retry_pointer
    add r0, r4, #4
    b resume_post_reply
done_pointer:
    .word done_reply
retry_pointer:
    .word retry_reply
.global command_name
command_name:
    .asciz "kryptonitedata"
retry_reply:
    .asciz "Retry"
.space (-(. - _start)) & 3, 0

.section .note.GNU-stack,"",%progbits
