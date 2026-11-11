/* Independent GNU ARM linker bindings for the reviewed j49 stock ELF. */
operator_new = 0x2ad84;
testaccess_constructor = 0x141b2c;
create_thread = 0x2bf24;
resume_client_init = 0x47e8c;
open_socket = 0x29e3c;
native_handle = 0x2c608;
set_socket_option = 0x2cbfc;
native_reply = 0x4fa9c0;
latest_temperature = 0x1ebc18;
remote_temperature = 0x1b72ec;
fallback_temperature = 0x237a8c;
return_temperature = 0x237ad8;
resume_rcs_configure = 0x19a66c;
resume_post_reply = 0x141868;
done_reply = 0x3dfa1c;
rcs_screen_lookup = 0x51a1c;
rcs_layer_lookup = 0x519a0;
rcs_layer_new = 0x19862c;
rcs_screen_new = 0x19a6f8;
rcs_add_layer = 0x51968;
rcs_add_screen = 0x519e8;
strcmp = 0x2bec4;
rcs_associated = 0x96a08;
rcs_string_find = 0x2a820;
rcs_native_post = 0x6c180;
rcs_native_read = 0x2c77c;
rcs_interval = 0x2b36c;
rcs_string_clear = 0x2ce48;
rcs_poll = 0x2ad60;
ENTRY(_start)
SECTIONS {
    . = 0x51451c;
    .text : { *(.text) *(.text.*) }
    .rodata : { *(.rodata*) }
    /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) }
}
