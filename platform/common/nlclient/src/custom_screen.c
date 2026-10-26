/* Native API addresses and object layouts are discovered from the input ELF.
 * No framebuffer drawing: use stock text layout, selection, and image APIs.
 * ABI/layout findings are build-specific and still require guest validation.
 */
#include "custom_flow.h"
typedef unsigned int u32;
typedef unsigned char u8;
/* Object layouts discovered from the input ELF, as absolute symbols the
 * linker defines (layout_NAME = value): one object serves every build. */
/* Numeric layouts must not acquire the PIE load bias. An ABS32 literal holds
 * the discovered number, including offsets within native vtables. */
#define LAYOUT(name) __extension__ ({ u32 value; \
    __asm__("ldr %0, 1f\n b 2f\n .balign 4\n1: .word layout_" #name "\n2:" \
            : "=r" (value)); value; })
extern const u8 layout_ACTIVITY_QUEUE_OFFSET __attribute__((visibility("hidden")));
#define ACTIVITY_QUEUE_OFFSET LAYOUT(ACTIVITY_QUEUE_OFFSET)
extern const u8 layout_BUCKET_GLOBALS_OFFSET __attribute__((visibility("hidden")));
#define BUCKET_GLOBALS_OFFSET LAYOUT(BUCKET_GLOBALS_OFFSET)
extern const u8 layout_COMMON_OFFSET __attribute__((visibility("hidden")));
#define COMMON_OFFSET LAYOUT(COMMON_OFFSET)
extern const u8 layout_GLOBALS_TIMER_OFFSET __attribute__((visibility("hidden")));
#define GLOBALS_TIMER_OFFSET LAYOUT(GLOBALS_TIMER_OFFSET)
extern const u8 layout_LAYER_COMMON_VPTR __attribute__((visibility("hidden")));
#define LAYER_COMMON_VPTR LAYOUT(LAYER_COMMON_VPTR)
extern const u8 layout_LAYER_SECOND_VPTR __attribute__((visibility("hidden")));
#define LAYER_SECOND_VPTR LAYOUT(LAYER_SECOND_VPTR)
extern const u8 layout_LAYER_THIRD_VPTR __attribute__((visibility("hidden")));
#define LAYER_THIRD_VPTR LAYOUT(LAYER_THIRD_VPTR)
extern const u8 layout_LAYER_VTABLE_SIZE __attribute__((visibility("hidden")));
#define LAYER_VTABLE_SIZE LAYOUT(LAYER_VTABLE_SIZE)
extern const u8 layout_LEGACY_CLICK __attribute__((visibility("hidden")));
#define LEGACY_CLICK LAYOUT(LEGACY_CLICK)
extern const u8 layout_PRIMARY_SIZE __attribute__((visibility("hidden")));
#define PRIMARY_SIZE LAYOUT(PRIMARY_SIZE)
extern const u8 layout_RENAME_SYSCALL __attribute__((visibility("hidden")));
#define RENAME_SYSCALL LAYOUT(RENAME_SYSCALL)
extern const u8 layout_SCREEN_SECOND_VPTR __attribute__((visibility("hidden")));
#define SCREEN_SECOND_VPTR LAYOUT(SCREEN_SECOND_VPTR)
extern const u8 layout_SCREEN_THIRD_VPTR __attribute__((visibility("hidden")));
#define SCREEN_THIRD_VPTR LAYOUT(SCREEN_THIRD_VPTR)
extern const u8 layout_SCREEN_TITLE_SLOT __attribute__((visibility("hidden")));
#define SCREEN_TITLE_SLOT LAYOUT(SCREEN_TITLE_SLOT)
extern const u8 layout_SCREEN_VTABLE_SIZE __attribute__((visibility("hidden")));
#define SCREEN_VTABLE_SIZE LAYOUT(SCREEN_VTABLE_SIZE)
extern const u8 layout_TIMER_EVENT_FIRST __attribute__((visibility("hidden")));
#define TIMER_EVENT_FIRST LAYOUT(TIMER_EVENT_FIRST)
extern const u8 layout_TIMER_EVENT_SECOND __attribute__((visibility("hidden")));
#define TIMER_EVENT_SECOND LAYOUT(TIMER_EVENT_SECOND)
/* Room for the copied vtables; the build refuses larger discovered ones. */
#define VTABLE_MAX 0x400
typedef struct { u32 words[7]; } nl_string;
typedef struct {
    int text_count;
    nl_string *text;
    u32 *flags;
    int choice_count;
    nl_string *choices;
    u32 *choice_flags;
    int cache_id;
} text_config;

extern void *native_new(unsigned int);
extern void native_layer_ctor(void *);
extern void native_screen_ctor(void *);
extern const u8 native_screen_vtable[] __attribute__((visibility("hidden")));
extern int native_layer_init(void *, void *, void *, const char *, int, int);
extern int native_screen_init(void *, void *, void *, const char *, void *);
extern void *native_layer_lookup(void *, const char *);
extern int native_attach(void *, void *);
extern void native_string_ctor(nl_string *);
extern void native_string_copy(nl_string *, const char *);
extern void native_config_invalidate(text_config *);
extern void native_config_layout(void *, text_config *, int, int);
extern int native_config_y(text_config *, void *, int, int, int);
extern void native_config_draw(void *, void *, text_config *, int, int, int, int, int, int);
extern void native_select(void *, int);
extern void native_scroll_target(void *, float);
extern void native_scroll_draw(void *, void *, int);
extern int native_style(void *);
extern float native_menu_scale(void *);
extern int native_menu_hidden(void *);
extern void *native_image_load(const char *, int *);
extern int native_image_width(void *);
extern int native_image_height(void *);
extern void native_rebuild_title(void *);
extern unsigned int native_event_type(void *);
extern int native_screen_event(void *, void *, void *);
extern void native_screen_enter(void *);
extern int native_screen_draw(void *, void *);
extern int native_layer_event(void *, void *, void *);
extern int native_layer_wheel(void *, int);
extern void *native_fopen(const char *, const char *);
extern unsigned int native_fread(void *, unsigned int, unsigned int, void *);
extern unsigned int native_fwrite(const void *, unsigned int, unsigned int, void *);
extern int native_fclose(void *);
extern int native_rename(const char *, const char *);
extern int native_send_timeout(void **, unsigned int, unsigned int, void *, void *, void *);
extern void native_cancel_timeout(void *);
extern void native_release(void *);
extern unsigned int native_timeout_id(void *);
extern void native_mark_activity(void *);
extern void *native_activity_client(void *);
extern void native_activity_ctor(void *);
extern void native_activity_init(void *, unsigned int, int);
extern int native_enqueue(void *, const void *, void *);

#define WORD(p, offset) (*(u32 *)((u8 *)(p) + (offset)))
#define PTR(p, offset) (*(void **)((u8 *)(p) + (offset)))
#define CUSTOM_IMAGE "/nestlabs/share/images/G_unfr0zen_192px.png"
#define FALLBACK_IMAGE CUSTOM_IMAGE
#define MENU_GLYPH "/nestlabs/share/images/G_unfr0zen_glyph_80px.png"
struct custom_state {
    struct custom_flow flow;
    int active;
    int at_top;
    unsigned int features;
    void *refresh_request;
    char status[96];
    char code[32];
    char detail[192];
    enum custom_page image_page;
    void *activity_images[8];
    unsigned int activity_ticks;
    int activity_started;
#ifdef CUSTOM_TRACE
    int image_traced;
#endif
    void *screen;
    /* The display's width, which the gen 3's layouts below scale to. */
    int display;
    void *menu_glyph;
    text_config config;
    nl_string text[3];
    nl_string choices[4];
    u32 flags[3];
    const char *choice_ids[5];
    u8 layer_vtables[VTABLE_MAX];
    u8 screen_vtables[VTABLE_MAX];
};
static void bytes_copy(void *to, const void *from, unsigned int count) {
    u8 *dest = to;
    const u8 *source = from;
    while (count--) *dest++ = *source++;
}
static void bytes_zero(void *to, unsigned int count) {
    u8 *dest = to;
    while (count--) *dest++ = 0;
}
static struct custom_state *state(void *raw) {
    return (struct custom_state *)((u8 *)raw + PRIMARY_SIZE);
}
static void *raw_layer(void *screen) { return PTR(screen, 0xec); }
static void *common(void *raw) { return (u8 *)raw + COMMON_OFFSET; }
static void notify_activity(void *globals) {
    void *client = native_activity_client(globals);
    /* This accessor is the stock client's queue. Check the ownership before
     * using the discovered queue field, including on older firmware. */
    if (!client || PTR(client, 0x70) != globals) return;
    void *queue = PTR(client, ACTIVITY_QUEUE_OFFSET);
    if (!queue) return;
    void *event = native_new(0x10);
    if (!event) return;
    native_activity_ctor(event);
    native_mark_activity(globals);
    native_activity_init(event, 0x40000005, 0);
    native_enqueue(queue, event, 0);
}
static void keep_awake(void *raw) {
    notify_activity(PTR(common(raw), 0x8c));
}
#ifdef CUSTOM_TRACE
static void write_char(int value, void *file) {
    u8 byte = (u8)value;
    native_fwrite(&byte, 1, 1, file);
}
static void trace(void *raw, u32 type, u32 value) {
    void *file = native_fopen("/media/user-config/nlcustom-wheel.log", "a");
    if (!file) return;
    u32 fields[] = {type, value, WORD(raw, 0x7c), WORD(common(raw), 0x74),
                    WORD(common(raw), 0x7c), WORD(common(raw), 0x18),
                    WORD(common(raw), 0x14), WORD(raw, 0x64), WORD(raw, 8), WORD(raw, 0x14), WORD(raw, 0x18)};
    for (unsigned int i = 0; i < sizeof(fields) / sizeof(fields[0]); i++) {
        for (int shift = 28; shift >= 0; shift -= 4)
            write_char("0123456789abcdef"[(fields[i] >> shift) & 15], file);
        write_char(i + 1 == sizeof(fields) / sizeof(fields[0]) ? '\n' : ' ', file);
    }
    native_fclose(file);
}
#endif
static int custom_wheel(void *raw, int value) {
    if (value) state(raw)->at_top = 0;
    int result = native_layer_wheel(raw, value);
#ifdef CUSTOM_TRACE
    trace(raw, 0x40000004, value);
#endif
    return result;
}
static void read_text(const char *path, char *out, unsigned int size, const char *fallback) {
    void *file = native_fopen(path, "r");
    unsigned int n = 0;
    if (file) { n = native_fread(out, 1, size - 1, file); native_fclose(file); }
    if (!n) { while (*fallback && n < size - 1) out[n++] = *fallback++; }
    while (n && (out[n-1] == '\n' || out[n-1] == '\r')) n--;
    out[n] = 0;
}
/* The framebuffer's width: 480 on a gen 3, 320 on a gen 2. */
static int display_width(void) {
    char size[24];
    int width = 0;
    read_text("/sys/class/graphics/fb0/virtual_size", size, sizeof(size), "480");
    for (const char *c = size; *c >= '0' && *c <= '9'; c++) width = width * 10 + (*c - '0');
    return width ? width : 480;
}
/* A length of the gen 3's 480-pixel layout, on this display. */
#define SCALED(s, n) ((n) * (s)->display / 480)
static int same_text(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return *a == *b;
}
static int rename_setting(const char *from, const char *to) {
    if (RENAME_SYSCALL) {
        /* ARM Linux EABI __NR_rename = 38. Older ELFs do not import rename;
         * preserve the same atomic filesystem operation without libc internals. */
        register const char *arg0 __asm__("r0") = from;
        register const char *arg1 __asm__("r1") = to;
        register unsigned int number __asm__("r7") = 38;
        __asm__ volatile("svc 0" : "+r"(arg0) : "r"(arg1), "r"(number) : "memory", "cc");
        return (int)arg0;
    }
    return native_rename(from, to);
}
extern u32 native_time(void *);
extern int native_device_commit(void *);
extern int native_shared_commit(void *);
extern int native_schedule_commit(void *);
extern void native_delete(void *);
struct wake_handler {
    void *vtable;
    int (*handle)(struct wake_handler *, void *);
    void *globals;
};
static int light_display(struct wake_handler *handler, void *event) {
    (void)event;
    native_mark_activity(handler->globals);
    native_delete(handler);
    return 5; /* nlUIResultLightDisplay: use the client's normal wake timer. */
}
static void wake_display(void *globals) {
    void *client = native_activity_client(globals);
    if (!client || PTR(client, 0x70) != globals) return;
    struct wake_handler *handler = native_new(sizeof(*handler));
    if (!handler) return;
    void *event = native_new(0x10);
    if (!event) { native_delete(handler); return; }
    handler->vtable = &handler->handle;
    handler->handle = light_display;
    handler->globals = globals;
    native_activity_ctor(event);
    native_activity_init(event, 0x40000005, 0);
    /* nlClient inherits nlEventQueue. Its main thread dispatches the handler
     * and applies the returned UI result before releasing the event. */
    native_enqueue(client, event, handler);
}
static void downlink_activity(void *bucket) {
    const char *taken = "/var/run/unfr0zen/display.taken";
    if (rename_setting("/var/run/unfr0zen/display.request", taken)) return;
    void *file = native_fopen(taken, "rb");
    if (!file) return;
    u32 expires = 0;
    unsigned int count = native_fread(&expires, 1, sizeof(expires), file);
    native_fclose(file);
    u32 now = native_time(0);
    /* Expiry prevents delayed subscribes or a client restart from waking
     * the panel for a command which is no longer current. */
    if (count == sizeof(expires) && expires >= now && expires - now <= 10)
        wake_display(PTR(bucket, BUCKET_GLOBALS_OFFSET));
}
int custom_device_commit(void *bucket) {
    int result = native_device_commit(bucket);
    downlink_activity(bucket);
    return result;
}
int custom_shared_commit(void *bucket) {
    int result = native_shared_commit(bucket);
    downlink_activity(bucket);
    return result;
}
int custom_schedule_commit(void *bucket) {
    int result = native_schedule_commit(bucket);
    downlink_activity(bucket);
    return result;
}
/* 0: Matter's pairing, 1: Zigbee's, 2: Doom's launch. */
static int request_action(int which) {
    char status[96];
    read_text(which == 2 ? "/var/run/unfr0zen/doom.status" : which ? "/var/run/unfr0zen/zigbee.status" :
              "/var/run/unfr0zen/matter.status", status, sizeof(status), "Unavailable");
    /* Disabled/still-starting services cannot acknowledge a command. */
    if (same_text(status, "Unavailable") || same_text(status, "Matter not enabled") ||
        same_text(status, "Zigbee not enabled") || same_text(status, "Starting Matter...") ||
        same_text(status, "Starting Zigbee...") || same_text(status, "Radio unavailable. Try again.")) return -1;
    const char *tmp = which == 2 ? "/var/run/unfr0zen/doom.request.tmp" : which ? "/var/run/unfr0zen/zigbee.request.tmp" :
                      "/var/run/unfr0zen/matter.request.tmp";
    const char *path = which == 2 ? "/var/run/unfr0zen/doom.request" : which ? "/var/run/unfr0zen/zigbee.request" :
                       "/var/run/unfr0zen/matter.request";
    void *file = native_fopen(tmp, "w");
    if (!file) return -1;
    unsigned int written = native_fwrite("1", 1, 1, file);
    int closed = native_fclose(file);
    if (written != 1 || closed != 0) return -1;
    return rename_setting(tmp, path);
}
static void matter_detail(struct custom_state *s) {
    unsigned int n = 0;
    const char *code = s->code;
    const char *status = s->flow.page == CUSTOM_MATTER_PAIRING && !s->activity_started ? "Opening pairing..." : s->status;
    while (*code && n < sizeof(s->detail) - 1) s->detail[n++] = *code++;
    if (n < sizeof(s->detail) - 1) s->detail[n++] = '\n';
    if (n < sizeof(s->detail) - 1) s->detail[n++] = '\n';
    while (*status && n < sizeof(s->detail) - 1) s->detail[n++] = *status++;
    s->detail[n] = 0;
}
static void release_image(void *image) {
    if (image) {
        void (*destroy)(void *) = PTR(PTR(image, 0), 4);
        destroy(image);
    }
}
static void load_image(void *raw) {
    /* Reload on entry so a deployed PNG needs no executable rebuild. */
    release_image(PTR(raw, 0xa0));
    PTR(raw, 0xa0) = 0;
    int error = 0;
    void *image = 0;
    enum custom_page page = state(raw)->flow.page;
    const char *path = custom_matter(page) ? "/var/run/unfr0zen/matter.png" : CUSTOM_IMAGE;
    if (page == CUSTOM_HOME || custom_matter(page)) {
        void *file = native_fopen(path, "rb");
        if (file) { native_fclose(file); image = native_image_load(path, &error); }
    }
    if (image && (native_image_width(image) > 192 || native_image_height(image) > 192)) {
        release_image(image); image = 0;
    }
    state(raw)->image_page = page;
    PTR(raw, 0xa0) = image; /* Missing images leave a usable text-only screen. */
#ifdef CUSTOM_TRACE
    state(raw)->image_traced = 0;
    trace(raw, 0x494d4701, (u32)image);
    trace(raw, 0x494d4702, image ? ((u32)native_image_width(image) << 16) |
                                 (u32)native_image_height(image) : (u32)error);
#endif
}
static int starts_with(const char *text, const char *prefix) {
    while (*prefix) { if (*text++ != *prefix++) return 0; }
    return 1;
}
static void load_activity(void *raw) {
    struct custom_state *s = state(raw);
    char path[] = "/var/run/unfr0zen/activity-0.png";
    for (unsigned int i = 0; i < 8; i++) {
        if (s->activity_images[i]) continue;
        path[sizeof(path) - 6] = '0' + i;
        void *file = native_fopen(path, "rb");
        if (!file) continue;
        native_fclose(file);
        int error = 0;
        s->activity_images[i] = native_image_load(path, &error);
    }
}
static int option_word(const char *text, const char *option) {
    while (*text) {
        while (*text == ' ' || *text == '\n' || *text == '\t' || *text == '\r') text++;
        const char *word = text, *needle = option;
        while (*text && *text != ' ' && *text != '\n' && *text != '\t' && *text != '\r') text++;
        while (word < text && *needle && *word == *needle) { word++; needle++; }
        if (word == text && !*needle) return 1;
    }
    return 0;
}
static unsigned int enabled_features(void) {
    char flags[8];
    read_text("/var/run/unfr0zen/features", flags, sizeof(flags), "");
    if (flags[0] >= '0' && flags[0] <= '7' && !flags[1]) return flags[0] - '0';
    /* Before nestlocal starts, use the same options precedence as its init script.
     * No installed options means no protocol integrations. */
    char options[1024];
    void *installed = native_fopen("/etc/nestlocal/options", "rb");
    const char *path = installed ? "/etc/nestlocal/options" : "/media/system-config/nestlocal.options";
    if (installed) native_fclose(installed);
    read_text(path, options, sizeof(options), "");
    return (option_word(options, "-matter") || option_word(options, "--matter") ? 1 : 0) |
           (option_word(options, "-zigbee") || option_word(options, "--zigbee") ? 2 : 0) |
           (option_word(options, "-doom") || option_word(options, "--doom") ? 4 : 0);
}
static void show(void *raw, int opening) {
    float old_y = *(float *)((u8 *)raw + 8);
    int old_selection = WORD(common(raw), 0x7c);
    struct custom_state *s = state(raw);
    int activity = custom_activity(s->flow.page);
    if (s->flow.page == CUSTOM_HOME) s->features = enabled_features();
    int choice_count = s->flow.page == CUSTOM_HOME ? custom_home_count(s->features) : activity ? 1 : 2;
    native_config_invalidate(&s->config);
    s->flags[0] = s->flags[1] = s->flags[2] = 0;
    s->config.text_count = s->flow.page == CUSTOM_HOME || activity ? 3 : 2;
    /* An image slot is part of the scrolling document, with no page heading.
     * Image positions below come from the actual native text-row anchors. */
    native_string_copy(&s->text[0], s->flow.page == CUSTOM_HOME ? "\n\n\n\n\n" :
                       custom_matter(s->flow.page) ? "\n\n\n\n\n" : "");
    native_string_copy(&s->text[2], "");
    if (s->flow.page == CUSTOM_HOME) {
        native_string_copy(&s->text[1], "This device is running free from Google thanks to unfr0zen by");
        native_string_copy(&s->text[2], "@will-tm");
        s->flags[2] = 0x80000000u; /* Native blue text, without the selection capsule (bit 0). */
        for (int i = 0; i < 4; i++) {
            int action = custom_home_action(i, s->features);
            native_string_copy(&s->choices[i], action == 0 ? "Matter" : action == 1 ? "Zigbee" :
                               action == 2 ? "Done" : action == 3 ? "Doom" : "");
        }
    } else {
        read_text(custom_matter(s->flow.page) ? "/var/run/unfr0zen/matter.status" :
                  "/var/run/unfr0zen/zigbee.status", s->status, sizeof(s->status), "Not enabled or service unavailable");
        read_text("/var/run/unfr0zen/matter.code", s->code, sizeof(s->code), "Code unavailable");
        matter_detail(s);
        native_string_copy(&s->text[1], s->flow.action_failed ? "Service unavailable. Try again." :
                           s->flow.page == CUSTOM_ZIGBEE_CONFIRM ? "Leave current network and pair again? Permit joining on your coordinator first." :
                           custom_matter(s->flow.page) ? s->detail :
                           activity && !s->activity_started ? "Starting pairing..." : s->status);
        if (activity) {
            native_string_copy(&s->text[2], "\n\n");
            native_string_copy(&s->choices[0], "Back");
            native_string_copy(&s->choices[1], "");
            load_activity(raw);
        } else {
            native_string_copy(&s->choices[0], "Enable pairing");
            native_string_copy(&s->choices[1], "Back");
        }
        native_string_copy(&s->choices[2], "");
        native_string_copy(&s->choices[3], "");
        if (s->flow.page == CUSTOM_MATTER && starts_with(s->status, "Pairing open")) {
            native_string_copy(&s->choices[0], "Back");
            native_string_copy(&s->choices[1], "");
            choice_count = 1;
        }
        if (!custom_matter(s->flow.page)) {
            /* Text-only pages start at their first real row. Empty rows still
             * occupy native line height and must not create scroll margins. */
            native_string_copy(&s->text[0], s->flow.action_failed ? "Service unavailable. Try again." :
                               s->flow.page == CUSTOM_ZIGBEE_CONFIRM ? "Leave current network and pair again? Permit joining on your coordinator first." :
                               activity && !s->activity_started ? "Starting pairing..." : s->status);
            native_string_copy(&s->text[1], activity ? "\n\n" : "");
            s->config.text_count = activity ? 2 : 1;
        }
    }
    if (s->image_page != s->flow.page) load_image(raw);
    s->config.choice_count = choice_count;
    void *base = common(raw);
    PTR(base, 0x90) = s->choice_ids;
    WORD(base, 0x94) = 0;
    WORD(base, 0x98) = choice_count - 1;
    native_config_layout(raw, &s->config, 0x10000, choice_count);
    int selection = opening ? (s->flow.page == CUSTOM_HOME ? s->flow.parent_selection : 0) : old_selection;
    if (selection >= choice_count) selection = choice_count - 1;
    if (selection < 0) selection = 0;
    native_select(raw, selection);
    WORD(base, 0x74) = selection;
    if (opening) s->at_top = 1;
    /* Select positions the viewport at its button. Retarget the stock
     * scroller to the document top on entry, or preserve its visible offset
     * on a status refresh. These fields are shared by the audited ABIs. */
    float top = *(float *)((u8 *)raw + 0x18) - *(float *)((u8 *)raw + 0x30);
    float factor = *(float *)((u8 *)raw + 0x84);
    float offset = s->at_top ? 0 : old_y;
    float target = top + (factor > 0 ? offset / factor : 0);
    if (target < 0) target = 0;
    if (target > top) target = top;
    native_scroll_target((u8 *)raw + 0xc, target);
    *(float *)((u8 *)raw + 8) = offset;
}
static int click(void *raw) {
    struct custom_state *s = state(raw);
    int selected = (int)WORD(common(raw), 0x7c);
    enum custom_page before = s->flow.page;
    int result;
    if (before == CUSTOM_HOME) {
        int action = custom_home_action(selected, s->features);
        result = action < 0 ? 1 : custom_choose(&s->flow, action, request_action);
        if (result != 2) s->flow.parent_selection = selected;
    } else if (before == CUSTOM_MATTER && starts_with(s->status, "Pairing open") && selected == 0) {
        custom_back(&s->flow); result = 1;
    } else result = custom_choose(&s->flow, selected, request_action);
    if (s->flow.page != before && custom_activity(s->flow.page)) {
        s->activity_ticks = 0; s->activity_started = 0;
        if (s->flow.page == CUSTOM_ZIGBEE_PAIRING) keep_awake(raw);
    }
    if (result != 2) show(raw, s->flow.page != before);
    return result;
}
static void refresh_status(void *raw) {
    struct custom_state *s = state(raw);
    if (s->flow.page == CUSTOM_HOME) {
        if (s->features != enabled_features()) show(raw, 0);
        return;
    }
    if (s->flow.page == CUSTOM_ZIGBEE_CONFIRM) return;
    enum custom_page before = s->flow.page;
    char status[96], code[32];
    read_text(custom_matter(s->flow.page) ? "/var/run/unfr0zen/matter.status" :
              "/var/run/unfr0zen/zigbee.status", status, sizeof(status), "Not enabled or service unavailable");
    read_text("/var/run/unfr0zen/matter.code", code, sizeof(code), "Code unavailable");
    int changed = !same_text(status, s->status) || !same_text(code, s->code);
    if (custom_activity(s->flow.page)) {
        s->activity_ticks++;
        if (s->flow.page == CUSTOM_ZIGBEE_PAIRING && s->activity_ticks < 120 && !(s->activity_ticks % 4))
            keep_awake(raw);
        if (!s->activity_started && (starts_with(status, "Pairing...") || starts_with(status, "Pairing open"))) {
            s->activity_started = 1; changed = 1;
        }
        int failed = starts_with(status, "Pairing timed out") || starts_with(status, "Radio unavailable") ||
                     starts_with(status, "Not enabled") || starts_with(status, "Zigbee not enabled");
        if (failed || (s->activity_started && (starts_with(status, "Paired") || starts_with(status, "Ready to pair")))) {
            custom_back(&s->flow); changed = 1;
        }
        if (s->flow.page == CUSTOM_ZIGBEE_PAIRING && s->activity_ticks >= 120) {
            custom_back(&s->flow); changed = 1;
        }
    }
    if (changed) {
        show(raw, s->flow.page != before);
    }
    if (custom_matter(s->flow.page) && !PTR(raw, 0xa0)) load_image(raw);
}
static void stop_refresh(void *raw) {
    struct custom_state *s = state(raw);
    if (s->refresh_request) {
        native_cancel_timeout(s->refresh_request);
        native_release(s->refresh_request);
        s->refresh_request = 0;
    }
}
static void schedule_refresh(void *raw) {
    struct custom_state *s = state(raw);
    void *globals = PTR(common(raw), 0x8c);
    native_send_timeout(&s->refresh_request, (u32)s, custom_activity(s->flow.page) ? 250 : 1000,
                        PTR(common(raw), 0x14), common(raw), PTR(globals, GLOBALS_TIMER_OFFSET));
}
static int layer_event(void *raw, void *event, void *target) {
    u32 type = native_event_type(event);
    struct custom_state *s = state(raw);
    if (type == 0x30000003 && native_timeout_id(event) == (u32)s) {
#ifdef CUSTOM_TRACE
        trace(raw, type, native_timeout_id(event));
#endif
        if (s->active) { refresh_status(raw); schedule_refresh(raw); }
        return 1;
    }
    int result = native_layer_event(raw, event, target);
    /* Old Legal Info consumes a click by stopping its timer and returning
     * exit (2), bypassing the click vtable. Keep that native cleanup, then
     * ask the custom flow whether this click actually exits the screen. */
    if (LEGACY_CLICK && type == 0x40000001 && result == 2) result = click(raw);
#ifdef CUSTOM_TRACE
    if (type == TIMER_EVENT_FIRST || type == TIMER_EVENT_SECOND) trace(raw, type, result);
#else
    (void)type;
#endif
    return result;
}
static int common_event(void *base, void *event, void *target) {
    return layer_event((u8 *)base - COMMON_OFFSET, event, target);
}
static void enter_layer(void *raw) { show(raw, 1); }
static void enter_common(void *base) { show((u8 *)base - COMMON_OFFSET, 1); }
static void no_reload(void *raw) { (void)raw; }
static int draw(void *raw, void *context) {
    struct custom_state *s = state(raw);
    /* Settings draws every screen while previewing its carousel tile.
     * The inner menu belongs only to the entered screen. */
    if (!s->active) return 0;
    void *base = common(raw);
    void *globals = PTR(base, 0x8c);
    int style = native_style(globals);
    /* The stock legal renderer initializes this same nlLayer text style. */
    void (*set_style)(void *, int, int) = PTR(PTR(base, 0), 0x5c);
    set_style(base, style, 255);
    int y = native_config_y(&s->config, context, WORD(base, 0xc), 0, 0x10000);
    /* Image documents start at the viewport edge. Short text-only pages
     * retain the circular display's safe inset, without a blank title row. */
    int inset = SCALED(s, s->flow.page == CUSTOM_HOME || custom_matter(s->flow.page) ? 240 : 120);
    y = (s->at_top ? 0 : (int)*(float *)((u8 *)raw + 8)) - y - inset;
    void *image = PTR(raw, 0xa0);
    if (image && (s->flow.page == CUSTOM_HOME || custom_matter(s->flow.page))) {
        void (*draw_image)(void *, int, int, void *, int) = PTR(PTR(context, 0), 0x50);
#ifdef CUSTOM_TRACE
        if (!s->image_traced) {
            trace(raw, 0x494d4704, (u32)draw_image);
            trace(raw, 0x494d4705, (u32)image);
            s->image_traced = 1;
        }
#endif
        /* Anchor the image directly above the following native text row.
         * This remains correct when returning with a different selection. */
        int text_y = native_config_y(&s->config, context, WORD(base, 0xc), 1, 0x10000);
        int width = native_image_width(image), height = native_image_height(image);
        /* The gen 2's text rows reach 12px further above their anchor. */
        int top = y + text_y - SCALED(s, 24 + (custom_matter(s->flow.page) ? 10 : 0)) - (s->display >= 480 ? 0 : 12);
        /* The home image is the gen 3's size, scaled down here; nestlocal
         * draws the Matter code at the display's own. */
        if (s->flow.page == CUSTOM_HOME && s->display != 480) {
            void (*draw_scaled)(void *, int, int, void *, int, int, void *) = PTR(PTR(context, 0), 0x60);
            draw_scaled(context, -SCALED(s, width) / 2, top - SCALED(s, height), image, style,
                        s->display * 0x10000 / 480, (void *)0);
        } else {
            draw_image(context, -width / 2, top - height, image, style);
        }
    }
    if (custom_activity(s->flow.page)) {
        void *spinner = s->activity_images[s->activity_ticks % 8];
        if (spinner) {
            void (*draw_image)(void *, int, int, void *, int) = PTR(PTR(context, 0), 0x50);
            int activity_y = native_config_y(&s->config, context, WORD(base, 0xc), custom_matter(s->flow.page) ? 2 : 1, 0x10000);
            draw_image(context, -native_image_width(spinner) / 2, y + activity_y, spinner, style);
        }
    }
    native_config_draw(base, context, &s->config, 0, y, 255,
                       WORD(base, 0x7c), style, 0x10000);
    return (native_scroll_draw(raw, context, 0x10000), 0);
}
static int draw_common(void *base, void *context) {
    return draw((u8 *)base - COMMON_OFFSET, context);
}
static void title(void *screen) {
    native_string_copy((nl_string *)((u8 *)screen + 0x54), "\nUNFR0ZEN");
    native_string_copy((nl_string *)((u8 *)screen + 0x70), "by @will-tm");
    native_rebuild_title(screen);
}
/* Preserve the native context and transform; offset only its title text calls. */
struct title_context {
    u8 vtable[0xc4];
    void *original;
    int y_offset;
};
static void subtitle_text(void *context, int x, int y, void *text,
                          int count, int scale, int a, int b, int c) {
    struct title_context *view = PTR(context, 0);
    void (*draw_text)(void *, int, int, void *, int, int, int, int, int) =
        PTR(view->original, 0x30);
    draw_text(context, x, y - view->y_offset, text, count, scale, a, b, c);
}
static int screen_draw(void *screen, void *context) {
    void *globals = PTR(screen, 0x3c);
    int menu_visible = !native_menu_hidden(globals);
    struct title_context view;
    struct custom_state *s = state(raw_layer(screen));
    if (menu_visible) {
        view.y_offset = (int)(SCALED(s, 40) * native_menu_scale(globals));
        view.original = PTR(context, 0);
        bytes_copy(view.vtable, view.original, sizeof(view.vtable));
        PTR(view.vtable, 0x30) = subtitle_text;
        PTR(context, 0) = view.vtable;
    }
    int result = native_screen_draw(screen, context);
    if (menu_visible) PTR(context, 0) = view.original;
    if (menu_visible) {
        if (!s->menu_glyph) {
            int error = 0;
            s->menu_glyph = native_image_load(MENU_GLYPH, &error);
        }
        if (s->menu_glyph) {
            /* Native menu titles settle at half scale; glyph coordinates
             * describe the full 80px image at that resting size, as the
             * gen 3's own glyphs are. The gen 2's are smaller and lower:
             * 30px showing, centred 59px above the tile's centre. */
            int gen3 = s->display >= 480;
            float scale = native_menu_scale(globals) * 2;
            float glyph = scale * (gen3 ? 1.0f : 44.0f / 80);
            void (*draw_image)(void *, int, int, void *, int, int, void *) =
                PTR(PTR(context, 0), 0x60);
            draw_image(context, (int)(-native_image_width(s->menu_glyph) * glyph / 2),
                       (int)((gen3 ? -120 : -71) * scale), s->menu_glyph, native_style(globals),
                       (int)(glyph * 0x10000), (void *)0);
        }
    }
    return result;
}
static void title_thunk(void *part) { title((u8 *)part - 8); }
static void enter_screen(void *screen) {
    struct custom_state *s = state(raw_layer(screen));
    s->active = 1;
    s->flow.page = CUSTOM_HOME;
    s->flow.parent_selection = 0;
    s->flow.action_failed = 0;
    load_image(raw_layer(screen));
    native_screen_enter(screen);
    show(raw_layer(screen), 1);
    schedule_refresh(raw_layer(screen));
}
static void exit_screen(void *screen) {
    state(raw_layer(screen))->active = 0;
    stop_refresh(raw_layer(screen));
    struct custom_state *s = state(raw_layer(screen));
    for (int i = 0; i < 8; i++) { release_image(s->activity_images[i]); s->activity_images[i] = 0; }
}
static int screen_event(void *screen, void *event, void *target) {
    void *raw = raw_layer(screen);
    u32 type = native_event_type(event);
    WORD(common(raw), 0x74) = WORD(common(raw), 0x7c);
#ifdef CUSTOM_TRACE
    if (type == 0x40000004) trace(raw, type, (u32)target);
#endif
    /* Settings' inherited dispatcher uses 0x40000006 for back navigation.
     * Consume it within a child page; the parent keeps the stock exit path. */
    if (type == 0x40000006 && custom_back(&state(raw)->flow)) {
        show(raw, 1);
        return 1;
    }
    /* The third argument is the event destination. GetType may clobber r2;
     * forward the saved destination, including NULL for user input. */
    return native_screen_event(screen, event, target);
}
/* Exported replacement for account's factory. */
void *custom_factory(void *globals, void *handler, const char *name) {
    void *screen = native_new(0xf0);
    if (!screen) return 0;
    native_screen_ctor(screen);
    /* Reproduce the derived factory's vptr installation after the base
     * constructor. All three pointers come from discovered ELF data. */
    PTR(screen, 0) = (void *)native_screen_vtable;
    PTR(screen, 4) = (void *)(native_screen_vtable + SCREEN_SECOND_VPTR - 8);
    PTR(screen, 8) = (void *)(native_screen_vtable + SCREEN_THIRD_VPTR - 8);
    PTR(screen, 0xec) = 0;
    void *background = native_layer_lookup(globals, "backgroundlayer");
    if (native_screen_init(screen, globals, handler, name, background)) return 0;
    native_attach(screen, background);
    /* Copy only dimensions and the event destination from an initialized
     * stock layer. Allocate a separate object; do not alter that singleton. */
    void *template = native_layer_lookup(globals, "legalinfolayer");
    if (!template) return 0;
    void *raw = native_new(PRIMARY_SIZE + sizeof(struct custom_state));
    if (!raw) return 0;
    bytes_zero(raw, PRIMARY_SIZE + sizeof(struct custom_state));
    native_layer_ctor(raw);
    struct custom_state *s = state(raw);
    s->screen = screen;
    s->display = display_width();
    s->config.text_count = 2;
    s->config.text = s->text;
    s->config.flags = s->flags;
    s->flags[0] = 0;
    s->config.choices = s->choices;
    s->config.cache_id = -1;
    s->choice_ids[0] = "first";
    s->choice_ids[1] = "second";
    s->choice_ids[2] = "third";
    s->choice_ids[3] = "fourth";
    for (int i = 0; i < 3; i++) native_string_ctor(&s->text[i]);
    for (int i = 0; i < 4; i++) native_string_ctor(&s->choices[i]);
    /* Preserve negative-offset metadata and all adjustment thunks. */
    bytes_copy(s->layer_vtables, (u8 *)PTR(raw, 0) - 12, LAYER_VTABLE_SIZE);
    PTR(raw, 0) = s->layer_vtables + 12;
    PTR(raw, COMMON_OFFSET) = s->layer_vtables + LAYER_COMMON_VPTR;
    PTR(raw, COMMON_OFFSET + 4) = s->layer_vtables + LAYER_SECOND_VPTR;
    PTR(raw, COMMON_OFFSET + 8) = s->layer_vtables + LAYER_THIRD_VPTR;
    PTR(PTR(raw, 0), 0x14) = layer_event;
    PTR(PTR(raw, COMMON_OFFSET), 0) = common_event;
    PTR(PTR(raw, 0), 0x1c) = click;
    PTR(PTR(raw, 0), 0x28) = draw;
    PTR(PTR(raw, 0), 0x2c) = enter_layer;
    PTR(PTR(raw, 0), 0x30) = no_reload;
    PTR(PTR(raw, COMMON_OFFSET), 0x0c) = enter_common;
    PTR(PTR(raw, COMMON_OFFSET), 0x10) = draw_common;
    PTR(PTR(raw, 0), 0x18) = custom_wheel;
    void *base = common(raw);
    if (native_layer_init(base, globals, PTR(template, 0x14), "nlcustomlayer",
                          WORD(template, 0x64), WORD(template, 0x68))) return 0;
    PTR(screen, 0xec) = raw;
    PTR(screen, 0x20) = base;
    native_attach(screen, base);
    bytes_copy(s->screen_vtables, (u8 *)PTR(screen, 0) - 8, SCREEN_VTABLE_SIZE);
    PTR(screen, 0) = s->screen_vtables + 8;
    PTR(screen, 4) = s->screen_vtables + SCREEN_SECOND_VPTR;
    PTR(screen, 8) = s->screen_vtables + SCREEN_THIRD_VPTR;
    PTR(PTR(screen, 0), 0) = screen_event;
    PTR(PTR(screen, 0), 0x1c) = enter_screen;
    PTR(PTR(screen, 0), 0x20) = exit_screen; /* retain image, hide inner menu */
    PTR(PTR(screen, 0), SCREEN_TITLE_SLOT) = title;
    PTR(PTR(screen, 0), 0x0c) = screen_draw;
    PTR(PTR(screen, 8), 0x10) = title_thunk;
    title(screen);
    return screen;
}
