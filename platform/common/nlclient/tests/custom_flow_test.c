#include <assert.h>
#include "../src/custom_flow.h"
static int last = -1, fail = 0;
static int request(int action, void *context) { (void)context; last = action; return fail; }
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
    /* Each entry's stock screen name opens its page; Legal Info's is UNFR0ZEN's. */
    assert(custom_root("account") == CUSTOM_ZIGBEE && custom_root("update") == CUSTOM_MATTER);
    assert(custom_root("proinfoscreen") == CUSTOM_THEMES && custom_root("templock") == CUSTOM_DOOM);
    assert(custom_root("legalinfo") == CUSTOM_HOME && custom_root("accounts") == CUSTOM_HOME);
    /* UNFR0ZEN: Done leaves; Back has no page to go to. */
    struct custom_flow f = {.page = CUSTOM_HOME, .root = CUSTOM_HOME};
    assert(custom_choose(&f, 0, request, 0) == 2);
    assert(!custom_back(&f));
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
    fail = 1; custom_choose(&f, 0, request, 0); assert(f.action_failed && f.page == CUSTOM_MATTER);
    fail = 0;
    assert(custom_choose(&f, 1, request, 0) == 2 && f.page == CUSTOM_MATTER && !f.action_failed);
    assert(!custom_back(&f));
    /* Zigbee alike. */
    f = (struct custom_flow){.page = CUSTOM_ZIGBEE, .root = CUSTOM_ZIGBEE};
    last = -1; custom_choose(&f, 0, request, 0);
    assert(f.page == CUSTOM_ZIGBEE_CONFIRM && last == -1);
    custom_choose(&f, 1, request, 0); assert(f.page == CUSTOM_ZIGBEE && last == -1);
    custom_choose(&f, 0, request, 0); custom_choose(&f, 0, request, 0);
    assert(f.page == CUSTOM_ZIGBEE_PAIRING && last == 1 && !f.action_failed);
    assert(custom_back(&f) && f.page == CUSTOM_ZIGBEE);
    assert(!custom_back(&f) && custom_choose(&f, 1, request, 0) == 2);
    /* Doom: Play asks for the game and stays; Back leaves. */
    f = (struct custom_flow){.page = CUSTOM_DOOM, .root = CUSTOM_DOOM};
    last = -1;
    assert(custom_choose(&f, 0, request, 0) == 1 && last == 2 && f.page == CUSTOM_DOOM && !f.action_failed);
    fail = 1; custom_choose(&f, 0, request, 0); assert(f.action_failed && f.page == CUSTOM_DOOM);
    fail = 0; assert(custom_choose(&f, 1, request, 0) == 2 && !f.action_failed);
    /* Themes: a choice asks for it and leaves, as Back does; a failed one stays. */
    f = (struct custom_flow){.page = CUSTOM_THEMES, .root = CUSTOM_THEMES, .theme_count = 4};
    for (int choice = 0; choice < f.theme_count; choice++) {
        assert(custom_choose(&f, choice, request, 0) == 2 && last == 3 + choice && !f.action_failed);
    }
    fail = 1;
    assert(custom_choose(&f, 0, request, 0) == 1 && last == 3 && f.action_failed && f.page == CUSTOM_THEMES);
    fail = 0; last = -1;
    assert(custom_choose(&f, f.theme_count, request, 0) == 2 && last == -1);
    f.theme_count = 7;
    assert(custom_choose(&f, 6, request, 0) == 2 && last == 9);
    assert(custom_choose(&f, 8, request, 0) == 1);
    return 0;
}
