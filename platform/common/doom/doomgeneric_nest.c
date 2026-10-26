// Doom on the Nest: doomgeneric's platform layer, one binary for the gen 3
// and the gen 2, told apart by their panels.
//
// Display: the gen 3's 480x640 panel (fb0, 32 bpp, two pages) is mounted on
// its side with the round face in its top 480x480; the gen 2's is the face
// itself, 320x320 and upright. The 320x200 picture is scaled to the face's
// width (on the gen 3, 480x300, the status bar's corners under the bezel)
// and turned onto the panel as it is mounted; each frame is drawn on the
// hidden page, then shown, or straight onto the only one.
//
// Controls (the gen 3's dial and Enter, the gen 2's ring and power key):
// in a level the player walks forward all along, the wheel turns
// (as a mouse would) and the button fires; use is pressed now and then, so
// doors and switches open on the way. Dead, the button restarts. In the
// menus and on the title screens the wheel moves through items and a press
// selects (answering yes to any question). Holding the button for three
// seconds quits.
//
// Sound: the piezo (pwm-beeper) plays one tone at a time. Effects are
// Doom's own PC speaker sounds (the DP lumps); music is the score played
// one note at a time, its highest note held, while no effect sounds.

#include <errno.h>
#include <fcntl.h>
#include <linux/fb.h>
#include <math.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <linux/kd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <dirent.h>

#include "doomgeneric.h"
#include "doomkeys.h"
#include "doomstat.h"
#include "d_event.h"
#include "m_controls.h"
#include "i_sound.h"
#include "i_system.h"
#include "w_wad.h"
#include "z_zone.h"

// evdev's event as the unit's 32-bit kernel lays it out (a 32-bit timeval,
// whatever the C library's time_t), and the codes used here; <linux/input.h>
// is left out as its KEY_ names clash with Doom's.
struct input_event {
    uint32_t sec, usec;
    uint16_t type, code;
    int32_t value;
};
_Static_assert(sizeof(struct input_event) == 16, "the kernel's input_event is 16 bytes");
#define EV_KEY 0x01
#define EV_REL 0x02
#define EV_SND 0x12
#define REL_X 0x00 /* the gen 2's ring */
#define REL_DIAL 0x07 /* the gen 3's */
#define SND_TONE 0x02
#define BUTTON 28 /* evdev's KEY_ENTER, the gen 3's button */
#define POWER 116 /* evdev's KEY_POWER, the gen 2's */

// Mouse units a dial count turns: one detent (about 120 counts) turns
// about 16 degrees.
#define TURN_PER_COUNT 3
// Use is pressed for USE_MS every USE_EVERY_MS while walking: long enough
// for a tic (1/35 s) to see it.
#define USE_EVERY_MS 400
#define USE_MS 60
// Holding the button this long quits, back to the thermostat.
#define QUIT_MS 3000

static uint8_t *fb;
static struct fb_var_screeninfo var;
static int fb_fd, stride, bytes_pp, page, pages, face;
// Whether the panel takes the picture's 0xRRGGBB words as they are.
static int xrgb;
// For each pixel of the face's square on the panel, the picture's pixel
// shown there, or -1 outside the picture.
static int *source;
// The console's mode before Doom took it, put back on the way out.
static int tty = -1, tty_mode = KD_TEXT;

static uint32_t now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000u + ts.tv_nsec / 1000000u;
}

static void die(const char *what)
{
    fprintf(stderr, "doom: %s: %s\n", what, strerror(errno));
    exit(1);
}

// The backlight up, if whatever ran before left it off.
static void light_up(void)
{
    DIR *dir = opendir("/sys/class/backlight");
    struct dirent *e;
    while (dir && (e = readdir(dir))) {
        if (e->d_name[0] == '.') continue;
        char path[300], value[16] = "";
        snprintf(path, sizeof path, "/sys/class/backlight/%s/brightness", e->d_name);
        FILE *f = fopen(path, "r");
        if (!f) continue;
        if (!fgets(value, sizeof value, f)) value[0] = 0;
        fclose(f);
        if (atoi(value) != 0) continue;
        snprintf(path, sizeof path, "/sys/class/backlight/%s/max_brightness", e->d_name);
        if ((f = fopen(path, "r"))) {
            if (!fgets(value, sizeof value, f)) value[0] = 0;
            fclose(f);
        }
        snprintf(path, sizeof path, "/sys/class/backlight/%s/brightness", e->d_name);
        if ((f = fopen(path, "w"))) {
            fprintf(f, "%d\n", atoi(value) > 0 ? atoi(value) : 1);
            fclose(f);
        }
    }
    if (dir) closedir(dir);
}

static void restore_console(void)
{
    if (tty >= 0)
        ioctl(tty, KDSETMODE, tty_mode);
}

static void init_display(void)
{
    struct fb_fix_screeninfo fix;
    fb_fd = open("/dev/fb0", O_RDWR);
    if (fb_fd < 0) die("/dev/fb0");
    if (ioctl(fb_fd, FBIOGET_VSCREENINFO, &var)) die("fb0 info");
    int gen3 = var.xres == 480 && var.yres == 640;
    if (!gen3 && var.xres != var.yres) {
        fprintf(stderr, "doom: fb0 is %ux%u, neither the gen 3's 480x640 nor a round face's square\n", var.xres, var.yres);
        exit(1);
    }
    if (var.bits_per_pixel != 16 && var.bits_per_pixel != 24 && var.bits_per_pixel != 32) {
        fprintf(stderr, "doom: fb0 is at %u bpp\n", var.bits_per_pixel);
        exit(1);
    }
    // Two pages, where the panel can pan between them.
    if (var.yres_virtual < 2 * var.yres) {
        struct fb_var_screeninfo two = var;
        two.yres_virtual = 2 * var.yres;
        two.yoffset = 0;
        if (ioctl(fb_fd, FBIOPUT_VSCREENINFO, &two) == 0)
            ioctl(fb_fd, FBIOGET_VSCREENINFO, &var);
    }
    if (ioctl(fb_fd, FBIOGET_FSCREENINFO, &fix)) die("fb0 info");
    pages = var.yres_virtual >= 2 * var.yres ? 2 : 1;
    bytes_pp = var.bits_per_pixel / 8;
    xrgb = bytes_pp == 4 && var.red.offset == 16 && var.green.offset == 8 && var.blue.offset == 0
        && var.red.length == 8 && var.green.length == 8 && var.blue.length == 8;
    stride = fix.line_length;
    size_t size = (size_t)fix.line_length * var.yres_virtual;
    fb = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fb_fd, 0);
    if (fb == MAP_FAILED) die("fb0 mmap");
    memset(fb, 0, size);
    ioctl(fb_fd, FBIOBLANK, FB_BLANK_UNBLANK);
    light_up();
    // The console's text stays off the panel while Doom draws.
    tty = open("/dev/tty0", O_RDWR);
    if (tty >= 0 && ioctl(tty, KDGETMODE, &tty_mode) == 0 && ioctl(tty, KDSETMODE, KD_GRAPHICS) == 0)
        atexit(restore_console);

    // 320x200 across the face's width, centred.
    face = gen3 ? 480 : (int)var.xres;
    double scale = face / 320.0;
    double w = 320 * scale, h = 200 * scale;
    double left = (face - w) / 2, top = (face - h) / 2;
    source = malloc(sizeof(int) * face * face);
    for (int y = 0; y < face; y++) {
        for (int x = 0; x < face; x++) {
            // On the gen 3 the upright face's point (u, v) is the panel's
            // (479 - v, u).
            int u = gen3 ? y : x, v = gen3 ? face - 1 - x : y;
            double sx = (u + 0.5 - left) / scale, sy = (v + 0.5 - top) / scale;
            source[y * face + x] = (sx >= 0 && sx < 320 && sy >= 0 && sy < 200)
                ? (int)sy * DOOMGENERIC_RESX + (int)sx : -1;
        }
    }
}

// A picture pixel (0xRRGGBB) as the panel lays it out.
static uint32_t panel_pixel(uint32_t rgb)
{
    uint32_t r = rgb >> 16 & 0xff, g = rgb >> 8 & 0xff, b = rgb & 0xff;
    return (r >> (8 - var.red.length)) << var.red.offset | (g >> (8 - var.green.length)) << var.green.offset
         | (b >> (8 - var.blue.length)) << var.blue.offset;
}

void DG_DrawFrame(void)
{
    int back = pages == 2 ? 1 - page : 0;
    uint8_t *dst = fb + (size_t)back * var.yres * stride;
    for (int y = 0; y < face; y++) {
        const int *src = source + y * face;
        uint8_t *row = dst + (size_t)y * stride;
        if (bytes_pp == 4) {
            uint32_t *px = (uint32_t *)row;
            for (int x = 0; x < face; x++)
                px[x] = src[x] < 0 ? 0 : xrgb ? DG_ScreenBuffer[src[x]] : panel_pixel(DG_ScreenBuffer[src[x]]);
            continue;
        }
        for (int x = 0; x < face; x++) {
            uint32_t value = src[x] < 0 ? 0 : panel_pixel(DG_ScreenBuffer[src[x]]);
            for (int b = 0; b < bytes_pp; b++)
                row[x * bytes_pp + b] = value >> (8 * b);
        }
    }
    if (pages == 2) {
        var.yoffset = back * var.yres;
        if (ioctl(fb_fd, FBIOPAN_DISPLAY, &var) == 0)
            page = back;
    }
}

// --- Input -----------------------------------------------------------

static int inputs[16], n_inputs;
// Keys waiting for doomgeneric, as (pressed << 8 | key).
static int keys[64], key_head, key_tail;
static int button_down, menu_dial;
static uint32_t pressed_at;

static void queue_key(int pressed, unsigned char key)
{
    keys[key_head] = pressed << 8 | key;
    key_head = (key_head + 1) % 64;
}

static void tap_key(unsigned char key)
{
    queue_key(1, key);
    queue_key(0, key);
}

static void post_mouse(int buttons, int dx)
{
    event_t ev = { .type = ev_mouse, .data1 = buttons, .data2 = dx, .data3 = 0 };
    D_PostEvent(&ev);
}

// The menus, the title screens and demos take keys; the game (levels,
// intermissions, the finale) takes the mouse.
static int in_menus(void)
{
    return menuactive || demoplayback || gamestate == GS_DEMOSCREEN;
}

static void init_input(void)
{
    DIR *dir = opendir("/dev/input");
    if (!dir) die("/dev/input");
    struct dirent *e;
    while ((e = readdir(dir)) && n_inputs < 16) {
        if (strncmp(e->d_name, "event", 5)) continue;
        char path[300];
        snprintf(path, sizeof path, "/dev/input/%s", e->d_name);
        int fd = open(path, O_RDONLY | O_NONBLOCK);
        if (fd >= 0) inputs[n_inputs++] = fd;
    }
    closedir(dir);
}

static void poll_input(void)
{
    uint32_t t = now_ms();
    int dial = 0, released = 0;
    struct input_event ev;
    for (int i = 0; i < n_inputs; i++) {
        while (read(inputs[i], &ev, sizeof ev) == sizeof ev) {
            if (ev.type == EV_REL && ev.code == REL_DIAL) {
                dial += ev.value;
            } else if (ev.type == EV_REL && ev.code == REL_X) {
                // The ring's counts run positive clockwise, the dial's negative.
                dial -= ev.value;
            } else if (ev.type == EV_KEY && (ev.code == BUTTON || ev.code == POWER) && ev.value != 2) {
                released |= button_down && !ev.value;
                if (ev.value && !button_down) pressed_at = t;
                button_down = ev.value != 0;
            }
        }
    }
    // As the menu's Quit: the settings are saved on the way out.
    if (button_down && t - pressed_at >= QUIT_MS)
        I_Quit();
    if (in_menus()) {
        if (released) {
            tap_key(KEY_ENTER);
            tap_key('y');
        }
        // One menu step for two detents, as the thermostat's menus; counts
        // run negative clockwise, and clockwise goes down.
        menu_dial -= dial;
        for (; menu_dial >= 240; menu_dial -= 240)
            tap_key(KEY_DOWNARROW);
        for (; menu_dial <= -240; menu_dial += 240)
            tap_key(KEY_UPARROW);
        return;
    }
    menu_dial = 0;
    // Mouse buttons: fire, use, forward (whatever the saved config says).
    mousebfire = 0;
    mousebuse = 1;
    mousebstrafe = -1;
    mousebforward = 2;
    int alive = gamestate == GS_LEVEL && players[consoleplayer].playerstate == PST_LIVE;
    int buttons;
    if (alive) {
        // Walking all along, using now and then so that doors and
        // switches open on the way, firing while the button is down.
        int use = t % USE_EVERY_MS < USE_MS;
        buttons = (button_down ? 1 : 0) | (use ? 2 : 0) | 4;
    } else {
        // Dead, the button is use (to restart); elsewhere it fires (to
        // hurry intermissions and the finale on).
        buttons = button_down ? (gamestate == GS_LEVEL ? 2 : 1) : 0;
    }
    static int last_buttons = -1;
    if (dial || buttons != last_buttons)
        post_mouse(buttons, -dial * TURN_PER_COUNT);
    last_buttons = buttons;
}

int DG_GetKey(int *pressed, unsigned char *key)
{
    if (key_tail == key_head)
        return 0;
    *pressed = keys[key_tail] >> 8;
    *key = keys[key_tail] & 0xff;
    key_tail = (key_tail + 1) % 64;
    return 1;
}

// --- The piezo -------------------------------------------------------

static int piezo = -1;

static void tone(int hz)
{
    static int playing = -1;
    if (piezo < 0 || hz == playing)
        return;
    struct input_event ev = { .type = EV_SND, .code = SND_TONE, .value = hz };
    if (write(piezo, &ev, sizeof ev) == sizeof ev)
        playing = hz;
}

static void open_piezo(void)
{
    DIR *dir = opendir("/sys/class/input");
    struct dirent *e;
    while (dir && (e = readdir(dir))) {
        if (strncmp(e->d_name, "event", 5)) continue;
        char path[300], name[64] = "";
        snprintf(path, sizeof path, "/sys/class/input/%s/device/name", e->d_name);
        FILE *f = fopen(path, "r");
        if (!f) continue;
        if (!fgets(name, sizeof name, f)) name[0] = 0;
        fclose(f);
        if (strncmp(name, "pwm-beeper", 10) == 0) {
            snprintf(path, sizeof path, "/dev/input/%s", e->d_name);
            piezo = open(path, O_WRONLY);
            break;
        }
    }
    if (dir) closedir(dir);
    if (piezo < 0)
        fprintf(stderr, "doom: no pwm-beeper, so no sound\n");
}

static pthread_mutex_t sound_lock = PTHREAD_MUTEX_INITIALIZER;

// The PC speaker's tones, as the timer divisors Doom's DP lumps index
// (from Chocolate Doom's i_pcsound.c).
static const uint16_t divisors[] = {
    0,
    6818, 6628, 6449, 6279, 6087, 5906, 5736, 5575,
    5423, 5279, 5120, 4971, 4830, 4697, 4554, 4435,
    4307, 4186, 4058, 3950, 3836, 3728, 3615, 3519,
    3418, 3323, 3224, 3131, 3043, 2960, 2875, 2794,
    2711, 2633, 2560, 2485, 2415, 2348, 2281, 2213,
    2153, 2089, 2032, 1975, 1918, 1864, 1810, 1757,
    1709, 1659, 1612, 1565, 1521, 1478, 1435, 1395,
    1355, 1316, 1280, 1242, 1207, 1173, 1140, 1107,
    1075, 1045, 1015,  986,  959,  931,  905,  879,
     854,  829,  806,  783,  760,  739,  718,  697,
     677,  658,  640,  621,  604,  586,  570,  553,
     538,  522,  507,  493,  479,  465,  452,  439,
     427,  415,  403,  391,  380,  369,  359,  348,
     339,  329,  319,  310,  302,  293,  285,  276,
     269,  261,  253,  246,  239,  232,  226,  219,
     213,  207,  201,  195,  190,  184,  179,
};

// The effect sounding: its tones left, one per 1/140 s.
static const uint8_t *sfx_pos;
static int sfx_left, sfx_channel = -1, sfx_lump = -1;

// The song: MUS data, where it is, ticks to the next event, the notes
// held on each channel, and whether it loops.
static uint8_t *song;
static int song_len, song_start, song_pos, song_wait, song_playing, song_looping, music_volume = 127;
static uint8_t held[16][128];

static int mus_byte(void)
{
    return song_pos < song_len ? song[song_pos++] : -1;
}

// Plays the song's events due this tick.
static void song_tick(void)
{
    if (!song_playing)
        return;
    while (song_wait == 0) {
        int d = mus_byte();
        if (d < 0) { song_playing = 0; return; }
        int channel = d & 15, type = (d >> 4) & 7, n;
        switch (type) {
        case 0:
            held[channel][mus_byte() & 127] = 0;
            break;
        case 1:
            n = mus_byte();
            if (n & 128) {
                int volume = mus_byte() & 127;
                held[channel][n & 127] = volume > 0;
            } else {
                held[channel][n & 127] = 1;
            }
            break;
        case 2: case 3:
            mus_byte();
            break;
        case 4:
            mus_byte();
            mus_byte();
            break;
        case 6:
            memset(held, 0, sizeof held);
            if (!song_looping) { song_playing = 0; return; }
            song_pos = song_start;
            continue;
        default:
            break;
        }
        if (d & 128) {
            int b;
            do {
                b = mus_byte();
                if (b < 0) { song_playing = 0; return; }
                song_wait = song_wait * 128 + (b & 127);
            } while (b & 128);
        }
    }
    song_wait--;
}

// The highest note held, but for the percussion channel, in hertz.
static int song_tone(void)
{
    if (!song_playing || music_volume == 0)
        return 0;
    for (int note = 127; note >= 0; note--)
        for (int c = 0; c < 15; c++)
            if (held[c][note])
                return (int)(440.0 * pow(2.0, (note - 69) / 12.0));
    return 0;
}

static void *sound_thread(void *arg)
{
    (void)arg;
    struct timespec next;
    clock_gettime(CLOCK_MONOTONIC, &next);
    for (;;) {
        next.tv_nsec += 1000000000L / 140;
        if (next.tv_nsec >= 1000000000L) { next.tv_sec++; next.tv_nsec -= 1000000000L; }
        clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &next, NULL);
        pthread_mutex_lock(&sound_lock);
        song_tick();
        int hz;
        if (sfx_left > 0) {
            unsigned t = *sfx_pos++;
            sfx_left--;
            hz = t < sizeof divisors / sizeof *divisors && divisors[t] ? 1193181 / divisors[t] : 0;
        } else {
            hz = song_tone();
        }
        pthread_mutex_unlock(&sound_lock);
        tone(hz);
    }
    return NULL;
}

static boolean sfx_init(boolean use_sfx_prefix)
{
    (void)use_sfx_prefix;
    return piezo >= 0;
}

static void sfx_shutdown(void)
{
    pthread_mutex_lock(&sound_lock);
    sfx_left = 0;
    song_playing = 0;
    pthread_mutex_unlock(&sound_lock);
    tone(0);
}

static int sfx_lump_num(sfxinfo_t *sfx)
{
    char name[9];
    snprintf(name, sizeof name, "dp%s", sfx->name);
    return W_GetNumForName(name);
}

static void sfx_update(void) {}
static void sfx_params(int channel, int vol, int sep) { (void)channel; (void)vol; (void)sep; }

static int sfx_start(sfxinfo_t *sfx, int channel, int vol, int sep)
{
    (void)sep;
    if (vol == 0)
        return -1;
    const uint8_t *lump = W_CacheLumpNum(sfx->lumpnum, PU_STATIC);
    int len = W_LumpLength(sfx->lumpnum);
    int count = lump[2] | lump[3] << 8;
    if (lump[0] || lump[1] || count > len - 4) {
        W_ReleaseLumpNum(sfx->lumpnum);
        return -1;
    }
    pthread_mutex_lock(&sound_lock);
    int old = sfx_lump;
    sfx_pos = lump + 4;
    sfx_left = count;
    sfx_channel = channel;
    sfx_lump = sfx->lumpnum;
    pthread_mutex_unlock(&sound_lock);
    if (old >= 0)
        W_ReleaseLumpNum(old);
    return channel;
}

static void sfx_stop(int channel)
{
    pthread_mutex_lock(&sound_lock);
    if (channel == sfx_channel)
        sfx_left = 0;
    pthread_mutex_unlock(&sound_lock);
}

static boolean sfx_playing(int channel)
{
    pthread_mutex_lock(&sound_lock);
    boolean on = channel == sfx_channel && sfx_left > 0;
    pthread_mutex_unlock(&sound_lock);
    return on;
}

static void sfx_cache(sfxinfo_t *sounds, int n) { (void)sounds; (void)n; }

// Settings i_sound.c binds for SDL's resampling, unused here.
int use_libsamplerate = 0;
float libsamplerate_scale = 0.65f;

static snddevice_t piezo_devices[] = { SNDDEVICE_PCSPEAKER, SNDDEVICE_SB, SNDDEVICE_GENMIDI };

sound_module_t DG_sound_module = {
    piezo_devices, sizeof piezo_devices / sizeof *piezo_devices,
    sfx_init, sfx_shutdown, sfx_lump_num, sfx_update, sfx_params,
    sfx_start, sfx_stop, sfx_playing, sfx_cache,
};

static boolean music_init(void) { return piezo >= 0; }
static void music_shutdown(void) {}

static void music_volume_set(int volume)
{
    pthread_mutex_lock(&sound_lock);
    music_volume = volume;
    pthread_mutex_unlock(&sound_lock);
}

static void music_pause(void) { music_volume_set(0); }
static void music_resume(void) { music_volume_set(127); }

static void *music_register(void *data, int len)
{
    const uint8_t *d = data;
    if (len < 16 || memcmp(d, "MUS\x1a", 4))
        return NULL;
    uint8_t *copy = malloc(len);
    memcpy(copy, d, len);
    return copy;
}

static void music_unregister(void *handle)
{
    pthread_mutex_lock(&sound_lock);
    if (song == handle) { song = NULL; song_playing = 0; }
    pthread_mutex_unlock(&sound_lock);
    free(handle);
}

static void music_play(void *handle, boolean looping)
{
    if (!handle)
        return;
    pthread_mutex_lock(&sound_lock);
    song = handle;
    song_len = song[4] | song[5] << 8;
    song_start = song[6] | song[7] << 8;
    song_len += song_start;
    song_pos = song_start;
    song_wait = 0;
    song_looping = looping;
    memset(held, 0, sizeof held);
    song_playing = 1;
    pthread_mutex_unlock(&sound_lock);
}

static void music_stop(void)
{
    pthread_mutex_lock(&sound_lock);
    song_playing = 0;
    pthread_mutex_unlock(&sound_lock);
}

static boolean music_playing(void)
{
    return song_playing;
}

static void music_poll(void) {}

music_module_t DG_music_module = {
    piezo_devices, sizeof piezo_devices / sizeof *piezo_devices,
    music_init, music_shutdown, music_volume_set, music_pause, music_resume,
    music_register, music_unregister, music_play, music_stop, music_playing, music_poll,
};

// --- doomgeneric ------------------------------------------------------

// A tone left on would sound until something else stops it.
static void silence(void)
{
    if (piezo >= 0) {
        struct input_event ev = { .type = EV_SND, .code = SND_TONE, .value = 0 };
        if (write(piezo, &ev, sizeof ev) < 0) {}
    }
}

static void quit(int sig)
{
    (void)sig;
    silence();
    restore_console();
    _exit(0);
}

// doomgeneric's I_Quit (the menu's Quit, and a long press) runs the exit
// functions and returns: registered before Doom's own, this runs last and
// leaves.
static void leave(void)
{
    exit(0);
}

void DG_Init(void)
{
    init_display();
    init_input();
    open_piezo();
    atexit(silence);
    I_AtExit(leave, false);
    signal(SIGTERM, quit);
    signal(SIGINT, quit);
    pthread_t t;
    if (piezo >= 0)
        pthread_create(&t, NULL, sound_thread, NULL);
}

void DG_SleepMs(uint32_t ms)
{
    usleep(ms * 1000);
}

uint32_t DG_GetTicksMs(void)
{
    return now_ms();
}

void DG_SetWindowTitle(const char *title)
{
    (void)title;
}

int main(int argc, char **argv)
{
    doomgeneric_Create(argc, argv);
    for (;;) {
        poll_input();
        doomgeneric_Tick();
    }
}
