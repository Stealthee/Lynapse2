#!/bin/sh
# Capture USB bus traffic (usbmon) for the BlackShark dongle while razerctl --hid queries it.
# Usage: sudo tools/usbcap.sh [out]   (default ~/usbmon-blackshark.log of the user running sudo)
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
U=${SUDO_USER:?run with sudo from your normal user}
H=$(getent passwd "$U" | cut -d: -f6)
HERE=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$H/usbmon-blackshark.log}
bus=$(basename "$(dirname "$(readlink -f /sys/bus/usb/devices/*/idProduct | xargs grep -l 0577 | head -1)")" | cut -d- -f1)
[ -n "$bus" ] || { echo "dongle not found"; exit 1; }
modprobe usbmon
mount | grep -q debugfs || mount -t debugfs none /sys/kernel/debug
timeout 20 cat /sys/kernel/debug/usb/usbmon/${bus}u > "$out" &
sleep 1
sudo -u "$U" "$HERE/src-tauri/target/release/razerctl" --hid
wait
chown "$U": "$out"
echo "bus $bus: $(wc -l < "$out") lines -> $out"
