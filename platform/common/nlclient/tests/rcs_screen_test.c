#include <assert.h>
#include <stdio.h>
#include <string.h>

static int globals, template_layer, layer, screen, handler, settings, registry, identifier, socket_fd;
static void *current_layer, *current_screen;
static int no_template, fail_layer, fail_screen, associated, layer_creations, screen_creations, posts;
static void *pointer_at(void *object, int offset) {
    if (object == &globals) {
        if (offset == 0xb0) return &handler;
        if (offset == 0x98) return settings ? &settings : 0;
        if (offset == 0x9c) return registry ? &registry : 0;
    }
    if (object == &template_layer && offset == 0x14) return &handler;
    assert(!"unexpected native pointer offset");
    return 0;
}
static int word_at(void *object, int offset) {
    assert(object == &template_layer && (offset == 0x64 || offset == 0x68));
    return 320;
}
#define PTR(p, n) pointer_at((p), (n))
#define WORD(p, n) word_at((p), (n))
#include "../src/rcs_screen.c"


void *rcs_screen_lookup(void *g, const char *name) {
    assert(g == &globals && name);
    return current_screen;
}
void *rcs_layer_lookup(void *g, const char *name) {
    assert(g == &globals);
    if (!strcmp(name, "legalinfolayer")) return no_template ? 0 : &template_layer;
    assert(!strcmp(name, "remotecomfortlayer"));
    return current_layer;
}
void *rcs_layer_new(void *g, void *target, const char *name, int width, int height) {
    assert(g == &globals && target == &handler && !strcmp(name, "remotecomfortlayer"));
    assert(width == 320 && height == 320);
    layer_creations++;
    return fail_layer ? 0 : &layer;
}
void *rcs_screen_new(void *g, void *target, const char *name) {
    assert(g == &globals && target == &handler && !strcmp(name, "remotecomfortscreen"));
    assert(current_layer == &layer);
    screen_creations++;
    return fail_screen ? 0 : &screen;
}
int rcs_add_layer(void *g, void *value) {
    assert(g == &globals && value == &layer);
    current_layer = value;
    return 0;
}
int rcs_add_screen(void *g, void *value) {
    assert(g == &globals && value == &screen);
    current_screen = value;
    return 0;
}
const void *rcs_associated(const void *value) {
    assert(value == &settings);
    return &associated;
}
int rcs_string_find(const void *list, const void *id, int ignore_case) {
    assert(list == &associated && id == &identifier && ignore_case == 0);
    return associated;
}
void rcs_native_post(void *g, const void *id, int temperature, int battery, signed char rssi) {
    assert(g == &globals && id == &identifier && temperature == 0 && battery == 100 && rssi == -30);
    posts++;
}

static const char *chunks[8];
static int chunk_index, poll_count, clear_count, stop_on_poll, read_error;
static volatile int running;
unsigned rcs_interval(unsigned ms) {
    assert(ms == 1000);
    return 123;
}
int rcs_poll(struct rcs_poll_descriptor *descriptor, int count, unsigned ticks) {
    assert(descriptor->fd == &socket_fd && descriptor->wanted == 1 && descriptor->ready == 0);
    assert(count == 1 && ticks == 123);
    poll_count++;
    if (stop_on_poll) running = 0;
    return chunks[chunk_index] ? 1 : 0;
}
int rcs_native_read(void *fd, void *out, int size) {
    assert(fd == &socket_fd && size > 0);
    if (read_error) return -1;
    const char *data = chunks[chunk_index++];
    assert(data);
    int length = (int)strlen(data);
    if (length > size) length = size;
    memcpy(out, data, (unsigned)length);
    return length;
}
void rcs_string_clear(void *reply) {
    assert(reply);
    ((char *)reply)[0] = 0;
    clear_count++;
}
static void reset_reader(void) {
    memset(chunks, 0, sizeof(chunks));
    chunk_index = poll_count = clear_count = stop_on_poll = read_error = 0;
    running = 1;
}
static void read_tests(void) {
    char buffer[128], reply[16];
    reset_reader();
    chunks[0] = "kryptonitedata "; chunks[1] = "kryptonite.AB -30 "; chunks[2] = "20.0 100\n";
    strcpy(reply, "Done");
    int length = rcs_read_command(&socket_fd, buffer, sizeof(buffer), &running, reply);
    assert(length == (int)strlen("kryptonitedata kryptonite.AB -30 20.0 100\n"));
    assert(!strcmp(buffer, "kryptonitedata kryptonite.AB -30 20.0 100\n"));
    assert(clear_count == 1 && poll_count == 3 && !reply[0]);

    reset_reader(); chunks[0] = "complete"; chunks[1] = "";
    assert(rcs_read_command(&socket_fd, buffer, sizeof(buffer), &running, reply) == 8);
    assert(!strcmp(buffer, "complete"));

    reset_reader(); chunks[0] = "unfinished";
    strcpy(reply, "Done");
    assert(rcs_read_command(&socket_fd, buffer, sizeof(buffer), &running, reply) == 0);
    assert(!buffer[0] && !reply[0] && poll_count == 2);

    reset_reader(); running = 0;
    assert(rcs_read_command(&socket_fd, buffer, sizeof(buffer), &running, reply) == 0 && poll_count == 0);
    reset_reader(); chunks[0] = "data"; stop_on_poll = 1;
    assert(rcs_read_command(&socket_fd, buffer, sizeof(buffer), &running, reply) == 0 && chunk_index == 0);
    reset_reader(); chunks[0] = "data"; read_error = 1;
    assert(rcs_read_command(&socket_fd, buffer, sizeof(buffer), &running, reply) == 0);
    reset_reader(); chunks[0] = "too much data";
    assert(rcs_read_command(&socket_fd, buffer, 8, &running, reply) == 0 && !buffer[0]);
}
int main(void) {
    assert(!custom_factory(&globals, "unrelated-screen") && !layer_creations);
    no_template = 1;
    assert(!custom_factory(&globals, "remotecomfortscreen") && !layer_creations);
    no_template = 0; fail_layer = 1;
    assert(!custom_factory(&globals, "remotecomfortscreen") && layer_creations == 1 && !screen_creations);
    fail_layer = 0; fail_screen = 1;
    assert(!custom_factory(&globals, "remotecomfortscreen") && current_layer == &layer && !current_screen);
    fail_screen = 0;
    assert(custom_factory(&globals, "remotecomfortscreen") == &screen);
    int made_layers = layer_creations, made_screens = screen_creations;
    assert(custom_factory(&globals, "remotecomfortscreen") == &screen);
    assert(layer_creations == made_layers && screen_creations == made_screens);

    assert(!rcs_post_reading(&globals, &identifier, 0, 100, -30) && !posts);
    settings = registry = 1;
    assert(!rcs_post_reading(&globals, &identifier, 0, 100, -30) && !posts);
    associated = 1;
    assert(rcs_post_reading(&globals, &identifier, 0, 100, -30) && posts == 1);
    read_tests();
    puts("RCS screen, association and framed-input tests passed");
    return 0;
}
