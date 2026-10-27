#!/bin/sh
# Plays Doom in place of the thermostat's UI, then gives the screen back:
# nestlocal runs this when Doom is chosen on the UNFR0ZEN screen. Doom's
# settings and saves are kept in /media/data/doom.
data=/media/data/doom
mkdir -p "$data"
# client-ng, run in place of nlclient (and named nlclient), stands
# aside by itself: only stock's own binary is stopped.
stock_pids() {
    for pid in $(pidof nlclient); do
        [ "$(readlink /proc/$pid/exe)" = /nestlabs/sbin/nlclient ] && echo $pid
    done
}
stock=$(stock_pids)
[ -n "$stock" ] && monit unmonitor nlclient > /dev/null 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
    pid=$(stock_pids)
    [ -n "$pid" ] || break
    if [ "$i" -lt 6 ]; then kill $pid; else kill -9 $pid; fi
    sleep 1
done
cd "$data" && HOME="$data" /nestlabs/sbin/doom -iwad /usr/share/doom/doom1.wad > "$data/doom.log" 2>&1
if [ -n "$stock" ]; then
    monit monitor nlclient > /dev/null 2>&1
    monit start nlclient > /dev/null 2>&1
fi
