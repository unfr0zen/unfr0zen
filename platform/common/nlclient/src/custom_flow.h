#ifndef UNFR0ZEN_FLOW_H
#define UNFR0ZEN_FLOW_H
/* unfr0zen's settings screens' navigation, independent of the firmware. nestlocal
 * acknowledges the actions it requests. */

enum custom_page {
    CUSTOM_HOME,
    CUSTOM_MATTER,
    CUSTOM_ZIGBEE,
    CUSTOM_ZIGBEE_CONFIRM,
    CUSTOM_MATTER_PAIRING,
    CUSTOM_ZIGBEE_PAIRING,
    CUSTOM_MATTER_CONFIRM,
    CUSTOM_DOOM,
    CUSTOM_THEMES,
};

/* The settings menu's entries, each a stock screen whose factory builds
 * this one, and the page it opens on: the Zigbee, Matter, Themes and Doom
 * pages, and the UNFR0ZEN page (the icon and version) in Legal Info's place. */
static inline int custom_same(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}
static inline enum custom_page custom_root(const char *name) {
    return custom_same(name, "account")         ? CUSTOM_ZIGBEE
           : custom_same(name, "update")        ? CUSTOM_MATTER
           : custom_same(name, "proinfoscreen") ? CUSTOM_THEMES
           : custom_same(name, "templock")      ? CUSTOM_DOOM
                                                : CUSTOM_HOME;
}

/* What the screen asks nestlocal for: Matter's pairing, Zigbee's, Doom's
 * launch, and from REQUEST_THEME on the theme page's choice. */
enum custom_request {
    REQUEST_MATTER_PAIRING = 0,
    REQUEST_ZIGBEE_PAIRING = 1,
    REQUEST_DOOM = 2,
    REQUEST_THEME = 3,
};

/* custom_choose's results: stay on the screen, or leave it. */
enum {
    CHOOSE_STAY = 1,
    CHOOSE_EXIT = 2,
};

/* Choice rows on every page but Themes, which has one per theme and Back. */
#define CHOICE_ROWS 5

/* A pairing that nestlocal has not acknowledged after this many status
 * polls has failed. */
#define PAIRING_TIMEOUT_TICKS 120

struct custom_flow {
    enum custom_page page;
    enum custom_page root;
    int action_failed;
    int theme_count;
};

/* Whether the page shows an operation in progress (a pairing). */
static int custom_activity(enum custom_page page) {
    return page == CUSTOM_MATTER_PAIRING || page == CUSTOM_ZIGBEE_PAIRING;
}

static inline int custom_matter(enum custom_page page) {
    return page == CUSTOM_MATTER || page == CUSTOM_MATTER_PAIRING || page == CUSTOM_MATTER_CONFIRM;
}

/* Goes to the page's parent; false where that is the settings menu: the
 * entry's own page, and Matter's pairing. */
static int custom_back(struct custom_flow *flow) {
    /* Matter's page shows its pairing again while the window is open
     * (custom_matter_refresh), so the pairing's Back leaves too. */
    if (flow->page == flow->root || flow->page == CUSTOM_MATTER_PAIRING)
        return 0;
    flow->page = custom_matter(flow->page)                                                    ? CUSTOM_MATTER
                 : flow->page == CUSTOM_ZIGBEE_PAIRING || flow->page == CUSTOM_ZIGBEE_CONFIRM ? CUSTOM_ZIGBEE
                                                                                              : flow->root;
    flow->action_failed = 0;
    return 1;
}

/* Back on a page: its parent, or out to the settings menu. */
static int custom_back_choice(struct custom_flow *flow) { return custom_back(flow) ? CHOOSE_STAY : CHOOSE_EXIT; }

static inline int starts_with(const char *text, const char *prefix) {
    while (*prefix) {
        if (*text++ != *prefix++)
            return 0;
    }
    return 1;
}

/* Follows Matter's status while pairing; true when the page changed or the
 * pairing started. nestlocal consuming the request acknowledges an attempt
 * even when polling misses its open window. A terminal status waits for
 * one more poll, since the worker removes the request before publishing
 * its new status. */
static inline int custom_matter_refresh(struct custom_flow *flow, const char *status, int pending, int *started,
                                        unsigned int ticks) {
    if (flow->page == CUSTOM_MATTER && starts_with(status, "Pairing open") &&
        !starts_with(status, "Pairing open. Already paired")) {
        flow->page = CUSTOM_MATTER_PAIRING;
        *started = 1;
        return 1;
    }
    if (flow->page != CUSTOM_MATTER_PAIRING)
        return 0;
    if (starts_with(status, "Matter not enabled") || starts_with(status, "Starting Matter") ||
        starts_with(status, "Not enabled") || starts_with(status, "Unavailable") ||
        (!*started && pending && ticks >= PAIRING_TIMEOUT_TICKS)) {
        flow->page = CUSTOM_MATTER;
        flow->action_failed = 1;
        return 1;
    }
    if (starts_with(status, "Removing controllers"))
        return 0;
    if (!*started && !pending) {
        *started = 1;
        return 1;
    }
    if (*started && !pending &&
        (starts_with(status, "Paired") || starts_with(status, "Ready to pair") ||
         starts_with(status, "Pairing timed out"))) {
        flow->page = CUSTOM_MATTER;
        flow->action_failed = 0;
        return 1;
    }
    return 0;
}

/* Acts on the page's entry `choice`, asking `request` for what nestlocal
 * does. Returns CHOOSE_EXIT to leave the screen, else CHOOSE_STAY. */
static int custom_choose(struct custom_flow *flow, int choice, int (*request)(int, void *), void *context) {
    if (choice < 0 || choice > (flow->page == CUSTOM_THEMES ? flow->theme_count : CHOICE_ROWS - 1))
        return CHOOSE_STAY;
    flow->action_failed = 0;
    if (flow->page == CUSTOM_HOME) {
        /* Done. */
        if (choice == 0)
            return CHOOSE_EXIT;
    } else if (flow->page == CUSTOM_THEMES) {
        /* A theme choice goes back as Back does; the last entry is Back. */
        if (choice < flow->theme_count)
            flow->action_failed = request(REQUEST_THEME + choice, context) != 0;
        if (!flow->action_failed)
            return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_DOOM) {
        /* Play: nestlocal gives Doom the screen, which comes back to this page. */
        if (choice == 0)
            flow->action_failed = request(REQUEST_DOOM, context) != 0;
        else if (choice == 1)
            return custom_back_choice(flow);
    } else if (custom_activity(flow->page)) {
        /* A pairing in progress has Back alone. */
        if (choice == 0)
            return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_MATTER_CONFIRM) {
        if (choice == 0) {
            flow->action_failed = request(REQUEST_MATTER_PAIRING, context) != 0;
            flow->page = flow->action_failed ? CUSTOM_MATTER : CUSTOM_MATTER_PAIRING;
        } else if (choice == 1)
            return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_ZIGBEE_CONFIRM) {
        if (choice == 0) {
            flow->action_failed = request(REQUEST_ZIGBEE_PAIRING, context) != 0;
            flow->page = flow->action_failed ? CUSTOM_ZIGBEE : CUSTOM_ZIGBEE_PAIRING;
        } else if (choice == 1)
            return custom_back_choice(flow);
    } else if (choice == 0) {
        /* Matter's or Zigbee's page: Enable pairing, then confirm. */
        flow->page = flow->page == CUSTOM_MATTER ? CUSTOM_MATTER_CONFIRM : CUSTOM_ZIGBEE_CONFIRM;
    } else if (choice == 1)
        return custom_back_choice(flow);
    return CHOOSE_STAY;
}
#endif
