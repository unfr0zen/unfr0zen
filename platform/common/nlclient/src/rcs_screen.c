/* The reviewed j49 has the RCS views but omits their registrations. Build
 * them lazily on the UI thread, using the initialized Legal Info layer's
 * dimensions and event target. Native bindings are supplied by the linker. */
extern void *rcs_screen_lookup(void *, const char *);
extern void *rcs_layer_lookup(void *, const char *);
extern void *rcs_layer_new(void *, void *, const char *, int, int);
extern void *rcs_screen_new(void *, void *, const char *);
extern int rcs_add_layer(void *, void *);
extern int rcs_add_screen(void *, void *);
extern int strcmp(const char *, const char *);
extern const void *rcs_associated(const void *);
extern int rcs_string_find(const void *, const void *, int);
extern void rcs_native_post(void *, const void *, int, int, signed char);
extern int rcs_native_read(void *, void *, int);
extern unsigned rcs_interval(unsigned);
extern void rcs_string_clear(void *);
struct rcs_poll_descriptor {
    void *fd;
    short wanted, ready;
};
extern int rcs_poll(struct rcs_poll_descriptor *, int, unsigned);

#ifndef PTR
#define PTR(p, n) (*(void **)((char *)(p) + (n)))
#endif
#ifndef WORD
#define WORD(p, n) (*(int *)((char *)(p) + (n)))
#endif

/* The globals' event handler, settings and association registry. */
#define GLOBALS_HANDLER    0xb0
#define GLOBALS_SETTINGS   0x98
#define GLOBALS_REGISTRY   0x9c
/* A stock layer's event destination and dimensions. */
#define LAYER_EVENT_TARGET 0x14
#define LAYER_WIDTH        0x64
#define LAYER_HEIGHT       0x68
/* poll(2)'s POLLIN, and how long each poll waits. */
#define POLL_IN            1
#define POLL_MS            1000

void *custom_factory(void *globals, const char *name) {
    void *screen = rcs_screen_lookup(globals, name);
    if (screen || strcmp(name, "remotecomfortscreen"))
        return screen;
    void *template = rcs_layer_lookup(globals, "legalinfolayer");
    if (!template)
        return 0;
    void *layer = rcs_layer_lookup(globals, "remotecomfortlayer");
    if (!layer) {
        layer = rcs_layer_new(globals, PTR(template, LAYER_EVENT_TARGET), "remotecomfortlayer",
                              WORD(template, LAYER_WIDTH), WORD(template, LAYER_HEIGHT));
        if (!layer)
            return 0;
        rcs_add_layer(globals, layer);
    }
    screen = rcs_screen_new(globals, PTR(globals, GLOBALS_HANDLER), name);
    if (screen)
        rcs_add_screen(globals, screen);
    return screen;
}

/* A TCP acknowledgement must not discard a sample while cloud association is
 * still loading. This is the native post routine's association gate, without
 * the test-device fallback; its queued sample precedes a later selection. */
int rcs_post_reading(void *globals, const void *id, int temperature, int battery, signed char rssi) {
    void *settings = PTR(globals, GLOBALS_SETTINGS);
    if (!settings || !PTR(globals, GLOBALS_REGISTRY) || rcs_string_find(rcs_associated(settings), id, 0) != 1)
        return 0;
    rcs_native_post(globals, id, temperature, battery, rssi);
    return 1;
}

/* TCP writes are not frames. Accumulate a newline/EOF-terminated command,
 * bound both size and idle time, and never acknowledge an abandoned fragment
 * with the preceding connection's reply. The run flag bounds shutdown too. */
int rcs_read_command(void *socket, char *buffer, int size, const volatile int *running, void *reply) {
    int used = 0;
    rcs_string_clear(reply);
    while (*running && used < size - 1) {
        struct rcs_poll_descriptor descriptor = {socket, POLL_IN, 0};
        if (rcs_poll(&descriptor, 1, rcs_interval(POLL_MS)) <= 0 || !*running)
            break;
        int count = rcs_native_read(socket, buffer + used, size - 1 - used);
        if (count == 0) {
            buffer[used] = 0;
            return used;
        }
        if (count < 0)
            break;
        int start = used;
        used += count;
        buffer[used] = 0;
        for (int i = start; i < used; i++) {
            if (buffer[i] == '\n')
                return used;
        }
    }
    buffer[0] = 0;
    return 0;
}
