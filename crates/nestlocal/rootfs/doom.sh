#!/bin/sh
# Plays Doom in place of the thermostat's UI, then gives the screen back:
# nestlocal runs this when Doom is chosen on the UNFR0ZEN screen. Doom's
# settings and saves are kept in /media/data/doom.
data=/media/data/doom
mkdir -p "$data"
monit unmonitor nlclient > /dev/null 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
    pid=$(pidof nlclient) || break
    if [ "$i" -lt 6 ]; then kill $pid; else kill -9 $pid; fi
    sleep 1
done
cd "$data" && HOME="$data" /nestlabs/sbin/doom -iwad /usr/share/doom/doom1.wad > "$data/doom.log" 2>&1
monit monitor nlclient > /dev/null 2>&1
monit start nlclient > /dev/null 2>&1
