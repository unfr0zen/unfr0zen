#!/bin/sh
# Plays Doom in place of the thermostat's UI, then gives the screen back:
# nestlocal runs this when Doom is chosen on the UNFR0ZEN screen. Doom's
# settings and saves are kept in /media/data/doom.
#
# An nlclient patched with the screen hand-over leaves its pid first in
# keeper.pid: it keeps running (and controlling the heating) and shows
# nothing while the process in screen.owner, Doom, is alive. Any other is
# stopped while Doom plays and started again after.
data=/media/data/doom
run=/var/run/unfr0zen
mkdir -p "$data"
play() {
    cd "$data" && HOME="$data" exec /nestlabs/sbin/doom -iwad /usr/share/doom/doom1.wad > "$data/doom.log" 2>&1
}
read -r keeper _ < "$run/keeper.pid" 2> /dev/null
if [ -n "$keeper" ] && [ "$keeper" = "$(pidof nlclient)" ]; then
    # This shell owns the display while it waits for Doom.
    echo $$ > "$run/screen.owner"
    (play)
    rm -f "$run/screen.owner"
    exit 0
fi
monit unmonitor nlclient > /dev/null 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
    pid=$(pidof nlclient) || break
    if [ "$i" -lt 6 ]; then kill $pid; else kill -9 $pid; fi
    sleep 1
done
(play)
monit monitor nlclient > /dev/null 2>&1
monit start nlclient > /dev/null 2>&1
