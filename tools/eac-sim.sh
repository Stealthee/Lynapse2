#!/bin/sh
# Simulate the EAC kernel for the BlackShark V3 Pro without rebooting.
#   sudo tools/eac-sim.sh on    unload razerkraken, headset falls back to hid-generic (= EAC kernel)
#   sudo tools/eac-sim.sh off   load razerkraken again (normal)
# While 'on', razerkraken is blocked from loading (also on replug) until 'off'.
set -e
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
ids=$(ls /sys/bus/hid/devices | grep -iE '^0003:1532:057[67]\.' || true)
case "$1" in
on)
    # block autoload too: a dongle re-enumeration would otherwise modprobe razerkraken again
    echo "install razerkraken /bin/false" > /etc/modprobe.d/zz-eac-sim.conf
    modprobe -r razerkraken
    for id in $ids; do
        [ -e /sys/bus/hid/devices/$id/driver ] || echo "$id" > /sys/bus/hid/drivers_probe
    done
    ;;
off)
    rm -f /etc/modprobe.d/zz-eac-sim.conf
    modprobe razerkraken
    sleep 1
    for id in $ids; do
        drv=$(basename "$(readlink /sys/bus/hid/devices/$id/driver 2>/dev/null)" 2>/dev/null || true)
        if grep -q 'input5$' /sys/bus/hid/devices/$id/uevent 2>/dev/null && [ "$drv" != razerkraken ]; then
            [ -n "$drv" ] && echo "$id" > /sys/bus/hid/drivers/$drv/unbind
            echo "$id" > /sys/bus/hid/drivers_probe
        fi
    done
    ;;
*) echo "usage: $0 on|off"; exit 1 ;;
esac
for id in $ids; do
    echo "$id -> $(basename "$(readlink /sys/bus/hid/devices/$id/driver 2>/dev/null)" 2>/dev/null || echo none) $(ls /sys/bus/hid/devices/$id/hidraw 2>/dev/null)"
done
