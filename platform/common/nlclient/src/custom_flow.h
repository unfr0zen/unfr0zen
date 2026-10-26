#ifndef UNFR0ZEN_FLOW_H
#define UNFR0ZEN_FLOW_H
/* Firmware-independent navigation. Actions are acknowledged by nestlocal. */
enum custom_page {
    CUSTOM_HOME, CUSTOM_MATTER, CUSTOM_ZIGBEE, CUSTOM_ZIGBEE_CONFIRM,
    CUSTOM_MATTER_PAIRING, CUSTOM_ZIGBEE_PAIRING
};
struct custom_flow { enum custom_page page; int parent_selection; int action_failed; };
static int custom_activity(enum custom_page page) {
    return page == CUSTOM_MATTER_PAIRING || page == CUSTOM_ZIGBEE_PAIRING;
}
static inline int custom_matter(enum custom_page page) {
    return page == CUSTOM_MATTER || page == CUSTOM_MATTER_PAIRING;
}
/* Canonical home actions: Matter=0, Zigbee=1, Done=2, Doom=3, shown as
 * Matter, Zigbee, Doom, Done. Compact the visible list without ever
 * interpreting Zigbee-only index 0 as Matter. */
static inline int custom_home_count(unsigned int features) {
    return 1 + !!(features & 1) + !!(features & 2) + !!(features & 4);
}
static inline int custom_home_action(int choice, unsigned int features) {
    int index = 0;
    if ((features & 1) && choice == index++) return 0;
    if ((features & 2) && choice == index++) return 1;
    if ((features & 4) && choice == index++) return 3;
    return choice == index ? 2 : -1;
}
static int custom_back(struct custom_flow *flow) {
    if (flow->page == CUSTOM_HOME) return 0;
    flow->page = flow->page == CUSTOM_MATTER_PAIRING ? CUSTOM_MATTER :
                 flow->page == CUSTOM_ZIGBEE_PAIRING || flow->page == CUSTOM_ZIGBEE_CONFIRM ? CUSTOM_ZIGBEE : CUSTOM_HOME;
    flow->action_failed = 0;
    return 1;
}
/* Requests: Matter's pairing=0, Zigbee's=1, Doom's launch=2. */
static int custom_choose(struct custom_flow *flow, int choice, int (*request)(int)) {
    if (choice < 0 || choice > 3) return 1;
    flow->action_failed = 0;
    if (flow->page == CUSTOM_HOME) {
        if (choice == 2) return 2;
        /* nestlocal takes the screen over for Doom and gives it back after. */
        if (choice == 3) {
            flow->action_failed = request(2) != 0;
            return 1;
        }
        flow->parent_selection = choice;
        flow->page = choice == 0 ? CUSTOM_MATTER : CUSTOM_ZIGBEE;
    } else if (custom_activity(flow->page)) {
        if (choice == 0) custom_back(flow);
    } else if (flow->page == CUSTOM_ZIGBEE_CONFIRM) {
        if (choice == 0) {
            flow->action_failed = request(1) != 0;
            flow->page = flow->action_failed ? CUSTOM_ZIGBEE : CUSTOM_ZIGBEE_PAIRING;
        } else if (choice == 1) custom_back(flow);
    } else if (choice == 0) {
        if (flow->page == CUSTOM_MATTER) {
            flow->action_failed = request(0) != 0;
            if (!flow->action_failed) flow->page = CUSTOM_MATTER_PAIRING;
        } else flow->page = CUSTOM_ZIGBEE_CONFIRM;
    } else if (choice == 1) custom_back(flow);
    return 1;
}
#endif
