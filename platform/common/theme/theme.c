/* Customizable home screen themes on the gen 3: preloaded into nlclient, this
 * draws the chosen theme (libnestlocal_theme.so, nestlocal's theme library) in
 * nlclient's own GLES2 context, in place of the dial's own drawing.
 *
 * The dial's draw marks the theme's place with a 480x480 marker image; the
 * texture nlclient uploads for it is tracked, and its draw is replaced by
 * the theme's, straight into the framebuffer nlclient has bound (or, scaled
 * into the menus, into that texture). nlclient's native controls stay its
 * own: the theme reads the setpoints, mode and heat-cool pick from the native
 * control model each frame. A redraw is asked of nlclient's event queue at
 * the panel's rate while the theme moves, and about once a second while it
 * stands still, as a redraw that never lights the display.
 *
 * Where everything is comes from BINDINGS, written by the setup for the
 * unit's own nlclient (unfr0zen-core's nlclient::theme); nothing is hooked
 * unless the vtable slots hold the stock methods it names. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <limits.h>
#include <link.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define BINDINGS "/etc/unfr0zen/theme.bindings"
/* On the scratch volume, with the themes (unfr0zen-core's diamond3::scratch);
 * when it is gone the dial stays stock. */
#define LIBRARY "/media/scratch/.unfr0zen/lib/libnestlocal_theme.so"
/* nestlocal's files (crates/nestlocal/src/theme/mod.rs). */
#define MARKER "/var/run/unfr0zen/theme-marker.png"
#define ENABLED "/var/run/unfr0zen/theme.native.enabled"
#define STATE "/var/run/unfr0zen/theme.native.json"

#define PTR(p, o) (*(void **)((char *)(p) + (o)))
#define WORD(p, o) (*(uint32_t *)((char *)(p) + (o)))
#define FLOAT(p, o) (*(float *)((char *)(p) + (o)))

/* --- nlclient's layouts, the same in both reviewed builds ---------------- */
/* A layer: its globals, its alpha (the background's: its colour, ARGB), its
 * scale. */
#define LAYER_GLOBALS 0x8c
#define LAYER_ALPHA 0x9c
#define LAYER_COLOR 0x9c
#define LAYER_SCALE 0xa0
/* The slider layer's heat-cool state: 1 and 2 edit the low and high end, 3
 * and 4 have them picked, 5 has neither. */
#define RANGE_STATE_OFFSET 0xec
/* The figures layer's opacity (float). */
#define FIGURES_OPACITY 0xa8
/* A view's screen, and a screen's active layer. */
#define VIEW_SCREEN 0x34
#define SCREEN_ACTIVE_LAYER 0x20
/* A layer's vptr sits this far into its vtable from the bound dial slot. */
#define DIAL_SLOT_IN_VTABLE 0x10
/* An image's pixels. */
#define IMAGE_PIXELS 0x0c
/* The client's globals, which tells the stock client from another queue. */
#define CLIENT_GLOBALS 0x70
/* The drawing context: the layer it draws into, where that is itself when
 * not nested; its clear colours; its vtable's surface and drawing slots,
 * and a surface's slot that clears it. */
#define CONTEXT_TARGET 0x14
#define CONTEXT_OWN_TARGET 0x48
#define CONTEXT_CLEAR_COLOR 0x0c
#define CONTEXT_CLEAR_COLOR_2 0x10
#define CONTEXT_DRAW_SCALED_SLOT 0x60
#define CONTEXT_SURFACE_SLOT 0xa0
#define SURFACE_CLEAR_SLOT 0x0c
/* The globals' vtable: setpoints (Celsius 16.16), the unit and the mode. */
#define GLOBALS_TARGET_SLOT 0xa8
#define GLOBALS_FAHRENHEIT_SLOT 0xa0
#define GLOBALS_LOW_SLOT 0xf0
#define GLOBALS_HIGH_SLOT 0xe8
#define GLOBALS_MODE_SLOT 0x138

/* nlclient's events and their size. */
#define EVENT_SIZE 16
#define EVENT_WHEEL 0x40000004
#define EVENT_ACTIVITY 0x40000005

/* 16.16 fixed point's 1. */
#define FIXED_ONE 65536
/* The dial's and the marker's size, in pixels. */
#define DIAL_SIZE 480
/* The marker's first pixel, RGBA 12 34 56 ff. */
#define MARKER_PIXEL 0xff123456u

/* Calls a native function at a bound address, or a virtual method. */
#define NATIVE(type, address) ((type)(base + (address)))
#define VIRTUAL(type, object, slot) ((type)PTR(PTR(object, 0), slot))
typedef int (*int_fn)(void *);
typedef unsigned (*unsigned_fn)(void *);
typedef float (*float_fn)(void *);
typedef void (*void_fn)(void *);
typedef void *(*pointer_fn)(void *);
typedef void *(*new_fn)(unsigned);
typedef void *(*lookup_fn)(void *, const char *);
typedef void *(*image_load_fn)(const char *, int *);
typedef void (*activity_init_fn)(void *, unsigned, unsigned);
typedef void (*enqueue_fn)(void *, void *, void *);
typedef int (*event_fn)(void *, void *, void *);
typedef void (*draw_scaled_fn)(void *, int, int, void *, int, int, void *);

/* A 16.16 native temperature, in degrees. */
static float from_fixed(int value) { return value / 65536.0f; }

/* --- Bindings ------------------------------------------------------------ */
/* The bindings, by the names the setup writes. */
static uintptr_t dial_slot, dial_draw, dial_event_slot, dial_event, figures_slot, figures_draw;
static uintptr_t background_slot, background_draw, brightness_manager, read_als, settings_view_vtable;
static uintptr_t thermostat_view_vtable, view_offset, current_temperature, current_humidity, leaf_slot;
static uintptr_t activity_client, activity_ctor, activity_init, delete_native, enqueue, event_type;
static uintptr_t image_load, layer_lookup, menu_hidden, menu_scale, new_native;
static const struct { const char *name; uintptr_t *value; } names[] = {
    {"dial_slot", &dial_slot},
    {"dial_draw", &dial_draw},
    {"dial_event_slot", &dial_event_slot},
    {"dial_event", &dial_event},
    {"figures_slot", &figures_slot},
    {"figures_draw", &figures_draw},
    {"background_slot", &background_slot},
    {"background_draw", &background_draw},
    {"brightness_manager", &brightness_manager},
    {"read_als", &read_als},
    {"settings_view", &settings_view_vtable},
    {"thermostat_view", &thermostat_view_vtable},
    {"view_offset", &view_offset},
    {"current_temperature", &current_temperature},
    {"current_humidity", &current_humidity},
    {"leaf_slot", &leaf_slot},
    {"activity_client", &activity_client},
    {"activity_ctor", &activity_ctor},
    {"activity_init", &activity_init},
    {"delete", &delete_native},
    {"enqueue", &enqueue},
    {"event_type", &event_type},
    {"image_load", &image_load},
    {"layer_lookup", &layer_lookup},
    {"menu_hidden", &menu_hidden},
    {"menu_scale", &menu_scale},
    {"new", &new_native},
};

/* --- GLES2 ------------------------------------------------------------------ */
#define GL_FRONT 0x0404
#define GL_BACK 0x0405
#define GL_TRIANGLES 0x0004
#define GL_CULL_FACE 0x0b44
#define GL_CULL_FACE_MODE 0x0b45
#define GL_FRONT_FACE 0x0b46
#define GL_DEPTH_TEST 0x0b71
#define GL_DEPTH_WRITEMASK 0x0b72
#define GL_DEPTH_CLEAR_VALUE 0x0b73
#define GL_DEPTH_FUNC 0x0b74
#define GL_STENCIL_TEST 0x0b90
#define GL_STENCIL_CLEAR_VALUE 0x0b91
#define GL_STENCIL_FUNC 0x0b92
#define GL_STENCIL_VALUE_MASK 0x0b93
#define GL_STENCIL_FAIL 0x0b94
#define GL_STENCIL_PASS_DEPTH_FAIL 0x0b95
#define GL_STENCIL_PASS_DEPTH_PASS 0x0b96
#define GL_STENCIL_REF 0x0b97
#define GL_STENCIL_WRITEMASK 0x0b98
#define GL_VIEWPORT 0x0ba2
#define GL_BLEND 0x0be2
#define GL_SCISSOR_BOX 0x0c10
#define GL_SCISSOR_TEST 0x0c11
#define GL_COLOR_CLEAR_VALUE 0x0c22
#define GL_COLOR_WRITEMASK 0x0c23
#define GL_UNPACK_ALIGNMENT 0x0cf5
#define GL_PACK_ALIGNMENT 0x0d05
#define GL_TEXTURE_2D 0x0de1
#define GL_FLOAT 0x1406
#define GL_BLEND_EQUATION_RGB 0x8009
#define GL_TEXTURE_BINDING_2D 0x8069
#define GL_BLEND_DST_RGB 0x80c8
#define GL_BLEND_SRC_RGB 0x80c9
#define GL_BLEND_DST_ALPHA 0x80ca
#define GL_BLEND_SRC_ALPHA 0x80cb
#define GL_TEXTURE0 0x84c0
#define GL_ACTIVE_TEXTURE 0x84e0
#define GL_VERTEX_ARRAY_BINDING_OES 0x85b5
#define GL_VERTEX_ATTRIB_ARRAY_ENABLED 0x8622
#define GL_VERTEX_ATTRIB_ARRAY_SIZE 0x8623
#define GL_VERTEX_ATTRIB_ARRAY_STRIDE 0x8624
#define GL_VERTEX_ATTRIB_ARRAY_TYPE 0x8625
#define GL_VERTEX_ATTRIB_ARRAY_POINTER 0x8645
#define GL_STENCIL_BACK_FUNC 0x8800
#define GL_STENCIL_BACK_FAIL 0x8801
#define GL_STENCIL_BACK_PASS_DEPTH_FAIL 0x8802
#define GL_STENCIL_BACK_PASS_DEPTH_PASS 0x8803
#define GL_BLEND_EQUATION_ALPHA 0x883d
#define GL_VERTEX_ATTRIB_ARRAY_NORMALIZED 0x886a
#define GL_ARRAY_BUFFER 0x8892
#define GL_ELEMENT_ARRAY_BUFFER 0x8893
#define GL_ARRAY_BUFFER_BINDING 0x8894
#define GL_ELEMENT_ARRAY_BUFFER_BINDING 0x8895
#define GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING 0x889f
#define GL_STREAM_DRAW 0x88e0
#define GL_FRAGMENT_SHADER 0x8b30
#define GL_VERTEX_SHADER 0x8b31
#define GL_COMPILE_STATUS 0x8b81
#define GL_LINK_STATUS 0x8b82
#define GL_CURRENT_PROGRAM 0x8b8d
#define GL_STENCIL_BACK_REF 0x8ca3
#define GL_STENCIL_BACK_VALUE_MASK 0x8ca4
#define GL_STENCIL_BACK_WRITEMASK 0x8ca5
#define GL_FRAMEBUFFER_BINDING 0x8ca6
#define GL_RENDERBUFFER_BINDING 0x8ca7
#define GL_COLOR_ATTACHMENT0 0x8ce0
#define GL_FRAMEBUFFER 0x8d40
#define GL_RENDERBUFFER 0x8d41

extern void glGetIntegerv(unsigned, int *);
extern void glGetFloatv(unsigned, float *);
extern void glGetBooleanv(unsigned, unsigned char *);
extern unsigned char glIsEnabled(unsigned);
extern void glEnable(unsigned);
extern void glDisable(unsigned);
extern void glScissor(int, int, int, int);
extern void glColorMask(unsigned char, unsigned char, unsigned char, unsigned char);
extern unsigned glCreateShader(unsigned);
extern void glShaderSource(unsigned, int, const char *const *, const int *);
extern void glCompileShader(unsigned);
extern void glGetShaderiv(unsigned, unsigned, int *);
extern unsigned glCreateProgram(void);
extern void glAttachShader(unsigned, unsigned);
extern void glBindAttribLocation(unsigned, unsigned, const char *);
extern void glLinkProgram(unsigned);
extern void glGetProgramiv(unsigned, unsigned, int *);
extern void glDeleteShader(unsigned);
extern void glDeleteProgram(unsigned);
extern void glUseProgram(unsigned);
extern int glGetUniformLocation(unsigned, const char *);
extern void glGenBuffers(int, unsigned *);
extern void glBindBuffer(unsigned, unsigned);
extern void glBufferData(unsigned, ptrdiff_t, const void *, unsigned);
extern void glGetVertexAttribiv(unsigned, unsigned, int *);
extern void glGetVertexAttribPointerv(unsigned, unsigned, void **);
extern void glVertexAttribPointer(unsigned, int, unsigned, unsigned char, int, const void *);
extern void glEnableVertexAttribArray(unsigned);
extern void glDisableVertexAttribArray(unsigned);
extern void glViewport(int, int, int, int);
extern void glActiveTexture(unsigned);
extern void glUniform1i(int, int);
extern void glBindFramebuffer(unsigned, unsigned);
extern void glBindRenderbuffer(unsigned, unsigned);
extern void glGenFramebuffers(int, unsigned *);
extern void glFramebufferTexture2D(unsigned, unsigned, unsigned, unsigned, int);
extern void glBlendFuncSeparate(unsigned, unsigned, unsigned, unsigned);
extern void glBlendEquationSeparate(unsigned, unsigned);
extern void glStencilFuncSeparate(unsigned, unsigned, int, unsigned);
extern void glStencilMaskSeparate(unsigned, unsigned);
extern void glStencilOpSeparate(unsigned, unsigned, unsigned, unsigned);
extern void glClearStencil(int);
extern void glClearDepthf(float);
extern void glDepthMask(unsigned char);
extern void glDepthFunc(unsigned);
extern void glFrontFace(unsigned);
extern void glCullFace(unsigned);
extern void glPixelStorei(unsigned, int);

/* nlclient's own GL calls under the ones this library replaces or calls
 * past its hooks, looked up once. */
#define NEXT(fn, name) ((fn) ? (fn) : ((fn) = dlsym(RTLD_NEXT, name)))
static void (*next_bind_texture)(unsigned, unsigned);
static void (*next_clear_color)(float, float, float, float);
static void (*next_draw_arrays)(unsigned, int, int);

/* The capabilities a theme may change, saved and restored. */
#define SAVED_CAP_COUNT 5
static const unsigned saved_caps[SAVED_CAP_COUNT] = {
    GL_BLEND,
    GL_DEPTH_TEST,
    GL_STENCIL_TEST,
    GL_CULL_FACE,
    GL_SCISSOR_TEST,
};
/* The texture units and vertex attributes saved. */
#define SAVED_UNITS 3
#define SAVED_ATTRIBUTES 3

/* --- State ---------------------------------------------------------------- */
static uintptr_t base;
static int (*original_draw)(void *, void *), (*original_figures)(void *, void *), (*original_background)(void *, void *);
static _Atomic(void *) globals;
static atomic_int pending, rendering, ambient_ready, showing_home;
/* The dial's last draw was the theme's: its marker draws take the theme. */
static atomic_int dial_themed;
/* `hooked` once nlclient's vtables carry this library's methods. */
static int full_theme, in_batch, failed, hooked;
static void *preview_figures, *theme_image, *theme_pixels;
static unsigned generation;
static _Atomic(float) native_target;

/* What the theme reads from the native control model (theme_plugin.rs's
 * NativeState): setpoints in Celsius, the unit, mode, the HVAC's state, the
 * leaf and the heat-cool pick (RANGE_STATE_OFFSET's). */
struct native_theme_state { float target, current, low, high, humidity; uint32_t unit, mode, hvac, leaf, range; };
/* native_theme_state's hvac: what the backdrop shows. */
enum {
    HVAC_IDLE = 0,
    HVAC_HEATING = 1,
    HVAC_COOLING = 2,
};
static struct native_theme_state native_state;
static pthread_mutex_t native_state_lock = PTHREAD_MUTEX_INITIALIZER;
static atomic_uint native_hvac;
static void native_read_state(struct native_theme_state *out) {
    pthread_mutex_lock(&native_state_lock);
    *out = native_state;
    pthread_mutex_unlock(&native_state_lock);
}
static void native_write_state(const struct native_theme_state *state) {
    pthread_mutex_lock(&native_state_lock);
    native_state = *state;
    pthread_mutex_unlock(&native_state_lock);
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}
static long long monotonic_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000000000LL + t.tv_nsec;
}
/* Whether the context draws nested in another layer (Settings' carousel
 * draws the thermostat as a tile so). */
static int nested(void *ctx) { return PTR(ctx, CONTEXT_TARGET) != (char *)ctx + CONTEXT_OWN_TARGET; }
static void *thermostat_view(void *g) {
    void *view = g ? PTR(g, view_offset) : 0;
    return view && PTR(view, 0) == (void *)(base + thermostat_view_vtable) ? view : 0;
}
static int settings_view(void *g) {
    void *view = g ? PTR(g, view_offset) : 0;
    return view && PTR(view, 0) == (void *)(base + settings_view_vtable);
}
/* Whether the thermostat view's active layer is the dial, the menu hidden. */
static int owns_home(void *g) {
    void *view = thermostat_view(g);
    if (!view || !NATIVE(int_fn, menu_hidden)(g)) return 0;
    void *screen = PTR(view, VIEW_SCREEN);
    void *layer = screen ? PTR(screen, SCREEN_ACTIVE_LAYER) : 0;
    return layer && PTR(layer, 0) == (void *)(base + dial_slot - DIAL_SLOT_IN_VTABLE);
}
/* Whether a theme is chosen and has not failed. */
static int selected(void) { return !failed && access(ENABLED, F_OK) == 0; }
/* STATE's modification time when the theme failed. */
static struct timespec failed_state;
static struct timespec state_time(void) {
    struct stat st;
    return stat(STATE, &st) ? (struct timespec){0} : st.st_mtim;
}
/* The theme fails: stock draws until nestlocal publishes another state (a
 * theme chosen, the buckets changed), when the theme is tried again. */
static void fail(void) {
    failed = 1;
    failed_state = state_time();
}
static void retry_failed(void) {
    struct timespec t = state_time();
    if (t.tv_sec != failed_state.tv_sec || t.tv_nsec != failed_state.tv_nsec) failed = 0;
}
/* Whether the theme draws the dial, opaque and unscaled or not: whenever
 * nlclient draws it in the thermostat view. That is not only while it is the
 * active layer: pressed for the menu, the dial fades and shrinks out after
 * the menu has become active. */
static int theming(void *layer, void *ctx) {
    return !nested(ctx) && selected() && thermostat_view(PTR(layer, LAYER_GLOBALS));
}
/* Whether a backdrop colour is the heating (orange) or cooling (blue) one. */
#define ACCENT(r, g, b) (((b) == 0 && (r) > 0 && (r) >= (g) * 2) || ((r) == 0 && (b) > 0 && (b) >= (g) * 3))

static unsigned compile(unsigned kind, const char *source) {
    unsigned shader = glCreateShader(kind);
    glShaderSource(shader, 1, &source, 0);
    glCompileShader(shader);
    int okay;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &okay);
    if (!okay) { glDeleteShader(shader); return 0; }
    return shader;
}

/* --- The theme's library -------------------------------------------------- */
/* The library's draw, its input hooks, and the destination it draws into. */
static unsigned (*render)(void *(*)(const char *), unsigned, unsigned, float);
static _Atomic(void (*)(void)) lua_wheel, lua_enter;
/* The library's seconds until its theme needs another frame: 0 while it
 * moves, infinite while it stands still. */
static float (*next_frame)(void);
/* When the theme next needs a frame, in CLOCK_MONOTONIC ns: 0 at once. */
static atomic_llong theme_due;
static unsigned destination;
/* The library's glBindFramebuffer: framebuffer 0 is the destination. */
static void bind_framebuffer(unsigned target, unsigned framebuffer) {
    glBindFramebuffer(target, framebuffer ? framebuffer : destination);
}
/* The library's GL and the native state, by name. */
static void *proc(const char *name) {
    if (!strcmp(name, "nestlocal_native_theme_v1")) return native_read_state;
    if (!strcmp(name, "glBindFramebuffer")) return bind_framebuffer;
    static void *gles;
    if (!gles) gles = dlopen("libGLESv2.so", RTLD_NOW | RTLD_LOCAL);
    return gles ? dlsym(gles, name) : 0;
}
static void wheel(void) { void (*input)(void) = atomic_load(&lua_wheel); if (input) input(); }
static void home(void) { void (*enter)(void) = atomic_load(&lua_enter); if (enter) enter(); }

/* Copies the theme's frame, texture `source`, into texture `target` (the
 * menus' scaled theme), turned and swizzled as nlclient's textures are. */
static void into_texture(unsigned source, unsigned target) {
    static unsigned program, vertices, framebuffer;
    if (!program) {
        unsigned vs = compile(GL_VERTEX_SHADER, "attribute vec2 p;varying vec2 uv;void main(){gl_Position=vec4(p,0.,1.);"
                                                "vec2 t=p*.5+.5;uv=vec2(1.-t.y,1.-t.x);}");
        unsigned fs = compile(GL_FRAGMENT_SHADER, "precision mediump float;varying vec2 uv;uniform sampler2D image;"
                                                  "void main(){gl_FragColor=texture2D(image,uv).bgra;}");
        if (!vs || !fs) { fail(); return; }
        program = glCreateProgram();
        glAttachShader(program, vs); glAttachShader(program, fs);
        glBindAttribLocation(program, 0, "p");
        glLinkProgram(program);
        glDeleteShader(vs); glDeleteShader(fs);
        int okay;
        glGetProgramiv(program, GL_LINK_STATUS, &okay);
        if (!okay) { glDeleteProgram(program); program = 0; fail(); return; }
        glGenBuffers(1, &vertices);
        glGenFramebuffers(1, &framebuffer);
    }
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, target, 0);
    glViewport(0, 0, DIAL_SIZE, DIAL_SIZE);
    for (unsigned i = 0; i < SAVED_CAP_COUNT; i++) glDisable(saved_caps[i]);
    glUseProgram(program);
    glActiveTexture(GL_TEXTURE0);
    NEXT(next_bind_texture, "glBindTexture")(GL_TEXTURE_2D, source);
    glUniform1i(glGetUniformLocation(program, "image"), 0);
    /* One triangle covering the viewport. */
    const float points[] = {-1, -1, 3, -1, -1, 3};
    glBindBuffer(GL_ARRAY_BUFFER, vertices);
    glBufferData(GL_ARRAY_BUFFER, sizeof(points), points, GL_STREAM_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, 0, 0, 0);
    NEXT(next_draw_arrays, "glDrawArrays")(GL_TRIANGLES, 0, 3);
}

/* The integer state saved, by its index in gl_state's `ints`. */
enum saved {
    SAVED_PROGRAM,
    SAVED_ARRAY_BUFFER,
    SAVED_ELEMENT_ARRAY_BUFFER,
    SAVED_FRAMEBUFFER,
    SAVED_RENDERBUFFER,
    SAVED_ACTIVE_TEXTURE,
    SAVED_BLEND_SRC_RGB,
    SAVED_BLEND_DST_RGB,
    SAVED_BLEND_SRC_ALPHA,
    SAVED_BLEND_DST_ALPHA,
    SAVED_BLEND_EQUATION_RGB,
    SAVED_BLEND_EQUATION_ALPHA,
    SAVED_FRONT_FACE,
    SAVED_CULL_FACE_MODE,
    /* Each face's stencil, in STENCIL_* order. */
    SAVED_STENCIL_FRONT,
    SAVED_STENCIL_BACK = SAVED_STENCIL_FRONT + 7,
    SAVED_UNPACK_ALIGNMENT = SAVED_STENCIL_BACK + 7,
    SAVED_PACK_ALIGNMENT,
    SAVED_STENCIL_CLEAR,
    SAVED_DEPTH_FUNC,
    SAVED_COUNT,
};
/* A face's stencil state, from its SAVED_STENCIL_ index. */
enum stencil {
    STENCIL_FUNC,
    STENCIL_REF,
    STENCIL_VALUE_MASK,
    STENCIL_WRITEMASK,
    STENCIL_FAIL,
    STENCIL_DEPTH_FAIL,
    STENCIL_DEPTH_PASS,
};
static const unsigned saved_queries[SAVED_COUNT] = {
    [SAVED_PROGRAM] = GL_CURRENT_PROGRAM,
    [SAVED_ARRAY_BUFFER] = GL_ARRAY_BUFFER_BINDING,
    [SAVED_ELEMENT_ARRAY_BUFFER] = GL_ELEMENT_ARRAY_BUFFER_BINDING,
    [SAVED_FRAMEBUFFER] = GL_FRAMEBUFFER_BINDING,
    [SAVED_RENDERBUFFER] = GL_RENDERBUFFER_BINDING,
    [SAVED_ACTIVE_TEXTURE] = GL_ACTIVE_TEXTURE,
    [SAVED_BLEND_SRC_RGB] = GL_BLEND_SRC_RGB,
    [SAVED_BLEND_DST_RGB] = GL_BLEND_DST_RGB,
    [SAVED_BLEND_SRC_ALPHA] = GL_BLEND_SRC_ALPHA,
    [SAVED_BLEND_DST_ALPHA] = GL_BLEND_DST_ALPHA,
    [SAVED_BLEND_EQUATION_RGB] = GL_BLEND_EQUATION_RGB,
    [SAVED_BLEND_EQUATION_ALPHA] = GL_BLEND_EQUATION_ALPHA,
    [SAVED_FRONT_FACE] = GL_FRONT_FACE,
    [SAVED_CULL_FACE_MODE] = GL_CULL_FACE_MODE,
    [SAVED_STENCIL_FRONT + STENCIL_FUNC] = GL_STENCIL_FUNC,
    [SAVED_STENCIL_FRONT + STENCIL_REF] = GL_STENCIL_REF,
    [SAVED_STENCIL_FRONT + STENCIL_VALUE_MASK] = GL_STENCIL_VALUE_MASK,
    [SAVED_STENCIL_FRONT + STENCIL_WRITEMASK] = GL_STENCIL_WRITEMASK,
    [SAVED_STENCIL_FRONT + STENCIL_FAIL] = GL_STENCIL_FAIL,
    [SAVED_STENCIL_FRONT + STENCIL_DEPTH_FAIL] = GL_STENCIL_PASS_DEPTH_FAIL,
    [SAVED_STENCIL_FRONT + STENCIL_DEPTH_PASS] = GL_STENCIL_PASS_DEPTH_PASS,
    [SAVED_STENCIL_BACK + STENCIL_FUNC] = GL_STENCIL_BACK_FUNC,
    [SAVED_STENCIL_BACK + STENCIL_REF] = GL_STENCIL_BACK_REF,
    [SAVED_STENCIL_BACK + STENCIL_VALUE_MASK] = GL_STENCIL_BACK_VALUE_MASK,
    [SAVED_STENCIL_BACK + STENCIL_WRITEMASK] = GL_STENCIL_BACK_WRITEMASK,
    [SAVED_STENCIL_BACK + STENCIL_FAIL] = GL_STENCIL_BACK_FAIL,
    [SAVED_STENCIL_BACK + STENCIL_DEPTH_FAIL] = GL_STENCIL_BACK_PASS_DEPTH_FAIL,
    [SAVED_STENCIL_BACK + STENCIL_DEPTH_PASS] = GL_STENCIL_BACK_PASS_DEPTH_PASS,
    [SAVED_UNPACK_ALIGNMENT] = GL_UNPACK_ALIGNMENT,
    [SAVED_PACK_ALIGNMENT] = GL_PACK_ALIGNMENT,
    [SAVED_STENCIL_CLEAR] = GL_STENCIL_CLEAR_VALUE,
    [SAVED_DEPTH_FUNC] = GL_DEPTH_FUNC,
};
struct attribute { int enabled, size, stride, type, normalized, buffer; void *pointer; };
/* Every piece of nlclient's GL state the theme's library can touch. */
struct gl_state {
    int ints[SAVED_COUNT];
    int vao, viewport[4], scissor[4], textures[SAVED_UNITS];
    float clear[4], depth;
    unsigned char mask[4], depth_mask, enabled[SAVED_CAP_COUNT];
    struct attribute attributes[SAVED_ATTRIBUTES];
};

static void gl_save(struct gl_state *s) {
    /* Stays 0 where the driver has no vertex array objects. */
    s->vao = 0;
    glGetIntegerv(GL_VERTEX_ARRAY_BINDING_OES, &s->vao);
    for (unsigned i = 0; i < SAVED_COUNT; i++) glGetIntegerv(saved_queries[i], &s->ints[i]);
    glGetIntegerv(GL_VIEWPORT, s->viewport);
    glGetIntegerv(GL_SCISSOR_BOX, s->scissor);
    glGetFloatv(GL_COLOR_CLEAR_VALUE, s->clear);
    glGetFloatv(GL_DEPTH_CLEAR_VALUE, &s->depth);
    glGetBooleanv(GL_COLOR_WRITEMASK, s->mask);
    glGetBooleanv(GL_DEPTH_WRITEMASK, &s->depth_mask);
    for (unsigned i = 0; i < SAVED_CAP_COUNT; i++) s->enabled[i] = glIsEnabled(saved_caps[i]);
    for (unsigned i = 0; i < SAVED_UNITS; i++) {
        glActiveTexture(GL_TEXTURE0 + i);
        glGetIntegerv(GL_TEXTURE_BINDING_2D, &s->textures[i]);
    }
    for (unsigned i = 0; i < SAVED_ATTRIBUTES; i++) {
        struct attribute *a = &s->attributes[i];
        glGetVertexAttribiv(i, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &a->enabled);
        glGetVertexAttribiv(i, GL_VERTEX_ATTRIB_ARRAY_SIZE, &a->size);
        glGetVertexAttribiv(i, GL_VERTEX_ATTRIB_ARRAY_STRIDE, &a->stride);
        glGetVertexAttribiv(i, GL_VERTEX_ATTRIB_ARRAY_TYPE, &a->type);
        glGetVertexAttribiv(i, GL_VERTEX_ATTRIB_ARRAY_NORMALIZED, &a->normalized);
        glGetVertexAttribiv(i, GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING, &a->buffer);
        glGetVertexAttribPointerv(i, GL_VERTEX_ATTRIB_ARRAY_POINTER, &a->pointer);
    }
}

static void gl_restore(const struct gl_state *s) {
    const int *old = s->ints;
    glBindFramebuffer(GL_FRAMEBUFFER, (unsigned)old[SAVED_FRAMEBUFFER]);
    static void (*bind_vao)(unsigned);
    static int looked_up;
    if (!looked_up) { bind_vao = proc("glBindVertexArrayOES"); looked_up = 1; }
    if (bind_vao) bind_vao((unsigned)s->vao);
    for (unsigned i = 0; i < SAVED_ATTRIBUTES; i++) {
        const struct attribute *a = &s->attributes[i];
        glBindBuffer(GL_ARRAY_BUFFER, (unsigned)a->buffer);
        glVertexAttribPointer(i, a->size, (unsigned)a->type, (unsigned char)a->normalized, a->stride, a->pointer);
        if (a->enabled) glEnableVertexAttribArray(i); else glDisableVertexAttribArray(i);
    }
    glBindBuffer(GL_ARRAY_BUFFER, (unsigned)old[SAVED_ARRAY_BUFFER]);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, (unsigned)old[SAVED_ELEMENT_ARRAY_BUFFER]);
    glBindRenderbuffer(GL_RENDERBUFFER, (unsigned)old[SAVED_RENDERBUFFER]);
    glUseProgram((unsigned)old[SAVED_PROGRAM]);
    for (unsigned i = 0; i < SAVED_UNITS; i++) {
        glActiveTexture(GL_TEXTURE0 + i);
        NEXT(next_bind_texture, "glBindTexture")(GL_TEXTURE_2D, (unsigned)s->textures[i]);
    }
    glActiveTexture((unsigned)old[SAVED_ACTIVE_TEXTURE]);
    glBlendFuncSeparate(old[SAVED_BLEND_SRC_RGB], old[SAVED_BLEND_DST_RGB],
                        old[SAVED_BLEND_SRC_ALPHA], old[SAVED_BLEND_DST_ALPHA]);
    glBlendEquationSeparate(old[SAVED_BLEND_EQUATION_RGB], old[SAVED_BLEND_EQUATION_ALPHA]);
    glFrontFace(old[SAVED_FRONT_FACE]);
    glCullFace(old[SAVED_CULL_FACE_MODE]);
    for (unsigned i = 0; i < 2; i++) {
        unsigned face = i ? GL_BACK : GL_FRONT;
        const int *st = &old[i ? SAVED_STENCIL_BACK : SAVED_STENCIL_FRONT];
        glStencilFuncSeparate(face, st[STENCIL_FUNC], st[STENCIL_REF], st[STENCIL_VALUE_MASK]);
        glStencilMaskSeparate(face, st[STENCIL_WRITEMASK]);
        glStencilOpSeparate(face, st[STENCIL_FAIL], st[STENCIL_DEPTH_FAIL], st[STENCIL_DEPTH_PASS]);
    }
    glPixelStorei(GL_UNPACK_ALIGNMENT, old[SAVED_UNPACK_ALIGNMENT]);
    glPixelStorei(GL_PACK_ALIGNMENT, old[SAVED_PACK_ALIGNMENT]);
    glClearStencil(old[SAVED_STENCIL_CLEAR]);
    glDepthFunc(old[SAVED_DEPTH_FUNC]);
    NEXT(next_clear_color, "glClearColor")(s->clear[0], s->clear[1], s->clear[2], s->clear[3]);
    glClearDepthf(s->depth);
    glDepthMask(s->depth_mask);
    glColorMask(s->mask[0], s->mask[1], s->mask[2], s->mask[3]);
    glViewport(s->viewport[0], s->viewport[1], s->viewport[2], s->viewport[3]);
    glScissor(s->scissor[0], s->scissor[1], s->scissor[2], s->scissor[3]);
    for (unsigned i = 0; i < SAVED_CAP_COUNT; i++) {
        if (s->enabled[i]) glEnable(saved_caps[i]); else glDisable(saved_caps[i]);
    }
}

/* The theme's library, loaded on its first frame; false when it cannot be. */
static int load_library(void) {
    if (render) return 1;
    void *library = dlopen(LIBRARY, RTLD_NOW | RTLD_LOCAL);
    if (library) {
        render = dlsym(library, "nestlocal_theme_draw");
        atomic_store(&lua_wheel, dlsym(library, "nestlocal_theme_input"));
        atomic_store(&lua_enter, dlsym(library, "nestlocal_theme_home"));
        next_frame = dlsym(library, "nestlocal_theme_next");
    }
    if (!render) { fprintf(stderr, "theme: %s\n", dlerror()); fail(); return 0; }
    return 1;
}

/* Draws the theme's frame: into texture `image`, or (0) straight into the
 * framebuffer nlclient has bound. The library shares nlclient's context,
 * so every piece of state it can touch is saved and restored. */
static void kick(void);
static void draw_theme(unsigned image) {
    if (!load_library()) return;
    struct gl_state saved;
    gl_save(&saved);
    glColorMask(1, 1, 1, 1);
    destination = (unsigned)saved.ints[SAVED_FRAMEBUFFER];
    unsigned result = render(proc, destination, image != 0, atomic_load(&native_target));
    if (!result) fail();
    else {
        float wait = next_frame ? next_frame() : 0;
        long long due = wait <= 0 ? 0 : wait >= 1e6f ? LLONG_MAX : monotonic_ns() + (long long)(wait * 1e9f);
        /* Sooner than the ticker last planned for (a still theme starting to
         * move, as coming home): it plans again, or it sleeps the settled
         * period through while nlclient draws nothing. */
        if (due < atomic_exchange(&theme_due, due)) kick();
        if (image) into_texture(result, image);
    }
    gl_restore(&saved);
}

/* --- The marker's textures ------------------------------------------------ */
#define MAX_TEXTURES 32
/* How long a scaled frame in the menus is kept while the theme is not
 * drawn anew, in seconds. */
#define SNAPSHOT_LIFE 0.5
/* The textures nlclient uploaded the marker into: the snapshot they hold,
 * when it was drawn, and whether the texture holds a single texel. */
static struct texture { unsigned id, snapshot, generation, texel; double captured; } textures[MAX_TEXTURES];
static struct texture *tracked(unsigned id) {
    for (unsigned i = 0; i < MAX_TEXTURES; i++) if (textures[i].id == id && id) return &textures[i];
    return 0;
}
static int marker(int width, int height, const void *pixels) {
    return pixels && theme_pixels && width == DIAL_SIZE && height == DIAL_SIZE &&
           (pixels == theme_pixels || *(const uint32_t *)pixels == MARKER_PIXEL);
}
/* Tracks the bound texture as holding the marker. */
static struct texture *uploaded(void) {
    int id;
    glGetIntegerv(GL_TEXTURE_BINDING_2D, &id);
    struct texture *texture = tracked((unsigned)id);
    if (!texture) for (unsigned i = 0; i < MAX_TEXTURES; i++) if (!textures[i].id) { texture = &textures[i]; texture->id = (unsigned)id; break; }
    if (texture) texture->snapshot = 0;
    return texture;
}
static void (*next_tex_image)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *);
/* The marker's last upload's formats, for full_size. */
static int marker_internal;
static unsigned marker_format, marker_type;
/* The marker's uploads: storage only, its pixels never needed. nlclient
 * uploads it anew for every dial draw; at full size the theme draws in its
 * place and the texture is never sampled, so it takes a single texel, and
 * the theme's size only once a scaled draw needs it (full_size). */
void glTexImage2D(unsigned target, int level, int internal, int width, int height, int border, unsigned format,
                  unsigned type, const void *pixels) {
    if (!next_tex_image) next_tex_image = dlsym(RTLD_NEXT, "glTexImage2D");
    int theme = marker(width, height, pixels), texel = theme && full_theme;
    next_tex_image(target, level, internal, texel ? 1 : width, texel ? 1 : height, border, format, type, theme ? 0 : pixels);
    if (!theme) return;
    marker_internal = internal, marker_format = format, marker_type = type;
    struct texture *texture = uploaded();
    if (texture) texture->texel = texel;
}
/* Gives the bound marker texture `texture` the theme's size. */
static void full_size(struct texture *texture) {
    if (!texture->texel) return;
    next_tex_image(GL_TEXTURE_2D, 0, marker_internal, DIAL_SIZE, DIAL_SIZE, 0, marker_format, marker_type, 0);
    texture->texel = 0;
}
void glTexSubImage2D(unsigned target, int level, int x, int y, int width, int height, unsigned format,
                     unsigned type, const void *pixels) {
    static void (*real)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *);
    if (!real) real = dlsym(RTLD_NEXT, "glTexSubImage2D");
    if (marker(width, height, pixels)) uploaded();
    else real(target, level, x, y, width, height, format, type, pixels);
}
void glDeleteTextures(int count, const unsigned *ids) {
    static void (*real)(int, const unsigned *);
    if (!real) real = dlsym(RTLD_NEXT, "glDeleteTextures");
    for (int i = 0; i < count; i++) {
        struct texture *texture = tracked(ids[i]);
        if (texture) memset(texture, 0, sizeof(*texture));
    }
    real(count, ids);
}
/* nlclient submits the theme into its own 480-square framebuffer before
 * composing the display: drawing the theme there keeps the native order. */
void glDrawArrays(unsigned mode, int first, int count) {
    static unsigned drawn_generation, snapshot = 1;
    void (*real)(unsigned, int, int) = NEXT(next_draw_arrays, "glDrawArrays");
    if (!atomic_load(&rendering) && !atomic_load(&dial_themed)) { real(mode, first, count); return; }
    int id;
    glGetIntegerv(GL_TEXTURE_BINDING_2D, &id);
    struct texture *texture = tracked((unsigned)id);
    if (!texture || !selected()) { real(mode, first, count); return; }
    int framebuffer;
    glGetIntegerv(GL_FRAMEBUFFER_BINDING, &framebuffer);
    if (!full_theme || framebuffer != 0) {
        /* Scaled or faded, into and out of the menus: the theme's frame into
         * the marker's texture, anew for each dial draw, as the dial moves;
         * kept for SNAPSHOT_LIFE while it is not drawn anew. */
        if (texture->snapshot != snapshot || texture->generation != generation
            || now() - texture->captured > SNAPSHOT_LIFE) {
            full_size(texture);
            in_batch = 1; draw_theme((unsigned)id); in_batch = 0;
            if (!failed) { texture->snapshot = snapshot; texture->generation = generation; texture->captured = now(); }
        }
        real(mode, first, count);
        return;
    }
    /* Once per dial draw: the marker may be drawn more than once a frame. */
    if (drawn_generation == generation) return;
    snapshot++;
    in_batch = 1; draw_theme(0); in_batch = 0;
    drawn_generation = generation;
}

/* --- Hooked layer methods ------------------------------------------------- */
/* Settings' carousel previews keep the target visible while its home layer
 * is faded. */
static int preview(void *ctx, void *g) {
    if (!preview_figures) preview_figures = NATIVE(lookup_fn, layer_lookup)(g, "temperaturelayer");
    if (!preview_figures) return 0;
    float *opacity = &FLOAT(preview_figures, FIGURES_OPACITY), saved = *opacity;
    *opacity = 1.0f;
    int result = original_figures(preview_figures, ctx);
    *opacity = saved;
    return result;
}
static void start_ticker(void);
/* The dial's draw: the native state for the theme, and the marker where the
 * dial would show. */
static int draw(void *layer, void *ctx) {
    atomic_store(&globals, PTR(layer, LAYER_GLOBALS));
    start_ticker();
    if (failed) retry_failed();
    int showing = owns_home(PTR(layer, LAYER_GLOBALS)) && selected();
    if (!atomic_exchange(&rendering, showing) && showing) kick();
    if (!showing) atomic_store(&showing_home, 0);
    if (nested(ctx)) return selected() ? 0 : original_draw(layer, ctx);
    int themed = theming(layer, ctx);
    atomic_store(&dial_themed, themed);
    if (!themed) {
        atomic_store(&showing_home, 0);
        full_theme = 0;
        return selected() ? 0 : original_draw(layer, ctx);
    }
    void *g = PTR(layer, LAYER_GLOBALS);
    /* Native setpoints are Celsius 16.16; themes use the selected unit. */
    float target = from_fixed(VIRTUAL(int_fn, g, GLOBALS_TARGET_SLOT)(g));
    int fahrenheit = VIRTUAL(int_fn, g, GLOBALS_FAHRENHEIT_SLOT)(g);
    atomic_store(&native_target, fahrenheit ? target * 1.8f + 32.0f : target);
    struct native_theme_state state = {
        .target = target,
        .current = from_fixed(NATIVE(int_fn, current_temperature)(g)),
        .low = from_fixed(VIRTUAL(int_fn, g, GLOBALS_LOW_SLOT)(g)),
        .high = from_fixed(VIRTUAL(int_fn, g, GLOBALS_HIGH_SLOT)(g)),
        .humidity = from_fixed(NATIVE(int_fn, current_humidity)(g)),
        .unit = (unsigned)fahrenheit,
        .mode = VIRTUAL(unsigned_fn, g, GLOBALS_MODE_SLOT)(g),
        .hvac = atomic_load(&native_hvac),
        .leaf = VIRTUAL(unsigned_fn, g, leaf_slot)(g),
        .range = WORD(layer, RANGE_STATE_OFFSET),
    };
    native_write_state(&state);
    /* Home is entered once it shows, not as the dial fades out to the menu. */
    if (showing && !atomic_exchange(&showing_home, 1)) home();
    if (!theme_image) {
        int error = 0;
        theme_image = NATIVE(image_load_fn, image_load)(MARKER, &error);
        if (!theme_image) return original_draw(layer, ctx);
        theme_pixels = PTR(theme_image, IMAGE_PIXELS);
    }
    uint32_t alpha = WORD(layer, LAYER_ALPHA);
    float scale = FLOAT(layer, LAYER_SCALE);
    float scaled_menu = NATIVE(float_fn, menu_scale)(g);
    full_theme = alpha == 255 && scale == 1.0f && scaled_menu == 1.0f;
    int result = 0;
    if (alpha == 0) {
        /* Faded out under the menu: the dial's own draw, unseen, alone. */
        atomic_store(&dial_themed, 0);
        return original_draw(layer, ctx);
    }
    if (!full_theme) {
        /* The dial's own draw runs unseen for what it keeps in step. */
        WORD(layer, LAYER_ALPHA) = 0;
        result = original_draw(layer, ctx);
        WORD(layer, LAYER_ALPHA) = alpha;
    }
    generation++;
    int width = (int)(DIAL_SIZE * scale);
    VIRTUAL(draw_scaled_fn, ctx, CONTEXT_DRAW_SCALED_SLOT)(ctx, -width / 2, -width / 2, theme_image, (int)(alpha * FIXED_ONE / 255), (int)(scale * FIXED_ONE), 0);
    return result;
}
/* The theme owns the home dial's backdrop: native menu clears in the heating
 * or cooling colour are made black. */
void glClearColor(float r, float g, float b, float a) {
    int accent = ACCENT(r, g, b);
    if (hooked && accent && a > 0 && !in_batch && selected() && !settings_view(atomic_load(&globals))) r = g = b = 0;
    NEXT(next_clear_color, "glClearColor")(r, g, b, a);
}
#define OPAQUE_BLACK 0xff000000u
static int background(void *layer, void *ctx) {
    atomic_store(&globals, PTR(layer, LAYER_GLOBALS));
    uint32_t color = WORD(layer, LAYER_COLOR), r = (color >> 16) & 255, g = (color >> 8) & 255, b = color & 255;
    /* The home dial's heating/cooling backdrop belongs to the theme. */
    int thermostat = ACCENT(r, g, b);
    int tile = settings_view(PTR(layer, LAYER_GLOBALS));
    if (thermostat_view(PTR(layer, LAYER_GLOBALS)) && (thermostat || !(color & 0xffffffu)))
        atomic_store(&native_hvac, thermostat ? (r ? HVAC_HEATING : HVAC_COOLING) : HVAC_IDLE);
    if (selected() && !tile && (thermostat || color == OPAQUE_BLACK)) {
        WORD(layer, LAYER_COLOR) = color & OPAQUE_BLACK;
        /* The GPU context keeps its clear colour apart from the layer. */
        WORD(ctx, CONTEXT_CLEAR_COLOR) = WORD(layer, LAYER_COLOR);
        WORD(ctx, CONTEXT_CLEAR_COLOR_2) = WORD(layer, LAYER_COLOR);
        if (!nested(ctx) && !owns_home(PTR(layer, LAYER_GLOBALS))) {
            /* A complete opaque background keeps partial menu frames off the front page. */
            void *surface = VIRTUAL(pointer_fn, ctx, CONTEXT_SURFACE_SLOT)(ctx);
            if (surface) {
                VIRTUAL(void_fn, surface, SURFACE_CLEAR_SLOT)(surface);
                WORD(layer, LAYER_COLOR) = color;
                return 0;
            }
        }
    }
    int result = original_background(layer, ctx);
    if (selected() && tile && thermostat && NATIVE(float_fn, menu_scale)(PTR(layer, LAYER_GLOBALS)) < 1.0f)
        preview(ctx, PTR(layer, LAYER_GLOBALS));
    WORD(layer, LAYER_COLOR) = color;
    return result;
}
/* The thermostat's figures (over the dial): the theme has its own. */
static int figures(void *layer, void *ctx) {
    preview_figures = layer;
    if (!selected()) return original_figures(layer, ctx);
    return settings_view(PTR(layer, LAYER_GLOBALS)) ? preview(ctx, PTR(layer, LAYER_GLOBALS)) : 0;
}

/* --- Redraws -------------------------------------------------------------- */
/* A handler queued on nlclient's event queue, run on its UI thread. Its
 * vtable is its own handle field: slot 0 is the handler. */
struct handler { void *vtable; int (*handle)(struct handler *, void *); void *globals; };
/* nlClient's results for a handled event: 1 redraws while the display is
 * lit; 5 would light it first, re-arming the backlight's ramp each time. */
enum {
    RESULT_REDRAW = 1,
};
static int redraw(struct handler *h, void *event) {
    (void)event;
    /* Automatic brightness, seeded once from the native ambient light sampler. */
    if (!atomic_load(&ambient_ready)) {
        void *manager = NATIVE(pointer_fn, brightness_manager)(h->globals);
        if (manager) {
            NATIVE(void_fn, read_als)(manager);
            atomic_store(&ambient_ready, 1);
        }
    }
    atomic_store(&rendering, owns_home(h->globals) && selected());
    if (!atomic_load(&rendering)) { atomic_store(&pending, 0); atomic_store(&showing_home, 0); }
    NATIVE(void_fn, delete_native)(h);
    return RESULT_REDRAW;
}
/* The redraw periods, in ns: the panel's rate while the theme moves; while
 * it stands still, often enough to follow the clock and what nlclient's
 * model or nestlocal's state change; until the ambient light is sampled. */
#define FRAME_NS 16666667
#define SETTLED_NS 1000000000
#define IDLE_NS 100000000
#define SECOND_NS 1000000000
/* The frame periods a queued redraw may go unswapped before the ticker
 * falls back to the settled period: nlclient swaps nothing while the
 * display is off. */
#define STALLED_FRAMES 30
static pthread_mutex_t tick_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t tick_cond;
static atomic_int ticking;
static int kicked;
/* Wakes the ticker for a frame now: the ring turned, or the theme shows
 * again. */
static void kick(void) {
    if (!atomic_load(&ticking)) return;
    pthread_mutex_lock(&tick_lock);
    kicked = 1;
    pthread_cond_signal(&tick_cond);
    pthread_mutex_unlock(&tick_lock);
}
/* Waits until `until` (CLOCK_MONOTONIC ns), or with a negative `until`
 * until kicked: true when kicked. */
static int wait_tick(long long until) {
    struct timespec t = { (time_t)(until / SECOND_NS), (long)(until % SECOND_NS) };
    pthread_mutex_lock(&tick_lock);
    while (!kicked) {
        if (until < 0) pthread_cond_wait(&tick_cond, &tick_lock);
        else if (pthread_cond_timedwait(&tick_cond, &tick_lock, &t) == ETIMEDOUT) break;
    }
    int woken = kicked;
    kicked = 0;
    pthread_mutex_unlock(&tick_lock);
    return woken;
}
/* A redraw for nlclient's queue, one at a time (the next only once a frame
 * has been swapped): at the panel's rate while the theme moves, when it
 * asks while it stands still, and not at all while it does not show. */
static void *ticker(void *unused) {
    (void)unused;
    long long next = monotonic_ns();
    unsigned stalled = 0;
    for (;;) {
        long long at = monotonic_ns(), due = atomic_load(&theme_due);
        int showing = atomic_load(&rendering);
        if (showing && stalled < STALLED_FRAMES && due <= at + FRAME_NS) {
            next += FRAME_NS;
            if (next < at) next = at;
        } else if (showing) {
            next = due > at && due < at + SETTLED_NS ? due : at + SETTLED_NS;
        } else {
            next = atomic_load(&ambient_ready) ? -1 : at + IDLE_NS;
        }
        if (wait_tick(next)) next = monotonic_ns();
        void *g = atomic_load(&globals);
        if (!g || (!atomic_load(&rendering) && atomic_load(&ambient_ready))) continue;
        if (atomic_exchange(&pending, 1)) { if (stalled < STALLED_FRAMES) stalled++; continue; }
        stalled = 0;
        void *client = NATIVE(pointer_fn, activity_client)(g);
        if (!client || PTR(client, CLIENT_GLOBALS) != g) { atomic_store(&pending, 0); continue; }
        struct handler *h = NATIVE(new_fn, new_native)(sizeof(*h));
        void *event = NATIVE(new_fn, new_native)(EVENT_SIZE);
        h->vtable = &h->handle;
        h->handle = redraw;
        h->globals = g;
        NATIVE(void_fn, activity_ctor)(event);
        NATIVE(activity_init_fn, activity_init)(event, EVENT_ACTIVITY, 0);
        NATIVE(enqueue_fn, enqueue)(client, event, h);
    }
    return 0;
}
unsigned eglSwapBuffers(void *display, void *surface) {
    static unsigned (*real)(void *, void *);
    if (!real) real = dlsym(RTLD_NEXT, "eglSwapBuffers");
    atomic_store(&pending, 0);
    unsigned result = real(display, surface);
    start_ticker();
    return result;
}
/* The thermostat screen's events: a turn of the ring tells the theme at once. */
static int event(void *screen, void *input, void *target) {
    int result = NATIVE(event_fn, dial_event)(screen, input, target);
    if (owns_home(atomic_load(&globals)) && NATIVE(unsigned_fn, event_type)(input) == EVENT_WHEEL) { wheel(); kick(); }
    return result;
}
/* NSPR must initialize its thread bookkeeping before a preload starts workers. */
static void start_ticker(void) {
    static atomic_int started;
    if (atomic_load(&started) || !atomic_load(&globals)) return;
    int (*initialized)(void) = dlsym(RTLD_DEFAULT, "PR_Initialized");
    if (!initialized || !initialized()) return;
    if (atomic_exchange(&started, 1)) return;
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_init(&tick_cond, &attr);
    pthread_condattr_destroy(&attr);
    pthread_t thread;
    if (pthread_create(&thread, 0, ticker, 0)) fprintf(stderr, "theme: no redraw thread\n");
    else { pthread_detach(thread); atomic_store(&ticking, 1); }
}

/* --- Loading -------------------------------------------------------------- */
/* The hooked slots, each a bit of `mapped` once found in a readable segment
 * and of `executable` when that segment holds code as well. */
#define HOOKED_SLOTS 4
static unsigned mapped, executable;
static int find_base(struct dl_phdr_info *info, size_t size, void *arg) {
    (void)size; (void)arg;
    if (info->dlpi_name[0]) return 0;
    base = info->dlpi_addr;
    const uintptr_t slots[HOOKED_SLOTS] = {dial_slot, figures_slot, background_slot, dial_event_slot};
    for (unsigned i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *p = &info->dlpi_phdr[i];
        if (p->p_type != PT_LOAD || !(p->p_flags & PF_R)) continue;
        for (unsigned j = 0; j < HOOKED_SLOTS; j++)
            if (slots[j] >= p->p_vaddr && slots[j] + sizeof(void *) <= p->p_vaddr + p->p_memsz) {
                mapped |= 1u << j;
                if (p->p_flags & PF_X) executable |= 1u << j;
            }
    }
    return 1;
}
#define PAGE_SIZE 4096
/* Makes hooked slot `index` writable, its page keeping any code it holds
 * executable: false when it cannot be. */
static int writable(unsigned index, uintptr_t slot) {
    int protection = PROT_READ | PROT_WRITE | (executable & (1u << index) ? PROT_EXEC : 0);
    return !mprotect((void *)((base + slot) & ~(uintptr_t)(PAGE_SIZE - 1u)), PAGE_SIZE, protection);
}
static void patch(uintptr_t slot, void *replacement, void **original) {
    void **p = (void **)(base + slot);
    *original = *p;
    *p = replacement;
}
/* Every binding, from BINDINGS; false (and nothing hooked) for any missing. */
static int read_bindings(void) {
    FILE *file = fopen(BINDINGS, "r");
    if (!file) return 0;
    unsigned found = 0;
    char name[32];
    unsigned long value;
    while (fscanf(file, "%31s %lx", name, &value) == 2)
        for (unsigned i = 0; i < sizeof(names) / sizeof(names[0]); i++)
            if (!strcmp(name, names[i].name) && !*names[i].value) { *names[i].value = value; found++; }
    fclose(file);
    return found == sizeof(names) / sizeof(names[0]);
}
/* nlclient's helpers inherit its environment: they load the other preloads
 * without this one, which only nlclient's GL resolves. */
#define SELF "libunfr0zen-theme.so"
static void unload_from_children(void) {
    const char *preload = getenv("LD_PRELOAD");
    if (!preload) return;
    char kept[256] = "";
    size_t used = 0;
    const size_t self = sizeof(SELF) - 1;
    for (const char *word = preload; *word;) {
        size_t length = strcspn(word, " :");
        if (length && !(length >= self && !strncmp(word + length - self, SELF, self)) &&
            used + length + 2 < sizeof(kept)) {
            if (used) kept[used++] = ' ';
            memcpy(kept + used, word, length);
            kept[used += length] = 0;
        }
        word += length;
        while (*word == ' ' || *word == ':') word++;
    }
    if (used) setenv("LD_PRELOAD", kept, 1);
    else unsetenv("LD_PRELOAD");
}
__attribute__((constructor)) static void init(void) {
    char exe[64];
    ssize_t length = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
    if (length < 0) return;
    exe[length] = 0;
    if (strcmp(exe, "/nestlabs/sbin/nlclient")) return;
    unload_from_children();
    if (!read_bindings()) { fprintf(stderr, "theme: no bindings for this nlclient\n"); return; }
    dl_iterate_phdr(find_base, 0);
    if (mapped != (1u << HOOKED_SLOTS) - 1 || PTR((void *)base, background_slot) != (void *)(base + background_draw) ||
        PTR((void *)base, dial_slot) != (void *)(base + dial_draw) ||
        PTR((void *)base, figures_slot) != (void *)(base + figures_draw) ||
        PTR((void *)base, dial_event_slot) != (void *)(base + dial_event)) {
        fprintf(stderr, "theme: nlclient's vtables are not the bound ones\n");
        return;
    }
    /* Every slot writable before any is hooked: all of the hooks or none. */
    if (!writable(0, dial_slot) || !writable(1, figures_slot) || !writable(2, background_slot) ||
        !writable(3, dial_event_slot)) {
        fprintf(stderr, "theme: nlclient's vtables cannot be written\n");
        return;
    }
    void *unused;
    patch(background_slot, background, (void **)&original_background);
    patch(dial_slot, draw, (void **)&original_draw);
    patch(figures_slot, figures, (void **)&original_figures);
    patch(dial_event_slot, event, &unused);
    hooked = 1;
}
