#include <assert.h>
#include "../src/custom_flow.h"
static int last = -1, fail = 0;
static int request(int action) { last = action; return fail; }
int main(void) {
    const int actions[8][4] = {{2, -1, -1, -1}, {0, 2, -1, -1}, {1, 2, -1, -1}, {0, 1, 2, -1},
                               {3, 2, -1, -1}, {0, 3, 2, -1}, {1, 3, 2, -1}, {0, 1, 3, 2}};
    for (unsigned int mask = 0; mask < 8; mask++) {
        for (int i = 0; i < 4; i++) assert(custom_home_action(i, mask) == actions[mask][i]);
        assert(custom_home_action(custom_home_count(mask) - 1, mask) == 2);
        assert(custom_home_action(-1, mask) == -1);
    }
    struct custom_flow f = {0};
    assert(custom_choose(&f, 0, request) == 1 && f.page == CUSTOM_MATTER);
    assert(custom_choose(&f, 0, request) == 1 && last == 0 && f.page == CUSTOM_MATTER_PAIRING);
    assert(custom_choose(&f, 0, request) == 1 && f.page == CUSTOM_MATTER);
    fail = 1; custom_choose(&f, 0, request); assert(f.action_failed);
    assert(custom_back(&f) && f.page == CUSTOM_HOME && !f.action_failed);
    custom_choose(&f, 1, request); assert(f.page == CUSTOM_ZIGBEE);
    last = -1; custom_choose(&f, 0, request);
    assert(f.page == CUSTOM_ZIGBEE_CONFIRM && last == -1);
    custom_choose(&f, 1, request); assert(f.page == CUSTOM_ZIGBEE && last == -1);
    custom_choose(&f, 0, request); fail = 0; custom_choose(&f, 0, request);
    assert(f.page == CUSTOM_ZIGBEE_PAIRING && last == 1 && !f.action_failed);
    assert(custom_back(&f) && f.page == CUSTOM_ZIGBEE);
    custom_back(&f); assert(f.page == CUSTOM_HOME && f.parent_selection == 1);
    assert(!custom_back(&f)); assert(custom_choose(&f, 2, request) == 2);
    last = -1; assert(custom_choose(&f, 3, request) == 1 && last == 2 && f.page == CUSTOM_HOME && !f.action_failed);
    fail = 1; custom_choose(&f, 3, request); assert(f.action_failed && f.page == CUSTOM_HOME);
    return 0;
}
