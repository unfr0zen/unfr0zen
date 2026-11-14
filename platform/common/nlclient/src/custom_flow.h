#ifndef UNFR0ZEN_FLOW_H
#define UNFR0ZEN_FLOW_H
/* The UNFR0ZEN screen's navigation, independent of the firmware. nestlocal
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

/* The home page's entries, as their actions: Matter, Zigbee, Done, Doom,
 * Themes. They are shown as Matter, Zigbee, Themes, Doom, Done, each but
 * Done only when its feature is on: Doom last of the features. */
enum custom_home_action {
    HOME_MATTER = 0,
    HOME_ZIGBEE = 1,
    HOME_DONE = 2,
    HOME_DOOM = 3,
    HOME_THEMES = 4,
};

/* The features nestlocal enables (/var/run/unfr0zen/features, a hex digit). */
enum custom_feature {
    FEATURE_MATTER = 1,
    FEATURE_ZIGBEE = 2,
    FEATURE_DOOM = 4,
    FEATURE_THEMES = 8,
};

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

struct custom_flow { enum custom_page page; int parent_selection; int action_failed; int theme_count; };

/* Whether the page shows an operation in progress (a pairing). */
static int custom_activity(enum custom_page page) {
    return page == CUSTOM_MATTER_PAIRING || page == CUSTOM_ZIGBEE_PAIRING;
}

static inline int custom_matter(enum custom_page page) {
    return page == CUSTOM_MATTER || page == CUSTOM_MATTER_PAIRING || page == CUSTOM_MATTER_CONFIRM;
}

/* How many entries the home page shows: Done, and one per feature. */
static inline int custom_home_count(unsigned int features) {
    return 1 + !!(features & FEATURE_MATTER) + !!(features & FEATURE_ZIGBEE) +
           !!(features & FEATURE_DOOM) + !!(features & FEATURE_THEMES);
}

/* The action of the home page's visible entry `choice`, or -1. Indices are
 * compacted over the features shown: with Zigbee alone, entry 0 is Zigbee,
 * never Matter. */
static inline int custom_home_action(int choice, unsigned int features) {
    int index = 0;
    if ((features & FEATURE_MATTER) && choice == index++) return HOME_MATTER;
    if ((features & FEATURE_ZIGBEE) && choice == index++) return HOME_ZIGBEE;
    if ((features & FEATURE_THEMES) && choice == index++) return HOME_THEMES;
    if ((features & FEATURE_DOOM) && choice == index++) return HOME_DOOM;
    return choice == index ? HOME_DONE : -1;
}

/* Goes to the page's parent; false on the home page, which has none. */
static int custom_back(struct custom_flow *flow) {
    if (flow->page == CUSTOM_HOME) return 0;
    flow->page = flow->page == CUSTOM_MATTER_CONFIRM ? CUSTOM_MATTER :
                 flow->page == CUSTOM_ZIGBEE_PAIRING || flow->page == CUSTOM_ZIGBEE_CONFIRM ? CUSTOM_ZIGBEE : CUSTOM_HOME;
    flow->action_failed = 0;
    return 1;
}

static inline int starts_with(const char *text, const char *prefix) {
    while (*prefix) { if (*text++ != *prefix++) return 0; }
    return 1;
}

/* Follows Matter's status while pairing; true when the page changed or the
 * pairing started. nestlocal consuming the request acknowledges an attempt
 * even when polling misses its open window. A terminal status waits for
 * one more poll, since the worker removes the request before publishing
 * its new status. */
static inline int custom_matter_refresh(struct custom_flow *flow, const char *status,
                                       int pending, int *started, unsigned int ticks) {
    if (flow->page == CUSTOM_MATTER && starts_with(status, "Pairing open") &&
        !starts_with(status, "Pairing open. Already paired")) {
        flow->page = CUSTOM_MATTER_PAIRING;
        *started = 1;
        return 1;
    }
    if (flow->page != CUSTOM_MATTER_PAIRING) return 0;
    if (starts_with(status, "Matter not enabled") || starts_with(status, "Starting Matter") ||
        starts_with(status, "Not enabled") || starts_with(status, "Unavailable") ||
        (!*started && pending && ticks >= PAIRING_TIMEOUT_TICKS)) {
        flow->page = CUSTOM_MATTER;
        flow->action_failed = 1;
        return 1;
    }
    if (starts_with(status, "Removing controllers")) return 0;
    if (!*started && !pending) {
        *started = 1;
        return 1;
    }
    if (*started && !pending && (starts_with(status, "Paired") || starts_with(status, "Ready to pair") ||
                                starts_with(status, "Pairing timed out"))) {
        flow->page = CUSTOM_MATTER;
        flow->action_failed = 0;
        return 1;
    }
    return 0;
}

/* Acts on the page's entry `choice` (on the home page, its action),
 * asking `request` for what nestlocal does. Returns CHOOSE_EXIT to leave
 * the screen, else CHOOSE_STAY. */
static int custom_choose(struct custom_flow *flow, int choice, int (*request)(int, void *), void *context) {
    if (choice < 0 || choice > (flow->page == CUSTOM_THEMES ? flow->theme_count : CHOICE_ROWS - 1)) return CHOOSE_STAY;
    flow->action_failed = 0;
    if (flow->page == CUSTOM_HOME) {
        if (choice == HOME_DONE) return CHOOSE_EXIT;
        if (choice == HOME_DOOM || choice == HOME_THEMES) {
            flow->page = choice == HOME_DOOM ? CUSTOM_DOOM : CUSTOM_THEMES;
            return CHOOSE_STAY;
        }
        flow->parent_selection = choice;
        flow->page = choice == HOME_MATTER ? CUSTOM_MATTER : CUSTOM_ZIGBEE;
    } else if (flow->page == CUSTOM_THEMES) {
        /* A theme choice returns to the home page; the last entry is Back. */
        if (choice < flow->theme_count) flow->action_failed = request(REQUEST_THEME + choice, context) != 0;
        if (!flow->action_failed) {
            custom_back(flow);
        }
    } else if (flow->page == CUSTOM_DOOM) {
        /* Play: nestlocal gives Doom the screen, which comes back to this page. */
        if (choice == 0) flow->action_failed = request(REQUEST_DOOM, context) != 0;
        else if (choice == 1) custom_back(flow);
    } else if (custom_activity(flow->page)) {
        /* A pairing in progress has Back alone. */
        if (choice == 0) custom_back(flow);
    } else if (flow->page == CUSTOM_MATTER_CONFIRM) {
        if (choice == 0) {
            flow->action_failed = request(REQUEST_MATTER_PAIRING, context) != 0;
            flow->page = flow->action_failed ? CUSTOM_MATTER : CUSTOM_MATTER_PAIRING;
        } else if (choice == 1) custom_back(flow);
    } else if (flow->page == CUSTOM_ZIGBEE_CONFIRM) {
        if (choice == 0) {
            flow->action_failed = request(REQUEST_ZIGBEE_PAIRING, context) != 0;
            flow->page = flow->action_failed ? CUSTOM_ZIGBEE : CUSTOM_ZIGBEE_PAIRING;
        } else if (choice == 1) custom_back(flow);
    } else if (choice == 0) {
        /* Matter's or Zigbee's page: Enable pairing, then confirm. */
        if (flow->page == CUSTOM_MATTER) {
            flow->page = CUSTOM_MATTER_CONFIRM;
        } else flow->page = CUSTOM_ZIGBEE_CONFIRM;
    } else if (choice == 1) custom_back(flow);
    return CHOOSE_STAY;
}
#endif
