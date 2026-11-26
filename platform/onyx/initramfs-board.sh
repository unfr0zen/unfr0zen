# shellcheck shell=sh
# Onyx display initialization in the shared i.MX6 initramfs.
# shellcheck disable=SC2034 # Used by the calling init script.
product=0xa4a8
# The Onyx stock kernel registers a 320-pixel XRGB8888 LCDIF.
fb=/sys/class/graphics/fb0
if [ -c /dev/fb0 ] && [ "$(cat "$fb/name")" = mxs-lcdif ] &&
    [ "$(cat "$fb/bits_per_pixel")" = 32 ] && [ "$(cat "$fb/stride")" = 1280 ]; then
    zcat /recovery-logo.bgrx.gz >/dev/fb0 || echo "unfr0zen initramfs: cannot draw recovery logo" >/dev/kmsg
fi
