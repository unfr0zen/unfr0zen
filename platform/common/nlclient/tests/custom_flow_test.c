#include <assert.h>
#include "../src/custom_flow.h"
static int last = -1, fail = 0;
static int request(int action, void *context) {
    (void)context;
    last = action;
    return fail;
}
static void matter_refresh_test(void) {
    struct custom_flow f = {.page = CUSTOM_MATTER, .root = CUSTOM_MATTER};
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
    assert(!custom_back(&f));
    f.page = CUSTOM_MATTER_PAIRING;
    started = 0;
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
    f.page = CUSTOM_MATTER_PAIRING;
    started = 0;
    assert(!custom_matter_refresh(&f, "Pairing open. Scan in your Matter app.", 1, &started, 1));
    assert(!started);
    assert(custom_matter_refresh(&f, "Pairing open. Scan in your Matter app.", 0, &started, 2));
    /* An acknowledged open window can outlive the request timeout. */
    assert(!custom_matter_refresh(&f, "Pairing open. Scan in your Matter app.", 0, &started, 120));
    assert(custom_matter_refresh(&f, paired, 0, &started, 121));
    assert(f.page == CUSTOM_MATTER);
    f.page = CUSTOM_MATTER_PAIRING;
    started = 0;
    assert(!custom_matter_refresh(&f, paired, 1, &started, 119));
    assert(custom_matter_refresh(&f, paired, 1, &started, 120));
    assert(f.page == CUSTOM_MATTER && f.action_failed);
    f.page = CUSTOM_MATTER_PAIRING;
    started = 1;
    assert(custom_matter_refresh(&f, "Matter not enabled", 0, &started, 3));
    assert(f.page == CUSTOM_MATTER && f.action_failed);
    f.page = CUSTOM_MATTER_PAIRING;
    assert(custom_matter_refresh(&f, "Ready to pair.", 0, &started, 3));
    assert(f.page == CUSTOM_MATTER && !f.action_failed);
    f.page = CUSTOM_ZIGBEE_PAIRING;
    assert(!custom_matter_refresh(&f, paired, 0, &started, 3));
    assert(f.page == CUSTOM_ZIGBEE_PAIRING);
}
static void schedule_test(void) {
    struct custom_flow f = {.page = CUSTOM_SCHEDULE_HOME, .root = CUSTOM_SCHEDULE_HOME, .hold_minutes = 120};
    unsigned int fields[4];
    const char *error;
    assert(custom_schedule_status("1 0 1800000000 120\n", fields, &error));
    assert(fields[0] == 1 && fields[1] == 0 && fields[2] == 1800000000 && fields[3] == 120 && !*error);
    assert(custom_schedule_status("0 1 0 1\nNo saved schedule", fields, &error));
    assert(custom_same(error, "No saved schedule"));
    assert(custom_schedule_status("1 0 0 1440\nWrite failed", fields, &error));
    assert(custom_same(error, "Write failed"));
    assert(!custom_schedule_status("", fields, &error));
    assert(!custom_schedule_status("1 0 0 0", fields, &error));
    assert(!custom_schedule_status("1 0 0 1441", fields, &error));
    assert(!custom_schedule_status("1 0 4294967296 120", fields, &error));
    assert(!custom_schedule_status("2 0 0 120", fields, &error));
    assert(!custom_schedule_status("1 0 0 120 extra", fields, &error));
    last = 99;
    custom_choose(&f, 1, request, 0);
    assert(f.page == CUSTOM_SCHEDULE && last == 99 && !custom_activity(f.page));
    custom_choose(&f, 0, request, 0);
    assert(last == REQUEST_SCHEDULE_ON);
    custom_choose(&f, 1, request, 0);
    assert(last == REQUEST_SCHEDULE_OFF);
    custom_choose(&f, 1, request, 0);
    assert(last == REQUEST_SCHEDULE_OFF);
    custom_choose(&f, 2, request, 0);
    assert(f.page == CUSTOM_SCHEDULE_HOME);
    custom_choose(&f, 2, request, 0);
    assert(f.page == CUSTOM_HOLD);
    custom_choose(&f, 0, request, 0);
    assert(last == REQUEST_HOLD_ON);
    custom_choose(&f, 1, request, 0);
    assert(last == REQUEST_HOLD_OFF);
    assert(custom_back(&f) && f.page == CUSTOM_SCHEDULE_HOME);
    custom_choose(&f, 3, request, 0);
    assert(f.page == CUSTOM_HOLD_DURATION && custom_schedule_page(f.page));
    custom_duration_step(&f, -1000);
    assert(f.hold_minutes == 1);
    custom_duration_step(&f, 2000);
    assert(f.hold_minutes == 1440);
    custom_duration_step(&f, -1);
    assert(f.hold_minutes == 1439);
    fail = 1;
    custom_choose(&f, 0, request, 0);
    assert(last == REQUEST_HOLD_DURATION && f.action_failed && f.page == CUSTOM_HOLD_DURATION);
    fail = 0;
    custom_choose(&f, 0, request, 0);
    assert(!f.action_failed && f.page == CUSTOM_SCHEDULE_HOME);
    custom_choose(&f, 3, request, 0);
    last = 99;
    f.hold_minutes = 0;
    custom_choose(&f, 0, request, 0);
    assert(f.action_failed && last == 99);
    assert(custom_back(&f) && f.page == CUSTOM_SCHEDULE_HOME && !f.action_failed);
}
static void duration_wheel_test(void) {
    struct custom_flow f = {.hold_minutes = 2};
    assert(custom_duration_wheel(&f, 3 * 65536) && f.hold_minutes == 5);
    assert(custom_duration_wheel(&f, -3 * 65536) && f.hold_minutes == 2);
    assert(!custom_duration_wheel(&f, 32768) && f.hold_fraction == 32768);
    assert(!custom_duration_wheel(&f, -65536) && f.hold_fraction == -32768);
    assert(!custom_duration_wheel(&f, 32768) && f.hold_fraction == 0);
    assert(!custom_duration_wheel(&f, 32768));
    assert(custom_duration_wheel(&f, 32768) && f.hold_minutes == 3 && !f.hold_fraction);
    assert(!custom_duration_wheel(&f, -32768));
    assert(custom_duration_wheel(&f, -32768) && f.hold_minutes == 2 && !f.hold_fraction);
    assert(custom_duration_wheel(&f, -2147483647 - 1) && f.hold_minutes == 1);
    assert(!custom_duration_wheel(&f, -32768) && !f.hold_fraction);
    assert(custom_duration_wheel(&f, 2147483647) && f.hold_minutes == 1440 && !f.hold_fraction);
    assert(custom_duration_wheel(&f, -65536) && f.hold_minutes == 1439);
    f.page = f.root = CUSTOM_SCHEDULE_HOME;
    f.hold_fraction = 123;
    custom_choose(&f, 3, request, 0);
    assert(f.page == CUSTOM_HOLD_DURATION && !f.hold_fraction);
}
static void menu_insertion_test(void) {
    assert(custom_schedule_insert_before("templock", 1));
    assert(!custom_schedule_insert_before("slideshow", 1));
    assert(custom_schedule_insert_before("slideshow", 0));
    assert(!custom_schedule_insert_before("templock", 0));
    assert(!custom_schedule_insert_before("legalinfo", 0));
    assert(!custom_schedule_insert_before("proinfoscreen", 1));
    int existing, schedule, legal;
    const char *names[32] = {"wifi", "legalinfo"};
    void *screens[36] = {0, 0, &existing};
    unsigned int n = 2, s = 3, next = 4;
    assert(custom_insert_legacy(names, screens, &n, &s, &next, &schedule));
    screens[s] = &legal;
    assert(n == 3 && s == 4 && next == 5);
    assert(custom_same(names[0], "wifi") && custom_same(names[1], "appsetup") && custom_same(names[2], "legalinfo"));
    assert(screens[2] == &existing && screens[3] == &schedule && screens[4] == &legal);
    n = 32;
    s = 33;
    next = 34;
    assert(!custom_insert_legacy(names, screens, &n, &s, &next, &schedule));
    assert(n == 32 && s == 33 && next == 34 && screens[4] == &legal);
    n = 2;
    assert(!custom_insert_legacy(names, screens, &n, &s, &next, &schedule));
    assert(n == 2 && s == 33 && next == 34);
    n = 30;
    s = 31;
    next = 32;
    assert(!custom_insert_legacy(names, screens, &n, &s, &next, &schedule));
    assert(n == 30 && s == 31 && next == 32 && !names[30] && !screens[31]);
    n = 29;
    s = 30;
    next = 31;
    names[28] = "legalinfo";
    assert(custom_insert_legacy(names, screens, &n, &s, &next, &schedule));
    assert(n == 30 && s == 31 && next == 32 && custom_same(names[29], "legalinfo"));
    assert(custom_menu_vector_valid(0, 0, 0));
    assert(custom_menu_vector_valid(16, 32, 32));
    assert(custom_menu_vector_valid(16, 24, 32));
    assert(!custom_menu_vector_valid(0, 4, 8));
    assert(!custom_menu_vector_valid(16, 12, 32));
    assert(!custom_menu_vector_valid(16, 36, 32));
    assert(!custom_menu_vector_valid(16, 18, 32));
    assert(!custom_menu_vector_valid(16, 32, 8192));
}
int main(void) {
    menu_insertion_test();
    duration_wheel_test();
    schedule_test();
    matter_refresh_test();
    /* Each entry's stock screen name opens its page; Legal Info's is UNFR0ZEN's. */
    assert(custom_root("account") == CUSTOM_ZIGBEE && custom_root("update") == CUSTOM_MATTER);
    assert(custom_root("proinfoscreen") == CUSTOM_THEMES && custom_root("templock") == CUSTOM_DOOM);
    assert(custom_root("legalinfo") == CUSTOM_HOME && custom_root("accounts") == CUSTOM_HOME);
    /* UNFR0ZEN: Done leaves; Back has no page to go to. */
    struct custom_flow f = {.page = CUSTOM_HOME, .root = CUSTOM_HOME};
    assert(custom_choose(&f, 0, request, 0) == 2);
    assert(!custom_back(&f));
    assert(custom_root("appsetup") == CUSTOM_SCHEDULE_HOME);
    last = 99;
    for (int choice = 1; choice < CHOICE_ROWS; choice++)
        assert(custom_choose(&f, choice, request, 0) == CHOOSE_STAY && f.page == CUSTOM_HOME && last == 99);
    /* Matter: Enable pairing, its confirmation, the pairing; Back goes up a
     * page, and from Matter's own page out to the menu. */
    f = (struct custom_flow){.page = CUSTOM_MATTER, .root = CUSTOM_MATTER};
    last = -1;
    assert(custom_choose(&f, 0, request, 0) == 1 && last == -1 && f.page == CUSTOM_MATTER_CONFIRM);
    assert(custom_choose(&f, 1, request, 0) == 1 && last == -1 && f.page == CUSTOM_MATTER);
    custom_choose(&f, 0, request, 0);
    assert(custom_choose(&f, 0, request, 0) == 1 && last == 0 && f.page == CUSTOM_MATTER_PAIRING);
    /* The pairing's Back leaves: Matter's page would show the pairing again. */
    assert(custom_choose(&f, 0, request, 0) == 2);
    f.page = CUSTOM_MATTER;
    custom_choose(&f, 0, request, 0);
    fail = 1;
    custom_choose(&f, 0, request, 0);
    assert(f.action_failed && f.page == CUSTOM_MATTER);
    fail = 0;
    assert(custom_choose(&f, 1, request, 0) == 2 && f.page == CUSTOM_MATTER && !f.action_failed);
    assert(!custom_back(&f));
    /* Zigbee alike. */
    f = (struct custom_flow){.page = CUSTOM_ZIGBEE, .root = CUSTOM_ZIGBEE};
    last = -1;
    custom_choose(&f, 0, request, 0);
    assert(f.page == CUSTOM_ZIGBEE_CONFIRM && last == -1);
    custom_choose(&f, 1, request, 0);
    assert(f.page == CUSTOM_ZIGBEE && last == -1);
    custom_choose(&f, 0, request, 0);
    custom_choose(&f, 0, request, 0);
    assert(f.page == CUSTOM_ZIGBEE_PAIRING && last == 1 && !f.action_failed);
    assert(custom_back(&f) && f.page == CUSTOM_ZIGBEE);
    assert(!custom_back(&f) && custom_choose(&f, 1, request, 0) == 2);
    /* Doom: Play asks for the game and stays; Back leaves. */
    f = (struct custom_flow){.page = CUSTOM_DOOM, .root = CUSTOM_DOOM};
    last = -1;
    assert(custom_choose(&f, 0, request, 0) == 1 && last == 2 && f.page == CUSTOM_DOOM && !f.action_failed);
    fail = 1;
    custom_choose(&f, 0, request, 0);
    assert(f.action_failed && f.page == CUSTOM_DOOM);
    fail = 0;
    assert(custom_choose(&f, 1, request, 0) == 2 && !f.action_failed);
    /* Themes: a choice asks for it and leaves, as Back does; a failed one stays. */
    f = (struct custom_flow){.page = CUSTOM_THEMES, .root = CUSTOM_THEMES, .theme_count = 4};
    for (int choice = 0; choice < f.theme_count; choice++) {
        assert(custom_choose(&f, choice, request, 0) == 2 && last == 3 + choice && !f.action_failed);
    }
    fail = 1;
    assert(custom_choose(&f, 0, request, 0) == 1 && last == 3 && f.action_failed && f.page == CUSTOM_THEMES);
    fail = 0;
    last = -1;
    assert(custom_choose(&f, f.theme_count, request, 0) == 2 && last == -1);
    f.theme_count = 7;
    assert(custom_choose(&f, 6, request, 0) == 2 && last == 9);
    assert(custom_choose(&f, 8, request, 0) == 1);
    return 0;
}
