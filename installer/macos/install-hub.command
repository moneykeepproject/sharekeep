#!/bin/bash
# Make this Mac the household's hub: it keeps the books and the household's phones and
# computers connect to it over the home Wi-Fi. Copy ShareKeep to Applications first.
# The hub runs in the background as you (not as root); its data stays in
# ~/Library/Application Support/ShareKeep Hub. Asks for your password once (to register it
# with launchd).
set -euo pipefail
HUB="/Applications/ShareKeep.app/Contents/MacOS/sharekeep-hub"
DATA="$HOME/Library/Application Support/ShareKeep Hub"
if [ ! -x "$HUB" ]; then
  echo "Copy ShareKeep to Applications first, then open this again."
  exit 1
fi
if [ ! -f "$DATA/hub.json" ]; then
  "$HUB" setup --data "$DATA" --address auto
fi
sudo "$HUB" install --data "$DATA" --user "$(id -un)"
echo
echo "The hub is running. Open ShareKeep: its Hub tab sets the PIN, the book and the people."
echo "Give this Mac a fixed address on your router (a DHCP reservation) so devices can find it."
read -r -p "Press Return to close." _
