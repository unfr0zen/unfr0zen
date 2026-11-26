// Exercise the Nest platform helpers without a real framebuffer or GPIO.
#define main doom_main
#include "../doomgeneric_nest.c"
#undef main
#include <assert.h>

int main(void) {
    // product.config polarity and the GPIO sysfs active_low attribute.
    assert(piezo_level(0, 0) == '0');
    assert(piezo_level(0, 1) == '1');
    assert(piezo_level(1, 0) == '1');
    assert(piezo_level(1, 1) == '0');

    // A subsequent pan must not expose the game on either saved page.
    uint8_t menu[8] = {1, 2, 3, 4, 5, 6, 7, 8}, display[8] = {0};
    fb = display;
    before = menu;
    before_size = sizeof menu;
    before_y = 1;
    var.yres = 1;
    var.yres_virtual = 2;
    stride = 4;
    fb_fd = -1;
    restore_display();
    assert(memcmp(display, menu, sizeof menu) == 0);
    assert(var.yoffset == before_y);
    return 0;
}
