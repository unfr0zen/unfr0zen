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
    CUSTOM_UPDATE,
    CUSTOM_UPDATE_OPEN,
    CUSTOM_UPDATE_TRUSTED,
    CUSTOM_UPDATE_FORGET,
};

/* The settings menu's entries, each a stock screen whose factory builds
 * this one, and the page it opens on: the Zigbee, Matter, Themes, Doom and
 * Update pages, and the UNFR0ZEN page (the icon and version) in Legal Info's
 * place. */
static inline int custom_same(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return *a == *b;
}
static inline enum custom_page custom_root(const char *name) {
    return custom_same(name, "account") ? CUSTOM_ZIGBEE : custom_same(name, "update") ? CUSTOM_MATTER :
           custom_same(name, "proinfoscreen") ? CUSTOM_THEMES : custom_same(name, "templock") ? CUSTOM_DOOM :
           custom_same(name, "utility") ? CUSTOM_UPDATE : CUSTOM_HOME;
}

/* What the screen asks nestlocal for: Matter's pairing, Zigbee's, Doom's
 * launch, the update window, with trust, the set on trial's roll back,
 * forgetting the laptop the Forget page names, and from REQUEST_THEME on the
 * theme page's choice. */
enum custom_request {
    REQUEST_MATTER_PAIRING = 0,
    REQUEST_ZIGBEE_PAIRING = 1,
    REQUEST_DOOM = 2,
    REQUEST_UPDATE = 3,
    REQUEST_UPDATE_TRUST = 4,
    REQUEST_UPDATE_ROLLBACK = 5,
    REQUEST_UPDATE_FORGET = 6,
    REQUEST_THEME = 7,
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

/* nestlocal trusts at most this many laptops. */
#define CUSTOM_LAPTOPS 4
/* A trusted laptop: its id (32 lowercase hex digits) and its name (at most
 * 32 printable characters), each terminated. */
struct custom_laptop { char id[33]; char name[33]; };
struct custom_flow {
    enum custom_page page; enum custom_page root; int action_failed; int theme_count;
    /* The Update page's state, as nestlocal publishes it: a set is on
     * trial, and the laptops it trusts. */
    int on_trial;
    int laptop_count;
    struct custom_laptop laptops[CUSTOM_LAPTOPS];
    /* The laptop the Forget page names. */
    struct custom_laptop forget;
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
    if (flow->page == flow->root || flow->page == CUSTOM_MATTER_PAIRING) return 0;
    flow->page = custom_matter(flow->page) ? CUSTOM_MATTER :
                 flow->page == CUSTOM_ZIGBEE_PAIRING || flow->page == CUSTOM_ZIGBEE_CONFIRM ? CUSTOM_ZIGBEE :
                 flow->page == CUSTOM_UPDATE_OPEN || flow->page == CUSTOM_UPDATE_TRUSTED ? CUSTOM_UPDATE :
                 flow->page == CUSTOM_UPDATE_FORGET ? CUSTOM_UPDATE_TRUSTED : flow->root;
    flow->action_failed = 0;
    return 1;
}

/* Back on a page: its parent, or out to the settings menu. */
static int custom_back_choice(struct custom_flow *flow) {
    return custom_back(flow) ? CHOOSE_STAY : CHOOSE_EXIT;
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

/* The update window's open page. nestlocal takes the request before it
 * opens the window, so an earlier window's Closed or Refused is stale while
 * the request is pending and for one poll after. The page returns to Update
 * when the window closes, an upload is refused, or nestlocal restarts (its
 * Ready means the window is gone). */
static inline int custom_update_refresh(struct custom_flow *flow, const char *status,
                                        int pending, int *started, unsigned int ticks) {
    if (flow->page != CUSTOM_UPDATE_OPEN) return 0;
    if (starts_with(status, "Update not enabled") || starts_with(status, "Unavailable") ||
        (pending && ticks >= 30)) {
        flow->page = CUSTOM_UPDATE;
        flow->action_failed = 1;
        return 1;
    }
    if (pending) return 0;
    if (!*started) {
        *started = 1;
        return 1;
    }
    if (starts_with(status, "Closed") || starts_with(status, "Refused:") || starts_with(status, "Ready")) {
        flow->page = CUSTOM_UPDATE;
        flow->action_failed = 0;
        return 1;
    }
    return 0;
}

/* Whether the Update page shows the window's code: a window left by Back,
 * or opened from the network, stays open, and Allow would replace it.
 * nestlocal removes the code once an upload authenticates with it, and
 * when the window closes. */
static inline int custom_update_shows_code(const char *status, const char *code) {
    return *code && starts_with(status, "Open.");
}

/* The Update page, when nestlocal's status or code changes: the news
 * replaces a failed Allow's message, so an open window's code shows. */
static inline void custom_update_changed(struct custom_flow *flow) {
    if (flow->page == CUSTOM_UPDATE) flow->action_failed = 0;
}

static inline void custom_copy(char *to, const char *from, unsigned int size) {
    unsigned int n = 0;
    while (from[n] && n < size - 1) { to[n] = from[n]; n++; }
    to[n] = 0;
}

/* Reads nestlocal's trusted list, a line per laptop: its id, a space and
 * its name. A line that is not that is skipped, a name cut to 32
 * characters, and the list to CUSTOM_LAPTOPS. Whether the list changed. */
static inline int custom_trusted_parse(struct custom_flow *flow, const char *text) {
    int count = 0, changed = 0;
    while (*text && count < CUSTOM_LAPTOPS) {
        const char *line = text;
        while (*text && *text != '\n') text++;
        unsigned int length = (unsigned int)(text - line), n = 0;
        if (*text) text++;
        while (n < 32 && n < length && ((line[n] >= '0' && line[n] <= '9') || (line[n] >= 'a' && line[n] <= 'f'))) n++;
        if (n != 32 || length < 34 || line[32] != ' ') continue;
        unsigned int printable = 33;
        while (printable < length && line[printable] >= 0x20 && line[printable] <= 0x7e) printable++;
        if (printable != length) continue;
        char id[33], name[33];
        for (n = 0; n < 32; n++) id[n] = line[n];
        id[32] = 0;
        for (n = 0; n < 32 && 33 + n < length; n++) name[n] = line[33 + n];
        name[n] = 0;
        struct custom_laptop *laptop = &flow->laptops[count++];
        if (count > flow->laptop_count || !custom_same(laptop->id, id) || !custom_same(laptop->name, name)) changed = 1;
        custom_copy(laptop->id, id, sizeof(laptop->id));
        custom_copy(laptop->name, name, sizeof(laptop->name));
    }
    if (count != flow->laptop_count) changed = 1;
    flow->laptop_count = count;
    return changed;
}

/* The Update page's actions: Allow update=0, Allow and trust=1 (while the
 * list has room), Trusted laptops=2 (while any is trusted), Roll back=3
 * (while a set is on trial), Back=4, shown in that order when they apply. */
static inline int custom_update_shows(const struct custom_flow *flow, int action) {
    return action == 1 ? flow->laptop_count < CUSTOM_LAPTOPS : action == 2 ? flow->laptop_count > 0 :
           action == 3 ? flow->on_trial : action == 0 || action == 4;
}
static inline int custom_update_count(const struct custom_flow *flow) {
    int count = 0;
    for (int action = 0; action < 5; action++) count += custom_update_shows(flow, action);
    return count;
}
static inline int custom_update_action(const struct custom_flow *flow, int choice) {
    int index = 0;
    for (int action = 0; action < 5; action++)
        if (custom_update_shows(flow, action) && choice == index++) return action;
    return -1;
}
/* The visible choice of an Update page action, or -1 while it is hidden. */
static inline int custom_update_choice(const struct custom_flow *flow, int action) {
    int index = 0;
    for (int shown = 0; shown < 5; shown++) {
        if (!custom_update_shows(flow, shown)) continue;
        if (shown == action) return index;
        index++;
    }
    return -1;
}

/* The Trusted laptops page: a choice per laptop, then Back. */
static inline int custom_trusted_count(const struct custom_flow *flow) {
    return flow->laptop_count + 1;
}

/* After the list changes: the Forget page leaves once its laptop is gone,
 * and the list page once none is left. Whether the page changed. */
static inline int custom_trusted_changed(struct custom_flow *flow) {
    enum custom_page before = flow->page;
    if (flow->page == CUSTOM_UPDATE_FORGET) {
        int listed = 0;
        for (int i = 0; i < flow->laptop_count; i++) listed |= custom_same(flow->laptops[i].id, flow->forget.id);
        if (!listed) custom_back(flow);
    }
    if (flow->page == CUSTOM_UPDATE_TRUSTED && !flow->laptop_count) custom_back(flow);
    return flow->page != before;
}

/* The choice a page opens on: Back on the Update page reached from the open
 * page (Allow there would replace a window in use) or the trusted list's
 * pages (a second press would open a window), else the first. */
static inline int custom_entry_selection(const struct custom_flow *flow, enum custom_page from) {
    return flow->page == CUSTOM_UPDATE && (from == CUSTOM_UPDATE_OPEN || from == CUSTOM_UPDATE_TRUSTED ||
                                           from == CUSTOM_UPDATE_FORGET) ? custom_update_choice(flow, 4) : 0;
}

/* Acts on the page's entry `choice`, asking `request` for what nestlocal
 * does. Returns CHOOSE_EXIT to leave the screen, else CHOOSE_STAY. */
static int custom_choose(struct custom_flow *flow, int choice, int (*request)(int, void *), void *context) {
    if (choice < 0 || choice > (flow->page == CUSTOM_THEMES ? flow->theme_count : CHOICE_ROWS - 1)) return CHOOSE_STAY;
    flow->action_failed = 0;
    if (flow->page == CUSTOM_HOME) {
        /* Done. */
        if (choice == 0) return CHOOSE_EXIT;
    } else if (flow->page == CUSTOM_THEMES) {
        /* A theme choice goes back as Back does; the last entry is Back. */
        if (choice < flow->theme_count) flow->action_failed = request(REQUEST_THEME + choice, context) != 0;
        if (!flow->action_failed) return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_DOOM) {
        /* Play: nestlocal gives Doom the screen, which comes back to this page. */
        if (choice == 0) flow->action_failed = request(REQUEST_DOOM, context) != 0;
        else if (choice == 1) return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_UPDATE) {
        int action = custom_update_action(flow, choice);
        if (action == 0 || action == 1) {
            flow->action_failed = request(action ? REQUEST_UPDATE_TRUST : REQUEST_UPDATE, context) != 0;
            flow->page = flow->action_failed ? CUSTOM_UPDATE : CUSTOM_UPDATE_OPEN;
        } else if (action == 2) flow->page = CUSTOM_UPDATE_TRUSTED;
        else if (action == 3) flow->action_failed = request(REQUEST_UPDATE_ROLLBACK, context) != 0;
        else if (action == 4) return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_UPDATE_TRUSTED) {
        /* A laptop opens Forget, which names it; the last entry is Back. */
        if (choice < flow->laptop_count) {
            custom_copy(flow->forget.id, flow->laptops[choice].id, sizeof(flow->forget.id));
            custom_copy(flow->forget.name, flow->laptops[choice].name, sizeof(flow->forget.name));
            flow->page = CUSTOM_UPDATE_FORGET;
        } else if (choice == flow->laptop_count) return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_UPDATE_FORGET) {
        /* Forget: nestlocal takes the laptop off its list within a second. */
        if (choice == 0) {
            flow->action_failed = request(REQUEST_UPDATE_FORGET, context) != 0;
            if (!flow->action_failed) flow->page = CUSTOM_UPDATE;
        } else if (choice == 1) return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_UPDATE_OPEN) {
        /* The open window's page has Back alone. */
        if (choice == 0) return custom_back_choice(flow);
    } else if (custom_activity(flow->page)) {
        /* A pairing in progress has Back alone. */
        if (choice == 0) return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_MATTER_CONFIRM) {
        if (choice == 0) {
            flow->action_failed = request(REQUEST_MATTER_PAIRING, context) != 0;
            flow->page = flow->action_failed ? CUSTOM_MATTER : CUSTOM_MATTER_PAIRING;
        } else if (choice == 1) return custom_back_choice(flow);
    } else if (flow->page == CUSTOM_ZIGBEE_CONFIRM) {
        if (choice == 0) {
            flow->action_failed = request(REQUEST_ZIGBEE_PAIRING, context) != 0;
            flow->page = flow->action_failed ? CUSTOM_ZIGBEE : CUSTOM_ZIGBEE_PAIRING;
        } else if (choice == 1) return custom_back_choice(flow);
    } else if (choice == 0) {
        /* Matter's or Zigbee's page: Enable pairing, then confirm. */
        flow->page = flow->page == CUSTOM_MATTER ? CUSTOM_MATTER_CONFIRM : CUSTOM_ZIGBEE_CONFIRM;
    } else if (choice == 1) return custom_back_choice(flow);
    return CHOOSE_STAY;
}
#endif
