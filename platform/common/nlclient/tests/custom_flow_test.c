#include <assert.h>
#include "../src/custom_flow.h"
static int last = -1, fail = 0;
static int request(int action, void *context) { (void)context; last = action; return fail; }
static void matter_refresh_test(void) {
    struct custom_flow f = {.page = CUSTOM_MATTER};
    int started = 0;
    const char *paired = "Paired. Enable pairing to replace the controller.";
    assert(!custom_matter_refresh(&f, "Pairing open. Already paired; scan to replace the controller.", 0, &started, 0));
    assert(f.page == CUSTOM_MATTER && !started);
    /* Initial enrollment is already open without a settings request. */
    assert(custom_matter_refresh(&f, "Pairing open. Scan in your Matter app.", 0, &started, 0));
    assert(f.page == CUSTOM_MATTER_PAIRING && started);
    assert(custom_matter_refresh(&f, paired, 0, &started, 1));
    assert(f.page == CUSTOM_MATTER);
    f.page = CUSTOM_MATTER_PAIRING;
    assert(custom_back(&f) && f.page == CUSTOM_HOME);
    f.page = CUSTOM_MATTER_PAIRING; started = 0;
    /* A pre-existing Paired status cannot acknowledge a pending request. */
    assert(!custom_matter_refresh(&f, paired, 1, &started, 1));
    assert(!started && f.page == CUSTOM_MATTER_PAIRING);
    assert(!custom_matter_refresh(&f, "Removing controllers...", 0, &started, 2));
    assert(!started && f.page == CUSTOM_MATTER_PAIRING);
    /* Completion can occur entirely between polls, including re-pairing. */
    assert(custom_matter_refresh(&f, paired, 0, &started, 2));
    assert(started && f.page == CUSTOM_MATTER_PAIRING);
    assert(custom_matter_refresh(&f, paired, 0, &started, 3));
    assert(f.page == CUSTOM_MATTER && !f.action_failed);
    f.page = CUSTOM_MATTER_PAIRING; started = 0;
    assert(!custom_matter_refresh(&f, "Pairing open. Scan in your Matter app.", 1, &started, 1));
    assert(!started);
    assert(custom_matter_refresh(&f, "Pairing open. Scan in your Matter app.", 0, &started, 2));
    /* An acknowledged open window can outlive the request timeout. */
    assert(!custom_matter_refresh(&f, "Pairing open. Scan in your Matter app.", 0, &started, 120));
    assert(custom_matter_refresh(&f, paired, 0, &started, 121));
    assert(f.page == CUSTOM_MATTER);
    f.page = CUSTOM_MATTER_PAIRING; started = 0;
    assert(!custom_matter_refresh(&f, paired, 1, &started, 119));
    assert(custom_matter_refresh(&f, paired, 1, &started, 120));
    assert(f.page == CUSTOM_MATTER && f.action_failed);
    f.page = CUSTOM_MATTER_PAIRING; started = 1;
    assert(custom_matter_refresh(&f, "Matter not enabled", 0, &started, 3));
    assert(f.page == CUSTOM_MATTER && f.action_failed);
    f.page = CUSTOM_MATTER_PAIRING;
    assert(custom_matter_refresh(&f, "Ready to pair.", 0, &started, 3));
    assert(f.page == CUSTOM_MATTER && !f.action_failed);
    f.page = CUSTOM_ZIGBEE_PAIRING;
    assert(!custom_matter_refresh(&f, paired, 0, &started, 3));
    assert(f.page == CUSTOM_ZIGBEE_PAIRING);
}
int main(void) {
    matter_refresh_test();
    const int actions[16][5] = {{2, -1, -1, -1, -1}, {0, 2, -1, -1, -1}, {1, 2, -1, -1, -1}, {0, 1, 2, -1, -1},
                                {3, 2, -1, -1, -1}, {0, 3, 2, -1, -1}, {1, 3, 2, -1, -1}, {0, 1, 3, 2, -1},
                                {4, 2, -1, -1, -1}, {0, 4, 2, -1, -1}, {1, 4, 2, -1, -1}, {0, 1, 4, 2, -1},
                                {4, 3, 2, -1, -1}, {0, 4, 3, 2, -1}, {1, 4, 3, 2, -1}, {0, 1, 4, 3, 2}};
    for (unsigned int mask = 0; mask < 16; mask++) {
        for (int i = 0; i < 5; i++) assert(custom_home_action(i, mask) == actions[mask][i]);
        assert(custom_home_action(custom_home_count(mask) - 1, mask) == 2);
        assert(custom_home_action(-1, mask) == -1);
    }
    struct custom_flow f = {0};
    assert(custom_choose(&f, 0, request, 0) == 1 && f.page == CUSTOM_MATTER);
    last = -1;
    assert(custom_choose(&f, 0, request, 0) == 1 && last == -1 && f.page == CUSTOM_MATTER_CONFIRM);
    assert(custom_choose(&f, 1, request, 0) == 1 && last == -1 && f.page == CUSTOM_MATTER);
    custom_choose(&f, 0, request, 0);
    assert(custom_choose(&f, 0, request, 0) == 1 && last == 0 && f.page == CUSTOM_MATTER_PAIRING);
    assert(custom_choose(&f, 0, request, 0) == 1 && f.page == CUSTOM_HOME);
    custom_choose(&f, 0, request, 0);
    custom_choose(&f, 0, request, 0);
    fail = 1; custom_choose(&f, 0, request, 0); assert(f.action_failed && f.page == CUSTOM_MATTER);
    assert(custom_back(&f) && f.page == CUSTOM_HOME && !f.action_failed);
    custom_choose(&f, 1, request, 0); assert(f.page == CUSTOM_ZIGBEE);
    last = -1; custom_choose(&f, 0, request, 0);
    assert(f.page == CUSTOM_ZIGBEE_CONFIRM && last == -1);
    custom_choose(&f, 1, request, 0); assert(f.page == CUSTOM_ZIGBEE && last == -1);
    custom_choose(&f, 0, request, 0); fail = 0; custom_choose(&f, 0, request, 0);
    assert(f.page == CUSTOM_ZIGBEE_PAIRING && last == 1 && !f.action_failed);
    assert(custom_back(&f) && f.page == CUSTOM_ZIGBEE);
    custom_back(&f); assert(f.page == CUSTOM_HOME && f.parent_selection == 1);
    assert(!custom_back(&f)); assert(custom_choose(&f, 2, request, 0) == 2);
    /* Doom opens its page; Play asks for the game and stays, Back goes home. */
    last = -1; assert(custom_choose(&f, 3, request, 0) == 1 && last == -1 && f.page == CUSTOM_DOOM);
    assert(custom_choose(&f, 0, request, 0) == 1 && last == 2 && f.page == CUSTOM_DOOM && !f.action_failed);
    fail = 1; custom_choose(&f, 0, request, 0); assert(f.action_failed && f.page == CUSTOM_DOOM);
    fail = 0; assert(custom_choose(&f, 1, request, 0) == 1 && f.page == CUSTOM_HOME && !f.action_failed);
    custom_choose(&f, 3, request, 0); assert(custom_back(&f) && f.page == CUSTOM_HOME);
    /* Theme choices and Back return to the unfr0zen menu. */
    f.theme_count = 4;
    last = -1; assert(custom_choose(&f, 4, request, 0) == 1 && last == -1 && f.page == CUSTOM_THEMES);
    for (int choice = 0; choice < f.theme_count; choice++) {
        f.page = CUSTOM_THEMES;
        assert(custom_choose(&f, choice, request, 0) == 1 && last == 3 + choice && f.page == CUSTOM_HOME && !f.action_failed);
    }
    custom_choose(&f, 4, request, 0); fail = 1;
    custom_choose(&f, 0, request, 0); assert(last == 3 && f.action_failed && f.page == CUSTOM_THEMES);
    fail = 0; last = -1;
    assert(custom_choose(&f, f.theme_count, request, 0) == 1 && last == -1 && f.page == CUSTOM_HOME);
    f.page = CUSTOM_THEMES; f.theme_count = 7;
    assert(custom_choose(&f, 6, request, 0) == 1 && last == 9 && f.page == CUSTOM_HOME);
    f.page = CUSTOM_THEMES; last = -1;
    assert(custom_choose(&f, 7, request, 0) == 1 && last == -1 && f.page == CUSTOM_HOME);
    return 0;
}
