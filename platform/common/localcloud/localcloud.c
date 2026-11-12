/* Preloaded into nlclient on a unit running nestlocal, which serves
 * nlclient's cloud on the unit itself (127.0.0.1). nlclient opens its cloud
 * connection only once its network layer reports a connected network, so
 * without Wi-Fi it would never reach nestlocal, and nothing nestlocal offers
 * (Zigbee, Matter, the external temperature) would ever get the thermostat's
 * state. Loopback is always up: this reports the network connected.
 *
 * nlclient learns of its network from libnlnetworkmanager's events, read
 * through nlNetworkEvent::Connected, and asks nlNetworkConnectivity whether
 * it is connected; both are calls into the library, which the preload takes
 * over. The first event nlclient handles becomes the change to connected,
 * which runs nlclient's own connect path; every event after it is no change,
 * so a Wi-Fi network coming or going later never runs it again. Being always
 * connected also keeps nlclient's network recovery, which scans, powers the
 * radio back on and finally resets the network settings, from ever starting.
 *
 * The same symbols in every nlclient build, gen 2 and gen 3 alike. Built
 * without libc: it calls nothing. */

typedef _Bool bool;

/* bool nlNetworkEvent::Connected::IsConnected() const */
bool _ZNK14nlNetworkEvent9Connected11IsConnectedEv(const void *event) {
    (void)event;
    return 1;
}

static const void *first_event;
static bool past_first;

/* bool nlNetworkEvent::Connected::IsChange() const: true for every look at
 * the first event, false from the first look at another one on. */
bool _ZNK14nlNetworkEvent9Connected8IsChangeEv(const void *event) {
    if (past_first) {
        return 0;
    }
    if (!first_event) {
        first_event = event;
    }
    if (event == first_event) {
        return 1;
    }
    past_first = 1;
    return 0;
}

/* bool nlNetworkConnectivity::IsConnected() const */
bool _ZNK21nlNetworkConnectivity11IsConnectedEv(const void *connectivity) {
    (void)connectivity;
    return 1;
}
