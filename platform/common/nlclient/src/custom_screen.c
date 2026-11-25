/* unfr0zen's settings screens, linked into the unit's stock nlclient: the
 * Zigbee, Matter, Themes, Doom and UNFR0ZEN entries of the settings menu,
 * one screen each (custom_root) built by one factory. It draws through stock text layout, selection and image APIs, no
 * framebuffer drawing of its own. Native API addresses and object layouts
 * are discovered from the input ELF; they are build-specific and still need
 * validating on a unit.
 *
 * The payload has no writable data: all state lives in the layer object
 * (struct custom_state, after the stock layer's PRIMARY_SIZE bytes), and
 * what must outlive it in files under /var/run. */
#include "custom_flow.h"
typedef unsigned int u32;
typedef unsigned char u8;

/* --- Discovered layouts ---------------------------------------------------
 * The linker defines each as an absolute symbol (layout_NAME = value), so
 * one object serves every build. Numeric layouts must not acquire the PIE
 * load bias: an ABS32 literal holds the discovered number, including
 * offsets within native vtables. */
#define LAYOUT(name)                                                                           \
    __extension__({                                                                            \
        u32 value;                                                                             \
        __asm__("ldr %0, 1f\n b 2f\n .balign 4\n1: .word layout_" #name "\n2:" : "=r"(value)); \
        value;                                                                                 \
    })
#define DECLARE_LAYOUT(name) extern const u8 layout_##name __attribute__((visibility("hidden")))
DECLARE_LAYOUT(ACTIVITY_QUEUE_OFFSET);
#define ACTIVITY_QUEUE_OFFSET LAYOUT(ACTIVITY_QUEUE_OFFSET)
DECLARE_LAYOUT(BUCKET_GLOBALS_OFFSET);
#define BUCKET_GLOBALS_OFFSET LAYOUT(BUCKET_GLOBALS_OFFSET)
DECLARE_LAYOUT(COMMON_OFFSET);
#define COMMON_OFFSET LAYOUT(COMMON_OFFSET)
DECLARE_LAYOUT(GLOBALS_TIMER_OFFSET);
#define GLOBALS_TIMER_OFFSET LAYOUT(GLOBALS_TIMER_OFFSET)
DECLARE_LAYOUT(LAYER_COMMON_VPTR);
#define LAYER_COMMON_VPTR LAYOUT(LAYER_COMMON_VPTR)
DECLARE_LAYOUT(LAYER_SECOND_VPTR);
#define LAYER_SECOND_VPTR LAYOUT(LAYER_SECOND_VPTR)
DECLARE_LAYOUT(LAYER_THIRD_VPTR);
#define LAYER_THIRD_VPTR LAYOUT(LAYER_THIRD_VPTR)
DECLARE_LAYOUT(LAYER_VTABLE_SIZE);
#define LAYER_VTABLE_SIZE LAYOUT(LAYER_VTABLE_SIZE)
DECLARE_LAYOUT(LEGACY_CLICK);
#define LEGACY_CLICK LAYOUT(LEGACY_CLICK)
DECLARE_LAYOUT(PRIMARY_SIZE);
#define PRIMARY_SIZE LAYOUT(PRIMARY_SIZE)
DECLARE_LAYOUT(RENAME_SYSCALL);
#define RENAME_SYSCALL LAYOUT(RENAME_SYSCALL)
/* The client's screen timeouts, in ms: its default (client.config's
 * sleeptimeout) and the one in force; 0 where discovery found none. */
DECLARE_LAYOUT(SLEEP_DEFAULT_OFFSET);
#define SLEEP_DEFAULT_OFFSET LAYOUT(SLEEP_DEFAULT_OFFSET)
DECLARE_LAYOUT(SLEEP_ACTIVE_OFFSET);
#define SLEEP_ACTIVE_OFFSET LAYOUT(SLEEP_ACTIVE_OFFSET)
/* The display's brightness manager: its field in the globals, its setting's
 * (0 low, 1 medium, 2 high, 3 auto) and the high level's (a float, in %,
 * after medium's and low's); the setting 0 where discovery found none. */
DECLARE_LAYOUT(BRIGHTNESS_GLOBALS_OFFSET);
#define BRIGHTNESS_GLOBALS_OFFSET LAYOUT(BRIGHTNESS_GLOBALS_OFFSET)
DECLARE_LAYOUT(BRIGHTNESS_MODE_OFFSET);
#define BRIGHTNESS_MODE_OFFSET LAYOUT(BRIGHTNESS_MODE_OFFSET)
DECLARE_LAYOUT(BRIGHTNESS_HIGH_OFFSET);
#define BRIGHTNESS_HIGH_OFFSET LAYOUT(BRIGHTNESS_HIGH_OFFSET)
/* The screen hand-over: 1 where nlclient presents through eglSwapBuffers
 * (custom_swap, the keeper thread), 2 where it pans its own fb0 mapping
 * (custom_flip, with the FB_ fields of its framebuffer object), 0 neither. */
DECLARE_LAYOUT(HANDOVER);
#define HANDOVER      LAYOUT(HANDOVER)
#define HANDOVER_SWAP 1
#define HANDOVER_FLIP 2
DECLARE_LAYOUT(FB_FD_OFFSET);
#define FB_FD_OFFSET LAYOUT(FB_FD_OFFSET)
DECLARE_LAYOUT(FB_MAP_OFFSET);
#define FB_MAP_OFFSET LAYOUT(FB_MAP_OFFSET)
DECLARE_LAYOUT(FB_SIZE_OFFSET);
#define FB_SIZE_OFFSET LAYOUT(FB_SIZE_OFFSET)
DECLARE_LAYOUT(SCREEN_SECOND_VPTR);
#define SCREEN_SECOND_VPTR LAYOUT(SCREEN_SECOND_VPTR)
DECLARE_LAYOUT(SCREEN_THIRD_VPTR);
#define SCREEN_THIRD_VPTR LAYOUT(SCREEN_THIRD_VPTR)
DECLARE_LAYOUT(SCREEN_TITLE_SLOT);
#define SCREEN_TITLE_SLOT LAYOUT(SCREEN_TITLE_SLOT)
DECLARE_LAYOUT(SCREEN_VTABLE_SIZE);
#define SCREEN_VTABLE_SIZE LAYOUT(SCREEN_VTABLE_SIZE)
DECLARE_LAYOUT(TIMER_EVENT_FIRST);
#define TIMER_EVENT_FIRST LAYOUT(TIMER_EVENT_FIRST)
DECLARE_LAYOUT(TIMER_EVENT_SECOND);
#define TIMER_EVENT_SECOND LAYOUT(TIMER_EVENT_SECOND)
DECLARE_LAYOUT(MENU_OBJECT_REGISTER);
#define MENU_OBJECT_REGISTER LAYOUT(MENU_OBJECT_REGISTER)
DECLARE_LAYOUT(MENU_NAMES_REGISTER);
#define MENU_NAMES_REGISTER LAYOUT(MENU_NAMES_REGISTER)
DECLARE_LAYOUT(MENU_SCREENS_REGISTER);
#define MENU_SCREENS_REGISTER LAYOUT(MENU_SCREENS_REGISTER)
DECLARE_LAYOUT(MENU_NEXT_REGISTER);
#define MENU_NEXT_REGISTER LAYOUT(MENU_NEXT_REGISTER)
/* Room for the copied vtables; the build refuses larger discovered ones. */
#define VTABLE_MAX         0x400

/* --- Fixed layouts ----------------------------------------------------------
 * The same in every audited build. */
#define WORD(p, offset)  (*(u32 *)((u8 *)(p) + (offset)))
#define PTR(p, offset)   (*(void **)((u8 *)(p) + (offset)))
#define FLOAT(p, offset) (*(float *)((u8 *)(p) + (offset)))

/* The layer (`raw`, the derived object), from its start. */
#define LAYER_SCROLL_Y       0x08 /* float: the viewport's offset */
#define LAYER_SCROLLER       0x0c /* the stock scroller */
/* floats: the furthest the scroller goes is their difference */
#define LAYER_CONTENT_HEIGHT 0x18
#define LAYER_VIEW_HEIGHT    0x30
#define LAYER_SCROLL_FACTOR  0x84 /* float */
#define LAYER_IMAGE          0xa0 /* the page's image, or 0 */
/* Its vtable's slots. */
#define LAYER_EVENT_SLOT     0x14
#define LAYER_WHEEL_SLOT     0x18
#define LAYER_CLICK_SLOT     0x1c
#define LAYER_DRAW_SLOT      0x28
#define LAYER_ENTER_SLOT     0x2c
#define LAYER_RELOAD_SLOT    0x30
/* Bytes before the vptr in its vtable: offset-to-top, RTTI and the
 * adjustment metadata, all copied. */
#define LAYER_VTABLE_HEAD    12

/* The layer's nlLayer part (`base`, at COMMON_OFFSET), and any stock
 * nlLayer's (the template). */
#define COMMON_TEXT_BOX        0x0c /* passed to the text layout as stock does */
#define COMMON_EVENT_TARGET    0x14
#define COMMON_WIDTH           0x64
#define COMMON_HEIGHT          0x68
#define COMMON_SELECTION_SHOWN 0x74
#define COMMON_SELECTION       0x7c
#define COMMON_GLOBALS         0x8c
#define COMMON_CHOICE_IDS      0x90 /* null-terminated */
#define COMMON_CHOICE_FIRST    0x94
#define COMMON_CHOICE_LAST     0x98
/* Its vtable's slots. */
#define COMMON_EVENT_SLOT      0x00
#define COMMON_ENTER_SLOT      0x0c
#define COMMON_DRAW_SLOT       0x10
#define COMMON_SET_STYLE_SLOT  0x5c

/* The Settings screen object. */
#define SCREEN_SIZE             0xf0
#define SCREEN_ACTIVE_LAYER     0x20
#define SCREEN_GLOBALS          0x3c
#define SCREEN_TITLE            0x54 /* nl_string */
#define SCREEN_SUBTITLE         0x70 /* nl_string */
#define SCREEN_LAYER            0xec /* our layer (raw) */
/* Its vtable's slots, and the third part's title slot (its this is 8
 * bytes into the screen). */
#define SCREEN_EVENT_SLOT       0x00
#define SCREEN_DRAW_SLOT        0x0c
#define SCREEN_ENTER_SLOT       0x1c
#define SCREEN_EXIT_SLOT        0x20
#define SCREEN_THIRD_TITLE_SLOT 0x10
#define SCREEN_THIRD_THIS       8
/* Bytes before the vptr in its vtable. */
#define SCREEN_VTABLE_HEAD      8

/* The drawing context's vtable: its size and slots. */
#define CONTEXT_VTABLE_SIZE      0xc4
#define CONTEXT_DRAW_TEXT_SLOT   0x30
#define CONTEXT_DRAW_IMAGE_SLOT  0x50
#define CONTEXT_DRAW_SCALED_SLOT 0x60
/* An image's vtable slot that destroys it. */
#define IMAGE_DESTROY_SLOT       4
/* The client's globals, which tells the stock client from another queue. */
#define CLIENT_GLOBALS           0x70

/* nlclient's events and their size. */
#define EVENT_SIZE           0x10
#define EVENT_TIMEOUT        0x30000003
#define EVENT_CLICK          0x40000001
#define EVENT_WHEEL          0x40000004
#define EVENT_ACTIVITY       0x40000005
#define EVENT_BACK           0x40000006
/* A handler's UI results: handled, leave the screen, and light the display
 * on the client's normal wake timer. */
#define RESULT_HANDLED       1
#define RESULT_EXIT          2
#define RESULT_LIGHT_DISPLAY 5

/* 16.16 fixed point's 1, and full opacity. */
#define FIXED_ONE 0x10000
#define OPAQUE    255

/* --- Files --------------------------------------------------------------- */
#define IMAGES           "/nestlabs/share/images/"
#define CUSTOM_IMAGE     IMAGES "G_unfr0zen_192px.png"
/* The menu glyphs: each entry's, 80 px. */
#define MENU_GLYPH       IMAGES "G_unfr0zen_glyph_80px.png"
#define SCHEDULE_GLYPH   IMAGES "G_unfr0zen_schedule_80px.png"
#define ZIGBEE_GLYPH     IMAGES "G_unfr0zen_zigbee_80px.png"
#define MATTER_GLYPH     IMAGES "G_unfr0zen_matter_80px.png"
#define THEMES_GLYPH     IMAGES "G_unfr0zen_themes_80px.png"
#define DOOM_GLYPH       IMAGES "G_unfr0zen_doom_80px.png"
/* Doom's title, made from the WAD when the app installs Doom (246x120). */
#define DOOM_IMAGE       IMAGES "G_unfr0zen_doom.png"
/* nestlocal's files. */
#define RUN              "/var/run/unfr0zen/"
#define MATTER_STATUS    RUN "matter.status"
#define ZIGBEE_STATUS    RUN "zigbee.status"
#define MATTER_CODE      RUN "matter.code"
#define MATTER_REQUEST   RUN "matter-reset.request"
#define THEME_REQUEST    RUN "theme.request"
#define SCHEDULE_STATUS  RUN "schedule.status"
#define SCHEDULE_REQUEST RUN "schedule.request"
#define NOT_ENABLED      "Not enabled or service unavailable"

/* The largest images shown: Doom's title is wider than the others. */
#define IMAGE_MAX            192
#define DOOM_IMAGE_MAX_WIDTH 256
/* The spinner's frames, activity-0.png to activity-7.png. */
#define ACTIVITY_FRAMES      8
/* Status polls a second apart, four a second during a pairing. */
#define REFRESH_IDLE_MS      1000
#define REFRESH_ACTIVE_MS    250
/* A pairing keeps the display lit every this many polls. */
#define KEEP_AWAKE_TICKS     4
/* The gen 3's display width, which every layout length is given for. */
#define GEN3_WIDTH           480

/* Page texts shown both as the heading and as the body. */
#define SERVICE_FAILED      "Service unavailable. Try again."
#define MATTER_CONFIRM_TEXT "Forget all Matter controllers and pair again? This cannot be undone."
#define ZIGBEE_CONFIRM_TEXT "Leave current network and pair again? Permit joining on your coordinator first."

/* --- Native types and calls ------------------------------------------------ */
typedef struct {
    u32 words[7];
} nl_string;
/* The stock text layout's configuration: text rows then choice rows. */
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
extern void *native_menu_lookup(void *, const char *);
extern void native_menu_grow(void *, void *, void *);
extern int native_attach(void *, void *);
extern void native_string_ctor(nl_string *);
extern void native_string_copy(nl_string *, const char *);
extern void native_string_destroy(nl_string *);
extern void native_config_invalidate(text_config *);
extern void native_config_layout(void *, text_config *, int, int);
extern int native_config_y(text_config *, void *, int, int, int);
extern void native_config_draw(void *, void *, text_config *, int, int, int, int, int, int);
extern void native_select(void *, int);
extern void native_set_brightness(void *, int);
extern void native_apply_brightness(void *);
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
extern u32 native_time(void *);
extern int native_device_commit(void *);
extern int native_shared_commit(void *);
extern int native_schedule_commit(void *);
extern void native_delete(void *);
extern u32 native_egl_swap(void *, void *);
extern int native_open(const char *, int);
extern int native_read(int, void *, unsigned int);
extern int native_close(int);
extern int native_kill(int, int);
extern int native_pthread_create(void *, void *, void *(*)(void *), void *);
extern int native_usleep(unsigned int);
extern int native_getpid(void);
extern int native_fb_flip(void *, void *);
extern void *native_mmap(void *, unsigned int, int, int, int, int);

typedef void draw_image_fn(void *context, int x, int y, void *image, int style);
typedef void draw_scaled_fn(void *context, int x, int y, void *image, int style, int scale, void *unused);

/* --- State ------------------------------------------------------------------ */
struct theme_catalog {
    char *text;
    char **ids, **labels;
    int count;
};
struct custom_state {
    struct custom_flow flow;
    int active;
    int at_top;
    void *refresh_request;
    char status[96];
    char code[32];
    char detail[192];
    enum custom_page image_page;
    void *activity_images[ACTIVITY_FRAMES];
    unsigned int activity_ticks;
    int activity_started;
#ifdef CUSTOM_TRACE
    int image_traced;
#endif
    void *screen;
    /* The display's width, which the gen 3's layouts below scale to. */
    int display;
    int drawn;
    void *menu_glyph;
    text_config config;
    nl_string text[3];
    nl_string *choices;
    int choice_capacity;
    struct theme_catalog themes;
    u32 flags[3];
    const char **choice_ids;
    u8 layer_vtables[VTABLE_MAX];
    u8 screen_vtables[VTABLE_MAX];
};

/* --- Helpers ----------------------------------------------------------------- */
static void bytes_copy(void *to, const void *from, unsigned int count) {
    u8 *dest = to;
    const u8 *source = from;
    while (count--)
        *dest++ = *source++;
}
static void bytes_zero(void *to, unsigned int count) {
    u8 *dest = to;
    while (count--)
        *dest++ = 0;
}
static struct custom_state *state(void *raw) { return (struct custom_state *)((u8 *)raw + PRIMARY_SIZE); }
static void *raw_layer(void *screen) { return PTR(screen, SCREEN_LAYER); }
static void *common(void *raw) { return (u8 *)raw + COMMON_OFFSET; }

/* The stock client, whose event queue this accessor gives, or 0. Its
 * ownership is checked before any discovered field of it is used,
 * including on older firmware. */
static void *stock_client(void *globals) {
    void *client = native_activity_client(globals);
    return client && PTR(client, CLIENT_GLOBALS) == globals ? client : 0;
}
/* Tells the stock client of user activity, which keeps the display lit. */
static void notify_activity(void *globals) {
    void *client = stock_client(globals);
    if (!client)
        return;
    void *queue = PTR(client, ACTIVITY_QUEUE_OFFSET);
    if (!queue)
        return;
    void *event = native_new(EVENT_SIZE);
    if (!event)
        return;
    native_activity_ctor(event);
    native_mark_activity(globals);
    native_activity_init(event, EVENT_ACTIVITY, 0);
    native_enqueue(queue, event, 0);
}
static void keep_awake(void *raw) { notify_activity(PTR(common(raw), COMMON_GLOBALS)); }
#ifdef CUSTOM_TRACE
static void write_char(int value, void *file) {
    u8 byte = (u8)value;
    native_fwrite(&byte, 1, 1, file);
}
/* Appends the event and the layer's selection and scroll fields, in hex. */
static void trace(void *raw, u32 type, u32 value) {
    void *file = native_fopen("/media/user-config/nlcustom-wheel.log", "a");
    if (!file)
        return;
    u32 fields[] = {type,
                    value,
                    WORD(raw, 0x7c),
                    WORD(common(raw), 0x74),
                    WORD(common(raw), 0x7c),
                    WORD(common(raw), 0x18),
                    WORD(common(raw), 0x14),
                    WORD(raw, 0x64),
                    WORD(raw, 8),
                    WORD(raw, 0x14),
                    WORD(raw, 0x18)};
    for (unsigned int i = 0; i < sizeof(fields) / sizeof(fields[0]); i++) {
        for (int shift = 28; shift >= 0; shift -= 4)
            write_char("0123456789abcdef"[(fields[i] >> shift) & 15], file);
        write_char(i + 1 == sizeof(fields) / sizeof(fields[0]) ? '\n' : ' ', file);
    }
    native_fclose(file);
}
#endif
static void show(void *raw, int opening);
static int custom_wheel(void *raw, int value) {
    if (state(raw)->flow.page == CUSTOM_HOLD_DURATION) {
        if (custom_duration_wheel(&state(raw)->flow, value))
            show(raw, 0);
        return RESULT_HANDLED;
    }
    if (value)
        state(raw)->at_top = 0;
    int result = native_layer_wheel(raw, value);
#ifdef CUSTOM_TRACE
    trace(raw, EVENT_WHEEL, value);
#endif
    return result;
}
/* The file's text without trailing line ends, or `fallback`. */
static void read_text(const char *path, char *out, unsigned int size, const char *fallback) {
    void *file = native_fopen(path, "r");
    unsigned int n = 0;
    if (file) {
        n = native_fread(out, 1, size - 1, file);
        native_fclose(file);
    }
    if (!n) {
        while (*fallback && n < size - 1)
            out[n++] = *fallback++;
    }
    while (n && (out[n - 1] == '\n' || out[n - 1] == '\r'))
        n--;
    out[n] = 0;
}
/* The decimal number at text[*at], up to `count`, moving *at past it. */
static u32 parse_number(const char *text, int *at, int count) {
    u32 value = 0;
    for (; *at < count && text[*at] >= '0' && text[*at] <= '9'; ++*at)
        value = value * 10 + (text[*at] - '0');
    return value;
}
/* The framebuffer's width: 480 on a gen 3, 320 on a gen 1 or gen 2. */
static int display_width(void) {
    char size[24];
    int at = 0;
    read_text("/sys/class/graphics/fb0/virtual_size", size, sizeof(size), "480");
    int width = (int)parse_number(size, &at, sizeof(size));
    return width ? width : GEN3_WIDTH;
}
/* A length of the gen 3's 480-pixel layout, on this display. */
#define SCALED(s, n) ((n) * (s)->display / GEN3_WIDTH)
static int same_text(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}
/* ARM Linux EABI's __NR_rename. */
#define NR_RENAME 38
/* rename(2): 0 on success. */
static int rename_setting(const char *from, const char *to) {
    if (RENAME_SYSCALL) {
        /* Older ELFs do not import rename; make the same atomic filesystem
         * call without libc internals. */
        register const char *arg0 __asm__("r0") = from;
        register const char *arg1 __asm__("r1") = to;
        register unsigned int number __asm__("r7") = NR_RENAME;
        __asm__ volatile("svc 0" : "+r"(arg0) : "r"(arg1), "r"(number) : "memory", "cc");
        return (int)arg0;
    }
    return native_rename(from, to);
}

/* --- Fault guard -------------------------------------------------------------
 * A build discovery matched on its structure alone can fault in the factory,
 * on entry, or on the first draw. A marker written before each and renamed
 * away after it outlives the restart a fault causes: finding it, the factory
 * declines, which the stock menu takes as the screen's absence, instead of
 * faulting at every restart. A reboot clears /var/run and tries again. */
#define ATTEMPT "/var/run/unfr0zen-screen.attempt"
static int attempt_pending(void) {
    void *file = native_fopen(ATTEMPT, "r");
    if (!file)
        return 0;
    native_fclose(file);
    return 1;
}
static void attempt_begin(void) {
    void *file = native_fopen(ATTEMPT, "w");
    if (file)
        native_fclose(file);
}
static void attempt_end(void) { rename_setting(ATTEMPT, "/var/run/unfr0zen-screen.done"); }

/* --- Lighting the display ------------------------------------------------- */
/* A handler queued on nlclient's event queue, run on its UI thread. */
struct wake_handler {
    void *vtable;
    int (*handle)(struct wake_handler *, void *);
    void *globals;
};
static int light_display(struct wake_handler *handler, void *event) {
    (void)event;
    native_mark_activity(handler->globals);
    native_delete(handler);
    return RESULT_LIGHT_DISPLAY;
}
static void queue_handler(void *globals, int (*handle)(struct wake_handler *, void *)) {
    void *client = stock_client(globals);
    if (!client)
        return;
    struct wake_handler *handler = native_new(sizeof(*handler));
    if (!handler)
        return;
    void *event = native_new(EVENT_SIZE);
    if (!event) {
        native_delete(handler);
        return;
    }
    /* Its vtable is its own handle field: slot 0 is the handler. */
    handler->vtable = &handler->handle;
    handler->handle = handle;
    handler->globals = globals;
    native_activity_ctor(event);
    native_activity_init(event, EVENT_ACTIVITY, 0);
    /* nlClient inherits nlEventQueue. Its main thread dispatches the handler
     * and applies the returned UI result before releasing the event. */
    native_enqueue(client, event, handler);
}
static void wake_display(void *globals) { queue_handler(globals, light_display); }
/* Seconds a wake request (display.request, its expiry as a u32) stays
 * current. */
#define WAKE_EXPIRY 10
/* Lights the display for a current wake request from nestlocal (a command
 * from Home Assistant). */
static void downlink_activity(void *bucket) {
    const char *taken = RUN "display.taken";
    if (rename_setting(RUN "display.request", taken))
        return;
    void *file = native_fopen(taken, "rb");
    if (!file)
        return;
    u32 expires = 0;
    unsigned int count = native_fread(&expires, 1, sizeof(expires), file);
    native_fclose(file);
    u32 now = native_time(0);
    /* Expiry prevents delayed subscribes or a client restart from waking
     * the panel for a command which is no longer current. */
    if (count == sizeof(expires) && expires >= now && expires - now <= WAKE_EXPIRY)
        wake_display(PTR(bucket, BUCKET_GLOBALS_OFFSET));
}

/* --- Screen hand-over ------------------------------------------------------
 * While the process named in SCREEN_OWNER is alive, nlclient keeps running,
 * drawing and controlling the heating, but presents nothing: the display is
 * the owner's (Doom). The check is live, so the owner's exit or death hands
 * it straight back; a stale file does nothing. KEEPER holds the pid of the
 * nlclient that hands over (and, for HANDOVER_FLIP, its globals after a
 * space): the owner's launcher checks it. */
#define SCREEN_OWNER  RUN "screen.owner"
#define KEEPER        RUN "keeper.pid"
/* How often the display is lit while owned, in seconds. */
#define KEEPER_PERIOD 2
/* The file's leading number, and the second, after a space, in *second. */
static u32 read_numbers(const char *path, u32 *second) {
    char text[32];
    *second = 0;
    int fd = native_open(path, 0);
    if (fd < 0)
        return 0;
    int count = native_read(fd, text, sizeof(text) - 1);
    native_close(fd);
    int at = 0;
    u32 first = parse_number(text, &at, count);
    if (at < count && text[at] == ' ') {
        at++;
        *second = parse_number(text, &at, count);
    }
    return first;
}
static int read_pid(const char *path) {
    u32 unused;
    return (int)read_numbers(path, &unused);
}
/* Writes `value` in decimal at `text`: its length. */
static unsigned int put_number(char *text, u32 value) {
    unsigned int length = 0;
    for (u32 rest = value; rest; rest /= 10)
        length++;
    if (!length)
        length = 1;
    for (unsigned int i = length; i > 0; value /= 10)
        text[--i] = '0' + value % 10;
    return length;
}
/* Writes `first`, and with `both` a space and `second`, as the file. */
static void write_numbers(const char *path, u32 first, u32 second, int both) {
    char text[24];
    unsigned int length = put_number(text, first);
    if (both) {
        text[length++] = ' ';
        length += put_number(text + length, second);
    }
    void *file = native_fopen(path, "w");
    if (!file)
        return;
    native_fwrite(text, 1, length, file);
    native_fclose(file);
}
static int screen_owned(void) {
    int pid = read_pid(SCREEN_OWNER);
    return pid > 1 && native_kill(pid, 0) == 0;
}
/* Replaces nlclient's eglSwapBuffers (HANDOVER_SWAP). */
u32 custom_swap(void *display, void *surface) {
    if (screen_owned())
        return 1; /* EGL_TRUE: drawn, not shown */
    return native_egl_swap(display, surface);
}
/* While the owner has the display, nlclient sees none of the ring and button
 * (the owner grabs them), so it would let the display go dark. A thread of
 * nlclient's own lights it every KEEPER_PERIOD as a command from Home
 * Assistant does (wake_display), and never sends nlclient input. */
static void *keeper(void *globals) {
    for (;;) {
        native_usleep(KEEPER_PERIOD * 1000000);
        if (screen_owned())
            wake_display(globals);
    }
    return 0;
}
/* Once per nlclient, from its first bucket commit or click on this screen
 * (Doom is launched from it). The payload has no writable data, so KEEPER
 * is the mark that the thread runs (HANDOVER_SWAP), or where the flip finds
 * the globals (HANDOVER_FLIP). */
static void keeper_start(void *globals) {
    if (!HANDOVER)
        return;
    int pid = native_getpid();
    if (read_pid(KEEPER) == pid)
        return;
    write_numbers(KEEPER, pid, (u32)globals, HANDOVER == HANDOVER_FLIP);
    /* Until nestlocal makes its directory the mark cannot be written: the
     * threads wait for a commit after that, so they start only once. */
    if (HANDOVER != HANDOVER_SWAP || read_pid(KEEPER) != pid)
        return;
    unsigned int thread;
    native_pthread_create(&thread, 0, keeper, globals);
}
/* The gen 1's and gen 2's flip (HANDOVER_FLIP). nlclient draws straight into its mapping
 * of fb0's two pages, so while the display is owned that mapping is
 * replaced, in place, by private memory: nlclient draws there unseen and
 * nothing is panned. Each owned frame waits FLIP_WAIT, as the vsync it
 * skips would have, so nlclient doesn't draw flat out. FB_PRIVATE marks the
 * swap and holds when the display was last lit (every KEEPER_PERIOD, as the
 * keeper thread does); once the owner is gone, fb0 is mapped back. */
#define FB_PRIVATE                  RUN "fb.private"
#define FLIP_WAIT                   100000 /* us */
#define PROT_READ_WRITE             3
#define MAP_SHARED_FIXED            0x11
#define MAP_PRIVATE_FIXED_ANONYMOUS 0x32
int custom_flip(void *fb, void *surface) {
    void *map = PTR(fb, FB_MAP_OFFSET);
    unsigned int size = WORD(fb, FB_SIZE_OFFSET);
    if (!screen_owned()) {
        if (!rename_setting(FB_PRIVATE, RUN "fb.shared"))
            native_mmap(map, size, PROT_READ_WRITE, MAP_SHARED_FIXED, WORD(fb, FB_FD_OFFSET), 0);
        return native_fb_flip(fb, surface);
    }
    u32 now = native_time(0), unused;
    u32 lit = read_numbers(FB_PRIVATE, &unused);
    if (!lit) {
        native_mmap(map, size, PROT_READ_WRITE, MAP_PRIVATE_FIXED_ANONYMOUS, -1, 0);
    }
    if (!lit || now - lit >= KEEPER_PERIOD) {
        u32 globals;
        if ((int)read_numbers(KEEPER, &globals) == native_getpid() && globals)
            wake_display((void *)globals);
        write_numbers(FB_PRIVATE, now, 0, 0);
    }
    native_usleep(FLIP_WAIT);
    return 0;
}
/* The screen timeout nestlocal asks for (screen.timeout, in ms, which its
 * setting's bucket change follows), as the client's default; the one in
 * force follows when it was the default, not a screen's own. */
#define SCREEN_TIMEOUT     RUN "screen.timeout"
#define SCREEN_TIMEOUT_MIN 1000
#define SCREEN_TIMEOUT_MAX 86400000
static void apply_screen_timeout(void *globals) {
    if (!SLEEP_DEFAULT_OFFSET || !SLEEP_ACTIVE_OFFSET)
        return;
    char text[16];
    read_text(SCREEN_TIMEOUT, text, sizeof(text), "");
    int at = 0;
    u32 timeout = parse_number(text, &at, sizeof(text));
    if (!at || text[at] || timeout < SCREEN_TIMEOUT_MIN || timeout > SCREEN_TIMEOUT_MAX)
        return;
    void *client = stock_client(globals);
    if (!client)
        return;
    u32 old = WORD(client, SLEEP_DEFAULT_OFFSET);
    if (old == timeout)
        return;
    if (WORD(client, SLEEP_ACTIVE_OFFSET) == old)
        WORD(client, SLEEP_ACTIVE_OFFSET) = timeout;
    WORD(client, SLEEP_DEFAULT_OFFSET) = timeout;
}
/* The brightness nestlocal asks for (screen.brightness: "auto", or 0 to 100
 * %), applied once per request and nlclient, so a choice on the unit's own
 * Brightness screen stays until the next request: a level is the high
 * setting's, in place of its own (kept in BRIGHTNESS_HIGH, which auto puts
 * back). What the unit shows goes to screen.brightness.state. */
#define SCREEN_BRIGHTNESS    RUN "screen.brightness"
#define BRIGHTNESS_APPLIED   RUN "screen.brightness.applied"
#define BRIGHTNESS_STATE     RUN "screen.brightness.state"
#define BRIGHTNESS_HIGH      RUN "brightness.high"
#define BRIGHTNESS_AUTO_CODE 101
enum { BRIGHTNESS_HIGH_MODE = 2, BRIGHTNESS_AUTO_MODE = 3 };
union level {
    float percent;
    u32 bits;
};
static void apply_brightness(void *globals) {
    if (!BRIGHTNESS_MODE_OFFSET)
        return;
    void *manager = PTR(globals, BRIGHTNESS_GLOBALS_OFFSET);
    if (!manager)
        return;
    char text[16];
    read_text(SCREEN_BRIGHTNESS, text, sizeof(text), "");
    int at = 0, automatic = same_text(text, "auto");
    u32 code = automatic ? BRIGHTNESS_AUTO_CODE : parse_number(text, &at, sizeof(text));
    u32 applied, unused, pid = (u32)native_getpid();
    if ((automatic || (at && !text[at] && code <= 100)) &&
        (read_numbers(BRIGHTNESS_APPLIED, &applied) != pid || applied != code)) {
        union level high = {.percent = FLOAT(manager, BRIGHTNESS_HIGH_OFFSET)};
        union level original = {.bits = read_numbers(BRIGHTNESS_HIGH, &unused)};
        if (!original.bits) {
            original = high;
            write_numbers(BRIGHTNESS_HIGH, original.bits, 0, 0);
        }
        FLOAT(manager, BRIGHTNESS_HIGH_OFFSET) = automatic ? original.percent : (float)code;
        native_set_brightness(manager, automatic ? BRIGHTNESS_AUTO_MODE : BRIGHTNESS_HIGH_MODE);
        native_apply_brightness(manager);
        write_numbers(BRIGHTNESS_APPLIED, pid, code, 1);
    }
    /* The state: auto, or the level of the setting in force. */
    u32 mode = WORD(manager, BRIGHTNESS_MODE_OFFSET);
    if (mode > BRIGHTNESS_AUTO_MODE)
        return;
    char state[8], shown[8];
    unsigned int length = 4;
    if (mode == BRIGHTNESS_AUTO_MODE) {
        bytes_copy(state, "auto", 4);
    } else {
        float level = FLOAT(manager, BRIGHTNESS_HIGH_OFFSET - (BRIGHTNESS_HIGH_MODE - mode) * 4);
        length = put_number(state, (u32)(level + 0.5f));
    }
    state[length] = 0;
    read_text(BRIGHTNESS_STATE, shown, sizeof(shown), "");
    if (same_text(state, shown))
        return;
    void *file = native_fopen(BRIGHTNESS_STATE, "w");
    if (!file)
        return;
    native_fwrite(state, 1, length, file);
    native_fclose(file);
}
/* Replace the buckets' commits: each starts the keeper, takes wake requests
 * and applies the screen timeout and brightness, as every bucket change
 * reaches one of them. */
static int after_commit(void *bucket, int result) {
    void *globals = PTR(bucket, BUCKET_GLOBALS_OFFSET);
    keeper_start(globals);
    apply_screen_timeout(globals);
    apply_brightness(globals);
    downlink_activity(bucket);
    return result;
}
int custom_device_commit(void *bucket) { return after_commit(bucket, native_device_commit(bucket)); }
int custom_shared_commit(void *bucket) { return after_commit(bucket, native_shared_commit(bucket)); }
int custom_schedule_commit(void *bucket) { return after_commit(bucket, native_schedule_commit(bucket)); }

/* --- Pages ---------------------------------------------------------------- */
/* The themes nestlocal offers (theme.catalog, "id\tlabel\n" lines), or
 * Stock alone. */
static void load_themes(struct theme_catalog *themes) {
    unsigned int capacity = 256, used = 0;
    char *text = native_new(capacity);
    void *file = native_fopen(RUN "theme.catalog", "r");
    if (file) {
        for (;;) {
            unsigned int n = native_fread(text + used, 1, capacity - used - 1, file);
            used += n;
            if (used < capacity - 1)
                break;
            char *larger = native_new(capacity * 2);
            bytes_copy(larger, text, used);
            native_delete(text);
            text = larger;
            capacity *= 2;
        }
        native_fclose(file);
    }
    if (!used) {
        bytes_copy(text, "stock\tStock\n", 12);
        used = 12;
    }
    text[used] = 0;
    int count = 0;
    for (unsigned int i = 0; i < used; i++)
        if (text[i] == '\n')
            count++;
    char **ids = native_new(2 * count * sizeof(char *));
    char **labels = ids + count;
    int index = 0;
    char *id = text, *label = 0;
    for (unsigned int i = 0; i < used; i++) {
        if (text[i] == '\t') {
            text[i] = 0;
            label = text + i + 1;
        } else if (text[i] == '\n') {
            text[i] = 0;
            if (label) {
                ids[index] = id;
                labels[index++] = label;
            }
            id = text + i + 1;
            label = 0;
        }
    }
    if (themes->text)
        native_delete(themes->text);
    if (themes->ids)
        native_delete(themes->ids);
    themes->text = text;
    themes->ids = ids;
    themes->labels = labels;
    themes->count = index;
}
/* Room for `count` choice rows. */
static void choices_capacity(struct custom_state *s, int count) {
    if (count <= s->choice_capacity)
        return;
    native_config_invalidate(&s->config);
    for (int i = 0; i < s->choice_capacity; i++)
        native_string_destroy(&s->choices[i]);
    if (s->choices)
        native_delete(s->choices);
    if (s->choice_ids)
        native_delete(s->choice_ids);
    s->choices = native_new(count * sizeof(nl_string));
    s->choice_ids = native_new((count + 1) * sizeof(char *));
    for (int i = 0; i < count; i++)
        native_string_ctor(&s->choices[i]);
    s->choice_capacity = count;
    s->config.choices = s->choices;
}
/* Writes `length` bytes of `data` to `tmp`, then renames it to `path`,
 * so nestlocal never reads a partial request: 0 once written. */
static int write_request(const char *tmp, const char *path, const char *data, unsigned int length) {
    void *file = native_fopen(tmp, "w");
    if (!file)
        return -1;
    unsigned int written = native_fwrite(data, 1, length, file);
    if (native_fclose(file) != 0 || written != length)
        return -1;
    return rename_setting(tmp, path);
}
/* Whether a service's status says it cannot take a request yet: disabled
 * or still starting. */
static int service_unready(const char *status) {
    return same_text(status, "Unavailable") || same_text(status, "Matter not enabled") ||
           same_text(status, "Zigbee not enabled") || same_text(status, "Starting Matter...") ||
           same_text(status, "Starting Zigbee...") || same_text(status, "Radio unavailable. Try again.");
}
/* custom_choose's requests (enum custom_request), as files nestlocal takes:
 * the theme's name in theme.request, else "1" in the action's request
 * file once its service is ready. 0 once written, else nonzero. */
static int request_action(int which, void *context) {
    struct custom_state *s = context;
    if (which < 0) {
        char status[192], command[32];
        unsigned int fields[4];
        const char *error;
        read_text(SCHEDULE_STATUS, status, sizeof(status), "");
        if (!custom_schedule_status(status, fields, &error))
            return -1;
        /* Do not overwrite a command the worker has not consumed. */
        void *pending = native_fopen(SCHEDULE_REQUEST, "r");
        if (pending) {
            native_fclose(pending);
            return -1;
        }
        const char *text = which == REQUEST_HOLD_DURATION                                  ? "hold_duration "
                           : which == REQUEST_SCHEDULE_ON || which == REQUEST_SCHEDULE_OFF ? "schedule_enabled "
                                                                                           : "hold ";
        unsigned int length = 0;
        while (*text)
            command[length++] = *text++;
        if (which == REQUEST_HOLD_DURATION) {
            length += put_number(command + length, s->flow.hold_minutes);
        } else {
            text = which == REQUEST_SCHEDULE_ON || which == REQUEST_HOLD_ON ? "ON" : "OFF";
            while (*text)
                command[length++] = *text++;
        }
        return write_request(SCHEDULE_REQUEST ".tmp", SCHEDULE_REQUEST, command, length);
    }
    if (which >= REQUEST_THEME) {
        if (which - REQUEST_THEME >= s->themes.count)
            return -1;
        const char *name = s->themes.ids[which - REQUEST_THEME];
        unsigned int length = 0;
        while (name[length])
            length++;
        return write_request(THEME_REQUEST ".tmp", THEME_REQUEST, name, length);
    }
    const char *status_path, *tmp, *path;
    switch (which) {
    case REQUEST_DOOM:
        status_path = RUN "doom.status", tmp = RUN "doom.request.tmp", path = RUN "doom.request";
        break;
    case REQUEST_ZIGBEE_PAIRING:
        status_path = ZIGBEE_STATUS, tmp = RUN "zigbee.request.tmp", path = RUN "zigbee.request";
        break;
    default:
        status_path = MATTER_STATUS, tmp = MATTER_REQUEST ".tmp", path = MATTER_REQUEST;
        break;
    }
    char status[96];
    read_text(status_path, status, sizeof(status), "Unavailable");
    if (service_unready(status))
        return -1;
    return write_request(tmp, path, "1", 1);
}
/* The Matter pairing page's body: the code, a blank line, the status. */
static void matter_detail(struct custom_state *s) {
    unsigned int n = 0;
    const char *code = s->activity_started ? s->code : "";
    const char *status =
        s->flow.page == CUSTOM_MATTER_PAIRING && !s->activity_started ? "Opening pairing..." : s->status;
    if (starts_with(status, "Pairing open. "))
        status += sizeof("Pairing open. ") - 1;
    while (*code && n < sizeof(s->detail) - 1)
        s->detail[n++] = *code++;
    if (n < sizeof(s->detail) - 1)
        s->detail[n++] = '\n';
    if (n < sizeof(s->detail) - 1)
        s->detail[n++] = '\n';
    while (*status && n < sizeof(s->detail) - 1)
        s->detail[n++] = *status++;
    s->detail[n] = 0;
}
static void release_image(void *image) {
    if (image) {
        void (*destroy)(void *) = PTR(PTR(image, 0), IMAGE_DESTROY_SLOT);
        destroy(image);
    }
}
/* The page's image: the logo at home, Doom's title, or nestlocal's Matter
 * code once pairing has opened. */
static void load_image(void *raw) {
    /* Reload on entry so a deployed PNG needs no executable rebuild. */
    release_image(PTR(raw, LAYER_IMAGE));
    PTR(raw, LAYER_IMAGE) = 0;
    int error = 0;
    void *image = 0;
    enum custom_page page = state(raw)->flow.page;
    const char *path = page == CUSTOM_MATTER_PAIRING ? RUN "matter.png"
                       : page == CUSTOM_DOOM         ? DOOM_IMAGE
                                                     : CUSTOM_IMAGE;
    if (page == CUSTOM_HOME || page == CUSTOM_DOOM || (page == CUSTOM_MATTER_PAIRING && state(raw)->activity_started)) {
        void *file = native_fopen(path, "rb");
        if (file) {
            native_fclose(file);
            image = native_image_load(path, &error);
        }
    }
    if (image && (native_image_width(image) > (page == CUSTOM_DOOM ? DOOM_IMAGE_MAX_WIDTH : IMAGE_MAX) ||
                  native_image_height(image) > IMAGE_MAX)) {
        release_image(image);
        image = 0;
    }
    state(raw)->image_page = page;
    PTR(raw, LAYER_IMAGE) = image; /* Missing images leave a usable text-only screen. */
#ifdef CUSTOM_TRACE
    state(raw)->image_traced = 0;
    trace(raw, 0x494d4701, (u32)image);
    trace(raw, 0x494d4702,
          image ? ((u32)native_image_width(image) << 16) | (u32)native_image_height(image) : (u32)error);
#endif
}
/* The spinner's frames nestlocal has made. */
static void load_activity(void *raw) {
    struct custom_state *s = state(raw);
    char path[] = RUN "activity-0.png";
    for (unsigned int i = 0; i < ACTIVITY_FRAMES; i++) {
        if (s->activity_images[i])
            continue;
        /* The digit, before ".png" and the terminator. */
        path[sizeof(path) - 6] = '0' + i;
        void *file = native_fopen(path, "rb");
        if (!file)
            continue;
        native_fclose(file);
        int error = 0;
        s->activity_images[i] = native_image_load(path, &error);
    }
}
/* The page's service status: Matter's or Zigbee's. */
static void read_status(enum custom_page page, char *out, unsigned int size) {
    read_text(custom_matter(page) ? MATTER_STATUS : ZIGBEE_STATUS, out, size, NOT_ENABLED);
}
/* Blanks choice rows `from` on. */
static void clear_choices(struct custom_state *s, int from) {
    for (int i = from; i < CHOICE_ROWS; i++)
        native_string_copy(&s->choices[i], "");
}
/* The schedule state is authoritative; rendering never changes it. */
static void schedule_detail(struct custom_state *s) {
    unsigned int fields[4];
    const char *error;
    read_text(SCHEDULE_STATUS, s->status, sizeof(s->status), "");
    const char *label = "Schedule unavailable";
    unsigned int remaining = 0;
    if (custom_schedule_status(s->status, fields, &error)) {
        if (s->flow.page != CUSTOM_HOLD_DURATION)
            s->flow.hold_minutes = fields[3];
        label = !fields[0]  ? "No saved schedule"
                : fields[2] ? "Schedule held"
                : fields[1] ? "Schedule on"
                            : "Schedule off (indefinite)";
        if (*error)
            label = error;
        else if (fields[2]) {
            unsigned int now = native_time(0);
            remaining = fields[2] > now ? (fields[2] - now + 59) / 60 : 0;
        }
    }
    if (s->flow.action_failed)
        label = "Request failed. Try again.";
    unsigned int n = 0;
    while (*label && n < sizeof(s->detail) - 32)
        s->detail[n++] = *label++;
    if (remaining && !s->flow.action_failed) {
        s->detail[n++] = '\n';
        n += put_number(s->detail + n, remaining);
        const char *suffix = " min remaining";
        while (*suffix)
            s->detail[n++] = *suffix++;
    }
    s->detail[n] = 0;
}
/* Lays out the current page: its texts, choices, selection and scroll.
 * `opening` is entering the page; otherwise its status refreshed and the
 * selection and scroll stay. */
static void show(void *raw, int opening) {
    float old_y = FLOAT(raw, LAYER_SCROLL_Y);
    int old_selection = WORD(common(raw), COMMON_SELECTION);
    struct custom_state *s = state(raw);
    int doom = s->flow.page == CUSTOM_DOOM, theme = s->flow.page == CUSTOM_THEMES;
    int schedule = custom_schedule_page(s->flow.page);
    if (schedule)
        schedule_detail(s);
    if (s->flow.page != CUSTOM_HOME && !schedule && !doom && !theme) {
        read_status(s->flow.page, s->status, sizeof(s->status));
        if (s->flow.page == CUSTOM_MATTER && !s->flow.action_failed &&
            custom_matter_refresh(&s->flow, s->status, 0, &s->activity_started, 0))
            s->activity_ticks = 0;
    }
    native_config_invalidate(&s->config);
    if (theme && opening) {
        load_themes(&s->themes);
        s->flow.theme_count = s->themes.count;
    }
    choices_capacity(s, theme && s->themes.count + 1 > CHOICE_ROWS ? s->themes.count + 1 : CHOICE_ROWS);
    int activity = custom_activity(s->flow.page);
    int matter_pairing = s->flow.page == CUSTOM_MATTER_PAIRING;
    int choice_count = s->flow.page == CUSTOM_SCHEDULE_HOME                                  ? 4
                       : s->flow.page == CUSTOM_HOME || s->flow.page == CUSTOM_HOLD_DURATION ? 1
                       : schedule                                                            ? 3
                       : theme                                                               ? s->themes.count + 1
                       : activity                                                            ? 1
                                                                                             : 2;
    s->flags[0] = s->flags[1] = s->flags[2] = 0;
    s->config.text_count = activity ? 3 : 2;
    /* Text row 0 reserves the image's room in blank lines, so images
     * scroll with the text: their positions come from the native text-row
     * anchors. Matter labels its open window above the code. */
    native_string_copy(&s->text[0], s->flow.page == CUSTOM_HOME ? "\n\n\n\n\n"
                                    : doom                      ? "\n\n\n\n\n"
                                    : matter_pairing            ? (s->activity_started ? "Ready to pair\n\n\n\n\n"
                                                                                       : "Opening pairing...\n\n\n\n\n")
                                                                : "");
    native_string_copy(&s->text[2], "");
    if (s->flow.page == CUSTOM_HOME) {
        char name[64];
        read_text("/etc/unfr0zen/version", name, sizeof(name), "unfr0zen");
        native_string_copy(&s->text[1], name);
        native_string_copy(&s->choices[0], "Done");
        clear_choices(s, 1);
    } else if (s->flow.page == CUSTOM_SCHEDULE_HOME) {
        native_string_copy(&s->text[0], s->detail);
        native_string_copy(&s->text[1], "Pause the schedule or hold your current temperature.");
        native_string_copy(&s->choices[0], "Done");
        native_string_copy(&s->choices[1], "Schedule");
        native_string_copy(&s->choices[2], "Hold");
        native_string_copy(&s->choices[3], "Hold duration");
        clear_choices(s, 4);
    } else if (schedule) {
        native_string_copy(&s->text[0], s->detail);
        const char *hint = s->flow.page == CUSTOM_SCHEDULE
                               ? "On resumes the saved schedule. Off pauses it indefinitely."
                           : s->flow.page == CUSTOM_HOLD
                               ? "Hold keeps your temperature until expiry. Cancel resumes the saved schedule."
                               : "Turn the ring to choose 1-1440 minutes. Click to save; Back cancels.";
        native_string_copy(&s->text[1], hint);
        if (s->flow.page == CUSTOM_HOLD_DURATION) {
            char label[32];
            unsigned int n = 0;
            const char *prefix = "Save ";
            while (*prefix)
                label[n++] = *prefix++;
            n += put_number(label + n, s->flow.hold_minutes);
            bytes_copy(label + n, " min", 5);
            native_string_copy(&s->choices[0], label);
            clear_choices(s, 1);
        } else {
            char label[40];
            const char *prefix = "Start / renew: ";
            unsigned int n = 0;
            while (*prefix)
                label[n++] = *prefix++;
            n += put_number(label + n, s->flow.hold_minutes);
            bytes_copy(label + n, " min", 5);
            native_string_copy(&s->choices[0], s->flow.page == CUSTOM_SCHEDULE ? "On" : label);
            native_string_copy(&s->choices[1], s->flow.page == CUSTOM_SCHEDULE ? "Off" : "Cancel hold");
            native_string_copy(&s->choices[2], "Back");
            clear_choices(s, 3);
        }
    } else if (theme) {
        native_string_copy(&s->text[0], s->flow.action_failed ? "nestlocal is unavailable. Try again."
                                                              : "The theme used on the home screen.");
        native_string_copy(&s->text[1], "");
        s->config.text_count = 1;
        for (int i = 0; i < s->themes.count; i++)
            native_string_copy(&s->choices[i], s->themes.labels[i]);
        native_string_copy(&s->choices[s->themes.count], "Back");
    } else if (doom) {
        native_string_copy(&s->text[1], s->flow.action_failed
                                            ? "Doom is unavailable. Try again."
                                            : "Doom runs full screen until you hold the button for three seconds.");
        native_string_copy(&s->choices[0], "Play");
        native_string_copy(&s->choices[1], "Back");
        clear_choices(s, 2);
    } else {
        /* Matter's and Zigbee's pages: status, confirmation or pairing. */
        read_text(MATTER_CODE, s->code, sizeof(s->code), "Code unavailable");
        if (matter_pairing) {
            /* Under the image: the code and the status. */
            matter_detail(s);
            native_string_copy(&s->text[1], s->flow.action_failed ? SERVICE_FAILED : s->detail);
        } else {
            /* Text-only pages start at their first real row. Empty rows still
             * occupy native line height and must not create scroll margins. */
            native_string_copy(&s->text[0], s->flow.action_failed                   ? SERVICE_FAILED
                                            : s->flow.page == CUSTOM_MATTER_CONFIRM ? MATTER_CONFIRM_TEXT
                                            : s->flow.page == CUSTOM_ZIGBEE_CONFIRM ? ZIGBEE_CONFIRM_TEXT
                                            : activity && !s->activity_started      ? "Starting pairing..."
                                                                                    : s->status);
            native_string_copy(&s->text[1], activity ? "\n\n" : "");
            s->config.text_count = activity ? 2 : 1;
        }
        if (activity) {
            /* Room for the spinner. */
            native_string_copy(&s->text[2], "\n\n");
            native_string_copy(&s->choices[0], "Back");
            clear_choices(s, 1);
            load_activity(raw);
        } else {
            native_string_copy(&s->choices[0],
                               s->flow.page == CUSTOM_MATTER_CONFIRM ? "Forget and pair" : "Enable pairing");
            native_string_copy(&s->choices[1], "Back");
            clear_choices(s, 2);
        }
    }
    if (s->image_page != s->flow.page)
        load_image(raw);
    s->config.choice_count = choice_count;
    void *base = common(raw);
    /* The choices' ids, as the stock menu names its rows. */
    for (int i = 0; i < choice_count; i++)
        s->choice_ids[i] = theme    ? (i < s->themes.count ? s->themes.ids[i] : "back")
                           : i == 0 ? "first"
                           : i == 1 ? "second"
                           : i == 2 ? "third"
                           : i == 3 ? "fourth"
                                    : "fifth";
    s->choice_ids[choice_count] = 0;
    PTR(base, COMMON_CHOICE_IDS) = s->choice_ids;
    WORD(base, COMMON_CHOICE_FIRST) = 0;
    WORD(base, COMMON_CHOICE_LAST) = choice_count - 1;
    native_config_layout(raw, &s->config, FIXED_ONE, choice_count);
    int selection = opening ? 0 : old_selection;
    if (selection >= choice_count)
        selection = choice_count - 1;
    if (selection < 0)
        selection = 0;
    native_select(raw, selection);
    WORD(base, COMMON_SELECTION_SHOWN) = selection;
    if (opening)
        s->at_top = 1;
    /* Select positions the viewport at its button. Retarget the stock
     * scroller to the document top on entry, or preserve its visible offset
     * on a status refresh. These fields are shared by the audited ABIs. */
    float top = FLOAT(raw, LAYER_CONTENT_HEIGHT) - FLOAT(raw, LAYER_VIEW_HEIGHT);
    float factor = FLOAT(raw, LAYER_SCROLL_FACTOR);
    float offset = s->at_top ? 0 : old_y;
    float target = top + (factor > 0 ? offset / factor : 0);
    if (target < 0)
        target = 0;
    if (target > top)
        target = top;
    native_scroll_target((u8 *)raw + LAYER_SCROLLER, target);
    FLOAT(raw, LAYER_SCROLL_Y) = offset;
}
/* The layer's click: acts on the selected choice. RESULT_EXIT leaves the
 * screen. */
static int click(void *raw) {
    struct custom_state *s = state(raw);
    keeper_start(PTR(common(raw), COMMON_GLOBALS));
    int selected = (int)WORD(common(raw), COMMON_SELECTION);
    enum custom_page before = s->flow.page;
    int result = custom_choose(&s->flow, selected, request_action, s);
    if (s->flow.page != before && custom_activity(s->flow.page)) {
        s->activity_ticks = 0;
        s->activity_started = 0;
        if (s->flow.page == CUSTOM_ZIGBEE_PAIRING)
            keep_awake(raw);
    }
    if (result != RESULT_EXIT)
        show(raw, s->flow.page != before);
    return result;
}
/* A status poll: follows nestlocal's status, the pairing's progress and
 * timeout, and lays the page out again when it changed. */
static void refresh_status(void *raw) {
    struct custom_state *s = state(raw);
    if (custom_schedule_page(s->flow.page)) {
        char previous[sizeof(s->detail)];
        bytes_copy(previous, s->detail, sizeof(previous));
        int minutes = s->flow.hold_minutes;
        schedule_detail(s);
        if (!same_text(previous, s->detail) || minutes != s->flow.hold_minutes)
            show(raw, 0);
        return;
    }
    if (s->flow.page == CUSTOM_HOME || s->flow.page == CUSTOM_ZIGBEE_CONFIRM || s->flow.page == CUSTOM_MATTER_CONFIRM ||
        s->flow.page == CUSTOM_DOOM || s->flow.page == CUSTOM_THEMES)
        return;
    enum custom_page before = s->flow.page;
    int pending = 0;
    if (s->flow.page == CUSTOM_MATTER_PAIRING) {
        void *request = native_fopen(MATTER_REQUEST, "r");
        pending = request != 0;
        if (request)
            native_fclose(request);
    }
    char status[96], code[32];
    read_status(s->flow.page, status, sizeof(status));
    read_text(MATTER_CODE, code, sizeof(code), "Code unavailable");
    int changed = !same_text(status, s->status) || !same_text(code, s->code);
    if (s->flow.page == CUSTOM_MATTER && !s->flow.action_failed &&
        custom_matter_refresh(&s->flow, status, pending, &s->activity_started, 0)) {
        s->activity_ticks = 0;
        changed = 1;
    }
    if (custom_activity(s->flow.page)) {
        s->activity_ticks++;
        if ((s->flow.page == CUSTOM_MATTER_PAIRING || s->activity_ticks < PAIRING_TIMEOUT_TICKS) &&
            !(s->activity_ticks % KEEP_AWAKE_TICKS))
            keep_awake(raw);
        if (s->flow.page == CUSTOM_MATTER_PAIRING) {
            changed |= custom_matter_refresh(&s->flow, status, pending, &s->activity_started, s->activity_ticks);
        } else {
            if (!s->activity_started && (starts_with(status, "Pairing...") || starts_with(status, "Pairing open"))) {
                s->activity_started = 1;
                changed = 1;
            }
            int failed = starts_with(status, "Pairing timed out") || starts_with(status, "Radio unavailable") ||
                         starts_with(status, "Not enabled") || starts_with(status, "Zigbee not enabled");
            if (failed ||
                (s->activity_started && (starts_with(status, "Paired") || starts_with(status, "Ready to pair")))) {
                custom_back(&s->flow);
                changed = 1;
            }
            if (s->flow.page == CUSTOM_ZIGBEE_PAIRING && s->activity_ticks >= PAIRING_TIMEOUT_TICKS) {
                custom_back(&s->flow);
                changed = 1;
            }
        }
    }
    if (before == CUSTOM_MATTER_PAIRING && s->flow.page != before)
        keep_awake(raw);
    if (changed) {
        show(raw, s->flow.page != before);
    }
    if (s->flow.page == CUSTOM_MATTER_PAIRING && !PTR(raw, LAYER_IMAGE))
        load_image(raw);
}

/* --- The layer's and screen's methods ------------------------------------ */
static void stop_refresh(void *raw) {
    struct custom_state *s = state(raw);
    if (s->refresh_request) {
        native_cancel_timeout(s->refresh_request);
        native_release(s->refresh_request);
        s->refresh_request = 0;
    }
}
/* The next status poll: a timeout event to the layer, its id the state. */
static void schedule_refresh(void *raw) {
    struct custom_state *s = state(raw);
    void *globals = PTR(common(raw), COMMON_GLOBALS);
    native_send_timeout(&s->refresh_request, (u32)s,
                        custom_activity(s->flow.page) ? REFRESH_ACTIVE_MS : REFRESH_IDLE_MS,
                        PTR(common(raw), COMMON_EVENT_TARGET), common(raw), PTR(globals, GLOBALS_TIMER_OFFSET));
}
static int layer_event(void *raw, void *event, void *target) {
    u32 type = native_event_type(event);
    struct custom_state *s = state(raw);
    if (type == EVENT_TIMEOUT && native_timeout_id(event) == (u32)s) {
#ifdef CUSTOM_TRACE
        trace(raw, type, native_timeout_id(event));
#endif
        if (s->active) {
            refresh_status(raw);
            schedule_refresh(raw);
        }
        return 1;
    }
    int result = native_layer_event(raw, event, target);
    /* Old Legal Info consumes a click by stopping its timer and returning
     * exit, bypassing the click vtable. Keep that native cleanup, then
     * ask the custom flow whether this click actually exits the screen. */
    if (LEGACY_CLICK && type == EVENT_CLICK && result == RESULT_EXIT)
        result = click(raw);
#ifdef CUSTOM_TRACE
    if (type == TIMER_EVENT_FIRST || type == TIMER_EVENT_SECOND)
        trace(raw, type, result);
#else
    (void)type;
#endif
    return result;
}
/* The nlLayer part's methods, adjusted to the layer. */
static int common_event(void *base, void *event, void *target) {
    return layer_event((u8 *)base - COMMON_OFFSET, event, target);
}
static void enter_layer(void *raw) { show(raw, 1); }
static void enter_common(void *base) { show((u8 *)base - COMMON_OFFSET, 1); }
static void no_reload(void *raw) { (void)raw; }
/* Where the images sit above their text rows, in the gen 3's layout:
 * Matter's code and Doom's title higher than the logo. */
#define IMAGE_LIFT        24
#define MATTER_IMAGE_LIFT 10
#define DOOM_IMAGE_LIFT   20
/* The gen 1's and gen 2's text rows reach this much further above their anchor. */
#define GEN2_TEXT_RISE    12
/* The pictured pages' and other pages' inset from the round display's
 * edge, in the gen 3's layout. */
#define PICTURED_INSET    240
#define TEXT_INSET        120
static int draw(void *raw, void *context) {
    struct custom_state *s = state(raw);
    /* Settings draws every screen while previewing its carousel tile.
     * The inner menu belongs only to the entered screen. */
    if (!s->active)
        return 0;
    void *base = common(raw);
    void *globals = PTR(base, COMMON_GLOBALS);
    int style = native_style(globals);
    /* The stock legal renderer initializes this same nlLayer text style. */
    void (*set_style)(void *, int, int) = PTR(PTR(base, 0), COMMON_SET_STYLE_SLOT);
    set_style(base, style, OPAQUE);
    int y = native_config_y(&s->config, context, WORD(base, COMMON_TEXT_BOX), 0, FIXED_ONE);
    /* The home and Doom images start at the viewport edge. Pairing headings and
     * text-only pages retain the circular display's safe inset. */
    int pictured = s->flow.page == CUSTOM_HOME || s->flow.page == CUSTOM_DOOM;
    int inset = SCALED(s, pictured ? PICTURED_INSET : TEXT_INSET);
    y = (s->at_top ? 0 : (int)FLOAT(raw, LAYER_SCROLL_Y)) - y - inset;
    void *image = PTR(raw, LAYER_IMAGE);
    if (image && (pictured || s->flow.page == CUSTOM_MATTER_PAIRING)) {
        draw_image_fn *draw_image = PTR(PTR(context, 0), CONTEXT_DRAW_IMAGE_SLOT);
#ifdef CUSTOM_TRACE
        if (!s->image_traced) {
            trace(raw, 0x494d4704, (u32)draw_image);
            trace(raw, 0x494d4705, (u32)image);
            s->image_traced = 1;
        }
#endif
        /* Anchor the image directly above the following native text row.
         * This remains correct when returning with a different selection. */
        int text_y = native_config_y(&s->config, context, WORD(base, COMMON_TEXT_BOX), 1, FIXED_ONE);
        int width = native_image_width(image), height = native_image_height(image);
        int top = y + text_y -
                  SCALED(s, IMAGE_LIFT + (custom_matter(s->flow.page)   ? MATTER_IMAGE_LIFT
                                          : s->flow.page == CUSTOM_DOOM ? DOOM_IMAGE_LIFT
                                                                        : 0)) -
                  (s->display >= GEN3_WIDTH ? 0 : GEN2_TEXT_RISE);
        /* The home and Doom images are the gen 3's size, scaled down here;
         * nestlocal draws the Matter code at the display's own. */
        if (pictured && s->display != GEN3_WIDTH) {
            draw_scaled_fn *draw_scaled = PTR(PTR(context, 0), CONTEXT_DRAW_SCALED_SLOT);
            draw_scaled(context, -SCALED(s, width) / 2, top - SCALED(s, height), image, style,
                        s->display * FIXED_ONE / GEN3_WIDTH, (void *)0);
        } else {
            draw_image(context, -width / 2, top - height, image, style);
        }
    }
    if (custom_activity(s->flow.page)) {
        void *spinner = s->activity_images[s->activity_ticks % ACTIVITY_FRAMES];
        if (spinner) {
            draw_image_fn *draw_image = PTR(PTR(context, 0), CONTEXT_DRAW_IMAGE_SLOT);
            /* Below Matter's code (text row 2), or Zigbee's status (row 1). */
            int activity_y = native_config_y(&s->config, context, WORD(base, COMMON_TEXT_BOX),
                                             custom_matter(s->flow.page) ? 2 : 1, FIXED_ONE);
            draw_image(context, -native_image_width(spinner) / 2, y + activity_y, spinner, style);
        }
    }
    native_config_draw(base, context, &s->config, 0, y, OPAQUE, WORD(base, COMMON_SELECTION), style, FIXED_ONE);
    native_scroll_draw(raw, context, FIXED_ONE);
    if (!s->drawn) {
        s->drawn = 1;
        attempt_end();
    }
    return 0;
}
static int draw_common(void *base, void *context) { return draw((u8 *)base - COMMON_OFFSET, context); }
static void title(void *screen) {
    enum custom_page root = state(raw_layer(screen))->flow.root;
    /* Local volatile stores prevent an absolute-pointer lookup table: the
     * injected PIE payload has no loader-relocated data section. */
    const char *volatile heading = "\nUNFR0ZEN";
    if (root == CUSTOM_ZIGBEE)
        heading = "\nZIGBEE";
    if (root == CUSTOM_MATTER)
        heading = "\nMATTER";
    if (root == CUSTOM_THEMES)
        heading = "\nTHEMES";
    if (root == CUSTOM_DOOM)
        heading = "\nDOOM";
    if (root == CUSTOM_SCHEDULE_HOME)
        heading = "\nSCHEDULE";
    native_string_copy((nl_string *)((u8 *)screen + SCREEN_TITLE), heading);
    native_string_copy((nl_string *)((u8 *)screen + SCREEN_SUBTITLE), "");
    native_rebuild_title(screen);
}
/* A copy of the drawing context's vtable whose text call lifts the title,
 * leaving the native context and transform as they are. */
struct title_context {
    u8 vtable[CONTEXT_VTABLE_SIZE];
    void *original;
    int y_offset;
};
static void subtitle_text(void *context, int x, int y, void *text, int count, int scale, int a, int b, int c) {
    struct title_context *view = PTR(context, 0);
    void (*draw_text)(void *, int, int, void *, int, int, int, int, int) = PTR(view->original, CONTEXT_DRAW_TEXT_SLOT);
    draw_text(context, x, y - view->y_offset, text, count, scale, a, b, c);
}
/* How far the title rises over the menu glyph, in the gen 3's layout. */
#define TITLE_LIFT      40
/* The menu glyph: 80 px, centred this far above the tile's centre at the
 * menu's resting (half) scale; on the gen 1 and gen 2, 44/80 of it, lower. */
#define GLYPH_SIZE      80
#define GLYPH_Y         (-120)
#define GEN2_GLYPH_SIZE 44
#define GEN2_GLYPH_Y    (-71)
static int screen_draw(void *screen, void *context) {
    void *globals = PTR(screen, SCREEN_GLOBALS);
    int menu_visible = !native_menu_hidden(globals);
    struct title_context view;
    struct custom_state *s = state(raw_layer(screen));
    if (menu_visible) {
        view.y_offset = (int)(SCALED(s, TITLE_LIFT) * native_menu_scale(globals));
        view.original = PTR(context, 0);
        bytes_copy(view.vtable, view.original, sizeof(view.vtable));
        PTR(view.vtable, CONTEXT_DRAW_TEXT_SLOT) = subtitle_text;
        PTR(context, 0) = view.vtable;
    }
    int result = native_screen_draw(screen, context);
    if (menu_visible)
        PTR(context, 0) = view.original;
    if (menu_visible) {
        if (!s->menu_glyph) {
            int error = 0;
            enum custom_page root = s->flow.root;
            s->menu_glyph = native_image_load(root == CUSTOM_SCHEDULE_HOME ? SCHEDULE_GLYPH
                                              : root == CUSTOM_ZIGBEE      ? ZIGBEE_GLYPH
                                              : root == CUSTOM_MATTER      ? MATTER_GLYPH
                                              : root == CUSTOM_THEMES      ? THEMES_GLYPH
                                              : root == CUSTOM_DOOM        ? DOOM_GLYPH
                                                                           : MENU_GLYPH,
                                              &error);
        }
        if (s->menu_glyph) {
            /* Native menu titles settle at half scale; glyph coordinates
             * describe the full 80px image at that resting size, as the
             * gen 3's own glyphs are. The gen 1's and gen 2's are smaller and lower:
             * 30px showing, centred 59px above the tile's centre. */
            int gen3 = s->display >= GEN3_WIDTH;
            float scale = native_menu_scale(globals) * 2;
            float glyph = scale * (gen3 ? 1.0f : (float)GEN2_GLYPH_SIZE / GLYPH_SIZE);
            draw_scaled_fn *draw_image = PTR(PTR(context, 0), CONTEXT_DRAW_SCALED_SLOT);
            draw_image(context, (int)(-native_image_width(s->menu_glyph) * glyph / 2),
                       (int)((gen3 ? GLYPH_Y : GEN2_GLYPH_Y) * scale), s->menu_glyph, native_style(globals),
                       (int)(glyph * FIXED_ONE), (void *)0);
        }
    }
    return result;
}
static void title_thunk(void *part) { title((u8 *)part - SCREEN_THIRD_THIS); }
static void enter_screen(void *screen) {
    struct custom_state *s = state(raw_layer(screen));
    s->drawn = 0;
    attempt_begin();
    s->active = 1;
    s->flow.page = s->flow.root;
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
    for (int i = 0; i < ACTIVITY_FRAMES; i++) {
        release_image(s->activity_images[i]);
        s->activity_images[i] = 0;
    }
}
static int screen_event(void *screen, void *event, void *target) {
    void *raw = raw_layer(screen);
    u32 type = native_event_type(event);
    WORD(common(raw), COMMON_SELECTION_SHOWN) = WORD(common(raw), COMMON_SELECTION);
#ifdef CUSTOM_TRACE
    if (type == EVENT_WHEEL)
        trace(raw, type, (u32)target);
#endif
    /* Settings' inherited dispatcher sends EVENT_BACK for back navigation.
     * Consume it within a child page; the parent keeps the stock exit path. */
    if (type == EVENT_BACK && custom_back(&state(raw)->flow)) {
        show(raw, 1);
        return 1;
    }
    /* The third argument is the event destination. GetType may clobber r2;
     * forward the saved destination, including NULL for user input. */
    return native_screen_event(screen, event, target);
}

/* Insert a borrowed screen before Legal Info without changing the stock
 * menu table or its sentinels. The caller's insertion and lookup own no
 * screen references; its native vector helper owns its buffer allocation. */
__attribute__((used, noinline)) static void custom_menu_insert(u32 *registers) {
    if (!custom_same((const char *)registers[1], "legalinfo"))
        return;
    void *menu = (void *)registers[MENU_OBJECT_REGISTER];
    void *screen = native_menu_lookup((void *)registers[0], "appsetup");
    if (!screen)
        return;
    if (MENU_NAMES_REGISTER) {
        custom_insert_legacy((const char **)((u8 *)menu + 0x70), (void **)((u8 *)menu + 0xf0),
                             &registers[MENU_NAMES_REGISTER], &registers[MENU_SCREENS_REGISTER],
                             &registers[MENU_NEXT_REGISTER], screen);
    } else {
        u32 begin = WORD(menu, 0x70), end = WORD(menu, 0x74), capacity = WORD(menu, 0x78);
        if (!custom_menu_vector_valid(begin, end, capacity))
            return;
        if (end == capacity) {
            native_menu_grow((u8 *)menu + 0x70, (void *)end, &screen);
        } else {
            *(void **)end = screen;
            WORD(menu, 0x74) = end + 4;
        }
    }
}
/* Preserve the lookup arguments and caller's live registers. The legacy
 * menu's validated counters are adjusted in the saved register frame. */
__attribute__((naked)) void custom_menu_lookup(void) {
    __asm__ volatile("push {r0-r12, lr}\n"
                     "mov r0, sp\n"
                     "bl custom_menu_insert\n"
                     "pop {r0-r12, lr}\n"
                     "b native_menu_lookup\n");
}

/* --- The factory -------------------------------------------------------- */
/* Replaces the factories of the stock screens the entries take the names
 * of (custom_root): builds the screen and its layer, their vtables copies of
 * stock ones with our methods in. */
void *custom_factory(void *globals, void *handler, const char *name) {
    if (attempt_pending())
        return 0;
    attempt_begin();
    void *screen = native_new(SCREEN_SIZE);
    if (!screen)
        return 0;
    native_screen_ctor(screen);
    /* Reproduce the derived factory's vptr installation after the base
     * constructor. All three pointers come from discovered ELF data. */
    PTR(screen, 0) = (void *)native_screen_vtable;
    PTR(screen, 4) = (void *)(native_screen_vtable + SCREEN_SECOND_VPTR - SCREEN_VTABLE_HEAD);
    PTR(screen, 8) = (void *)(native_screen_vtable + SCREEN_THIRD_VPTR - SCREEN_VTABLE_HEAD);
    PTR(screen, SCREEN_LAYER) = 0;
    void *background = native_layer_lookup(globals, "backgroundlayer");
    if (native_screen_init(screen, globals, handler, name, background))
        return 0;
    native_attach(screen, background);
    /* Copy only dimensions and the event destination from an initialized
     * stock layer. Allocate a separate object; do not alter that singleton. */
    void *template = native_layer_lookup(globals, "legalinfolayer");
    if (!template)
        return 0;
    void *raw = native_new(PRIMARY_SIZE + sizeof(struct custom_state));
    if (!raw)
        return 0;
    bytes_zero(raw, PRIMARY_SIZE + sizeof(struct custom_state));
    native_layer_ctor(raw);
    struct custom_state *s = state(raw);
    s->screen = screen;
    s->flow.root = custom_root(name);
    s->flow.hold_minutes = 120;
    s->display = display_width();
    s->config.text_count = 2;
    s->config.text = s->text;
    s->config.flags = s->flags;
    s->flags[0] = 0;
    s->config.choices = s->choices;
    s->config.cache_id = -1;
    for (int i = 0; i < 3; i++)
        native_string_ctor(&s->text[i]);
    choices_capacity(s, CHOICE_ROWS);
    /* Preserve negative-offset metadata and all adjustment thunks. */
    bytes_copy(s->layer_vtables, (u8 *)PTR(raw, 0) - LAYER_VTABLE_HEAD, LAYER_VTABLE_SIZE);
    PTR(raw, 0) = s->layer_vtables + LAYER_VTABLE_HEAD;
    PTR(raw, COMMON_OFFSET) = s->layer_vtables + LAYER_COMMON_VPTR;
    PTR(raw, COMMON_OFFSET + 4) = s->layer_vtables + LAYER_SECOND_VPTR;
    PTR(raw, COMMON_OFFSET + 8) = s->layer_vtables + LAYER_THIRD_VPTR;
    PTR(PTR(raw, 0), LAYER_EVENT_SLOT) = layer_event;
    PTR(PTR(raw, COMMON_OFFSET), COMMON_EVENT_SLOT) = common_event;
    PTR(PTR(raw, 0), LAYER_CLICK_SLOT) = click;
    PTR(PTR(raw, 0), LAYER_DRAW_SLOT) = draw;
    PTR(PTR(raw, 0), LAYER_ENTER_SLOT) = enter_layer;
    PTR(PTR(raw, 0), LAYER_RELOAD_SLOT) = no_reload;
    PTR(PTR(raw, COMMON_OFFSET), COMMON_ENTER_SLOT) = enter_common;
    PTR(PTR(raw, COMMON_OFFSET), COMMON_DRAW_SLOT) = draw_common;
    PTR(PTR(raw, 0), LAYER_WHEEL_SLOT) = custom_wheel;
    void *base = common(raw);
    if (native_layer_init(base, globals, PTR(template, COMMON_EVENT_TARGET), "nlcustomlayer",
                          WORD(template, COMMON_WIDTH), WORD(template, COMMON_HEIGHT)))
        return 0;
    PTR(screen, SCREEN_LAYER) = raw;
    PTR(screen, SCREEN_ACTIVE_LAYER) = base;
    native_attach(screen, base);
    bytes_copy(s->screen_vtables, (u8 *)PTR(screen, 0) - SCREEN_VTABLE_HEAD, SCREEN_VTABLE_SIZE);
    PTR(screen, 0) = s->screen_vtables + SCREEN_VTABLE_HEAD;
    PTR(screen, 4) = s->screen_vtables + SCREEN_SECOND_VPTR;
    PTR(screen, 8) = s->screen_vtables + SCREEN_THIRD_VPTR;
    PTR(PTR(screen, 0), SCREEN_EVENT_SLOT) = screen_event;
    PTR(PTR(screen, 0), SCREEN_ENTER_SLOT) = enter_screen;
    PTR(PTR(screen, 0), SCREEN_EXIT_SLOT) = exit_screen; /* retain image, hide inner menu */
    PTR(PTR(screen, 0), SCREEN_TITLE_SLOT) = title;
    PTR(PTR(screen, 0), SCREEN_DRAW_SLOT) = screen_draw;
    PTR(PTR(screen, SCREEN_THIRD_THIS), SCREEN_THIRD_TITLE_SLOT) = title_thunk;
    title(screen);
    attempt_end();
    return screen;
}
