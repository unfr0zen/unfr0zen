#include <assert.h>
#include <string.h>
#include "../src/custom_flow.h"
static int last = -1, fail = 0;
/* The laptop a forget request names: the Forget page's, read from the flow
 * the request's context is. */
static char forgotten[33];
static int request(int action, void *context) {
    last = action;
    forgotten[0] = 0;
    if (action == REQUEST_UPDATE_FORGET && context)
        strncpy(forgotten, ((struct custom_flow *)context)->forget.id, sizeof(forgotten) - 1);
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
#define UPDATE_FLOW {.page = CUSTOM_UPDATE, .root = CUSTOM_UPDATE}
static void update_flow_test(void) {
    struct custom_flow f = UPDATE_FLOW;
    int started = 0;
    last = -1; fail = 0;
    /* Allow asks nestlocal for the window and shows its code. */
    assert(custom_choose(&f, 0, request, &f) == 1 && last == REQUEST_UPDATE && f.page == CUSTOM_UPDATE_OPEN &&
           !f.action_failed);
    /* An earlier window's Closed is stale until nestlocal takes the request,
     * and for one poll after, since it takes it before opening the window. */
    assert(!custom_update_refresh(&f, "Closed", 1, &started, 1));
    assert(f.page == CUSTOM_UPDATE_OPEN && !started);
    assert(custom_update_refresh(&f, "Closed", 0, &started, 2));
    assert(started && f.page == CUSTOM_UPDATE_OPEN);
    assert(!custom_update_refresh(&f, "Open. Enter the code in unfr0zen.", 0, &started, 3));
    assert(!custom_update_refresh(&f, "Receiving update...", 0, &started, 4));
    assert(!custom_update_refresh(&f, "Restarting...", 0, &started, 5));
    assert(f.page == CUSTOM_UPDATE_OPEN);
    assert(custom_update_refresh(&f, "Closed", 0, &started, 6));
    assert(f.page == CUSTOM_UPDATE && !f.action_failed);
    f.page = CUSTOM_UPDATE_OPEN;
    assert(custom_update_refresh(&f, "Refused: not an ARM executable", 0, &started, 7));
    assert(f.page == CUSTOM_UPDATE && !f.action_failed);
    /* A restarted nestlocal says Ready: its window is gone. */
    f.page = CUSTOM_UPDATE_OPEN;
    assert(custom_update_refresh(&f, "Ready", 0, &started, 8) && f.page == CUSTOM_UPDATE);
    f.page = CUSTOM_UPDATE_OPEN; started = 0;
    assert(custom_update_refresh(&f, "Update not enabled", 1, &started, 1));
    assert(f.page == CUSTOM_UPDATE && f.action_failed);
    /* A request nestlocal never takes gives up after 30 polls. */
    f.page = CUSTOM_UPDATE_OPEN; f.action_failed = 0; started = 0;
    assert(!custom_update_refresh(&f, "Ready", 1, &started, 29));
    assert(custom_update_refresh(&f, "Ready", 1, &started, 30));
    assert(f.page == CUSTOM_UPDATE && f.action_failed);
    /* Back is the open page's one action, and goes to the Update page,
     * which opens on its Back: Allow there would replace the window. */
    f.page = CUSTOM_UPDATE_OPEN; f.action_failed = 0;
    assert(custom_choose(&f, 0, request, &f) == 1 && f.page == CUSTOM_UPDATE);
    assert(custom_entry_selection(&f, CUSTOM_UPDATE_OPEN) == 2);
    assert(custom_entry_selection(&f, CUSTOM_UPDATE) == 0);
    assert(!custom_update_refresh(&f, "Closed", 0, &started, 1) && f.page == CUSTOM_UPDATE);
    /* News from nestlocal replaces a failed Allow's message. */
    f.action_failed = 1;
    custom_update_changed(&f);
    assert(!f.action_failed);
    f.page = CUSTOM_MATTER; f.action_failed = 1;
    custom_update_changed(&f);
    assert(f.action_failed);
    /* The Update page is the entry's own: Back leaves to the settings menu. */
    f.page = CUSTOM_UPDATE; f.action_failed = 0;
    assert(!custom_back(&f) && f.page == CUSTOM_UPDATE);
    fail = 1; custom_choose(&f, 0, request, &f); assert(f.action_failed && f.page == CUSTOM_UPDATE);
    fail = 0; assert(custom_choose(&f, 2, request, &f) == 2 && f.page == CUSTOM_UPDATE && !f.action_failed);
    custom_choose(&f, 0, request, &f);
    assert(custom_back(&f) && f.page == CUSTOM_UPDATE);
    assert(!custom_back(&f) && f.page == CUSTOM_UPDATE);
    assert(custom_choose(&f, 5, request, &f) == 1 && f.page == CUSTOM_UPDATE);
    /* The Update page shows the code of a window left open by Back, and not
     * once an upload has used it or the window has closed. */
    const char *code = "40718263\n192.168.1.50";
    assert(custom_update_shows_code("Open. Enter the code in unfr0zen.", code));
    assert(!custom_update_shows_code("Open. Enter the code in unfr0zen.", ""));
    assert(!custom_update_shows_code("Refused: not an ARM executable", code));
    assert(!custom_update_shows_code("Receiving update...", code));
    assert(!custom_update_shows_code("Refused: not an ARM executable", ""));
    assert(!custom_update_shows_code("Restarting...", ""));
    assert(!custom_update_shows_code("Closed", code) && !custom_update_shows_code("Ready", code));
}
static const char *const TWO =
    "0123456789abcdef0123456789abcdef desk\n"
    "fedcba9876543210fedcba9876543210 Kitchen laptop\n";
/* The Update page offers Allow update, Allow and trust while fewer than 4
 * laptops are trusted, Trusted laptops while any is, Roll back on trial,
 * and Back: the visible choice maps to its action. */
static void update_page_lists_only_applicable_choices(void) {
    struct custom_flow f = UPDATE_FLOW;
    assert(custom_trusted_parse(&f, "") == 0 && f.laptop_count == 0);
    assert(custom_update_count(&f) == 3);
    assert(custom_update_action(&f, 0) == 0 && custom_update_action(&f, 1) == 1 &&
           custom_update_action(&f, 2) == 4 && custom_update_action(&f, 3) == -1);
    assert(custom_trusted_parse(&f, TWO) == 1 && f.laptop_count == 2);
    assert(!strcmp(f.laptops[1].id, "fedcba9876543210fedcba9876543210") && !strcmp(f.laptops[1].name, "Kitchen laptop"));
    assert(custom_trusted_parse(&f, TWO) == 0);
    assert(custom_update_count(&f) == 4);
    assert(custom_update_action(&f, 2) == 2 && custom_update_action(&f, 3) == 4);
    assert(custom_update_choice(&f, 2) == 2 && custom_update_choice(&f, 3) == -1 && custom_update_choice(&f, 4) == 3);
    /* A full list offers no trust. */
    const char *full =
        "00000000000000000000000000000000 a\n11111111111111111111111111111111 b\n"
        "22222222222222222222222222222222 c\n33333333333333333333333333333333 d\n"
        "44444444444444444444444444444444 e\n";
    assert(custom_trusted_parse(&f, full) == 1 && f.laptop_count == 4);
    assert(custom_update_count(&f) == 3);
    assert(custom_update_action(&f, 0) == 0 && custom_update_action(&f, 1) == 2 && custom_update_action(&f, 2) == 4);
    assert(custom_update_choice(&f, 1) == -1);
    /* Lines that are not "<32 lowercase hex> <name>" are skipped; a name
     * keeps its first 32 printable characters. */
    const char *odd =
        "0123456789ABCDEF0123456789abcdef upper\n"
        "0123456789abcdef0123456789abcde short\n"
        "0123456789abcdef0123456789abcdef\n"
        "0123456789abcdef0123456789abcdef \n"
        "0123456789abcdef0123456789abcdef bad\001name\n"
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0123456789012345678901234567890123456789\n"
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb last";
    assert(custom_trusted_parse(&f, odd) == 1 && f.laptop_count == 2);
    assert(!strcmp(f.laptops[0].id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert(!strcmp(f.laptops[0].name, "01234567890123456789012345678901"));
    assert(!strcmp(f.laptops[1].name, "last"));
    /* Allow and trust asks for a window with trust, and opens its page. */
    custom_trusted_parse(&f, TWO);
    last = -1; fail = 0;
    assert(custom_choose(&f, 1, request, &f) == 1 && last == REQUEST_UPDATE_TRUST && !forgotten[0] &&
           f.page == CUSTOM_UPDATE_OPEN);
    assert(custom_back(&f) && f.page == CUSTOM_UPDATE);
    /* Back is selected on arriving from the open page, wherever it is. */
    assert(custom_entry_selection(&f, CUSTOM_UPDATE_OPEN) == 3);
    fail = 1;
    assert(custom_choose(&f, 1, request, &f) == 1 && f.action_failed && f.page == CUSTOM_UPDATE);
    fail = 0; last = -1;
    assert(custom_choose(&f, 0, request, &f) == 1 && last == REQUEST_UPDATE && f.page == CUSTOM_UPDATE_OPEN);
    custom_back(&f);
    last = -1;
    assert(custom_choose(&f, 3, request, &f) == 2 && last == -1 && f.page == CUSTOM_UPDATE);
}
/* Trusted laptops lists them by name, then Back; one opens Forget, whose
 * Forget asks nestlocal to forget that laptop's id. */
static void trusted_list_opens_forget_and_forget_requests_with_the_id(void) {
    struct custom_flow f = UPDATE_FLOW;
    custom_trusted_parse(&f, TWO);
    last = -1; fail = 0;
    assert(custom_choose(&f, 2, request, &f) == 1 && last == -1 && f.page == CUSTOM_UPDATE_TRUSTED);
    assert(custom_trusted_count(&f) == 3);
    /* Back (after the two names) returns to the Update page. */
    assert(custom_choose(&f, 2, request, &f) == 1 && f.page == CUSTOM_UPDATE);
    custom_choose(&f, 2, request, &f);
    assert(custom_choose(&f, 1, request, &f) == 1 && last == -1 && f.page == CUSTOM_UPDATE_FORGET);
    assert(!strcmp(f.forget.name, "Kitchen laptop"));
    assert(custom_choose(&f, 1, request, &f) == 1 && last == -1 && f.page == CUSTOM_UPDATE_TRUSTED);
    custom_choose(&f, 1, request, &f);
    assert(custom_back(&f) && f.page == CUSTOM_UPDATE_TRUSTED);
    custom_choose(&f, 1, request, &f);
    fail = 1;
    assert(custom_choose(&f, 0, request, &f) == 1 && last == REQUEST_UPDATE_FORGET && f.action_failed &&
           f.page == CUSTOM_UPDATE_FORGET);
    fail = 0; last = -1;
    assert(custom_choose(&f, 0, request, &f) == 1 && last == REQUEST_UPDATE_FORGET && !f.action_failed);
    assert(!strcmp(forgotten, "fedcba9876543210fedcba9876543210"));
    assert(f.page == CUSTOM_UPDATE);
    /* The Update page opens on Back after Forget, and from the list: Allow
     * there would open a window by a second press. */
    assert(custom_entry_selection(&f, CUSTOM_UPDATE_FORGET) == 3);
    assert(custom_entry_selection(&f, CUSTOM_UPDATE_TRUSTED) == 3);
    /* A list that loses the Forget page's laptop leaves the page; an empty
     * list leaves the list too. */
    custom_choose(&f, 2, request, &f);
    custom_choose(&f, 0, request, &f);
    assert(f.page == CUSTOM_UPDATE_FORGET && !strcmp(f.forget.name, "desk"));
    assert(custom_trusted_parse(&f, "fedcba9876543210fedcba9876543210 Kitchen laptop\n"));
    assert(custom_trusted_changed(&f) && f.page == CUSTOM_UPDATE_TRUSTED);
    assert(!custom_trusted_changed(&f) && f.page == CUSTOM_UPDATE_TRUSTED);
    custom_choose(&f, 0, request, &f);
    assert(f.page == CUSTOM_UPDATE_FORGET);
    assert(custom_trusted_parse(&f, "fedcba9876543210fedcba9876543210 renamed\n"));
    assert(!custom_trusted_changed(&f) && f.page == CUSTOM_UPDATE_FORGET);
    assert(custom_trusted_parse(&f, ""));
    assert(custom_trusted_changed(&f) && f.page == CUSTOM_UPDATE);
    assert(custom_update_choice(&f, 2) == -1);
    assert(custom_entry_selection(&f, CUSTOM_UPDATE_FORGET) == 2);
    assert(custom_choose(&f, 2, request, &f) == 2 && f.page == CUSTOM_UPDATE);
}
/* Roll back shows while a set is on trial, before Back, and asks nestlocal
 * to roll it back; the page stays. */
static void roll_back_shows_only_on_trial(void) {
    struct custom_flow f = UPDATE_FLOW;
    custom_trusted_parse(&f, TWO);
    assert(custom_update_choice(&f, 3) == -1);
    f.on_trial = 1;
    assert(custom_update_count(&f) == 5);
    assert(custom_update_action(&f, 3) == 3 && custom_update_action(&f, 4) == 4);
    assert(custom_entry_selection(&f, CUSTOM_UPDATE_OPEN) == 4);
    last = -1; fail = 0;
    assert(custom_choose(&f, 3, request, &f) == 1 && last == REQUEST_UPDATE_ROLLBACK && !forgotten[0] &&
           f.page == CUSTOM_UPDATE && !f.action_failed);
    fail = 1;
    assert(custom_choose(&f, 3, request, &f) == 1 && f.action_failed && f.page == CUSTOM_UPDATE);
    fail = 0;
    custom_trusted_parse(&f, "");
    assert(custom_update_count(&f) == 4 && custom_update_action(&f, 2) == 3);
    f.on_trial = 0;
    assert(custom_update_choice(&f, 3) == -1 && custom_update_count(&f) == 3);
    last = -1;
    assert(custom_choose(&f, 2, request, &f) == 2 && last == -1 && f.page == CUSTOM_UPDATE);
}
int main(void) {
    matter_refresh_test();
    update_flow_test();
    update_page_lists_only_applicable_choices();
    trusted_list_opens_forget_and_forget_requests_with_the_id();
    roll_back_shows_only_on_trial();
    /* Each entry's stock screen name opens its page; Legal Info's is UNFR0ZEN's. */
    assert(custom_root("account") == CUSTOM_ZIGBEE && custom_root("update") == CUSTOM_MATTER);
    assert(custom_root("proinfoscreen") == CUSTOM_THEMES && custom_root("templock") == CUSTOM_DOOM);
    assert(custom_root("utility") == CUSTOM_UPDATE);
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
        assert(custom_choose(&f, choice, request, 0) == 2 && last == REQUEST_THEME + choice && !f.action_failed);
    }
    fail = 1;
    assert(custom_choose(&f, 0, request, 0) == 1 && last == REQUEST_THEME && f.action_failed && f.page == CUSTOM_THEMES);
    fail = 0; last = -1;
    assert(custom_choose(&f, f.theme_count, request, 0) == 2 && last == -1);
    f.theme_count = 7;
    assert(custom_choose(&f, 6, request, 0) == 2 && last == REQUEST_THEME + 6);
    assert(custom_choose(&f, 8, request, 0) == 1);
    return 0;
}
