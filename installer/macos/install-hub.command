#!/bin/bash
# Make this Mac the household's hub: it keeps the books and the household's phones and
# computers connect to it over the home Wi-Fi. Copy ShareKeep to Applications first.
# The hub runs in the background as you (not as root); its data stays in
# ~/Library/Application Support/ShareKeep Hub. Asks for your password once, only to place
# launchd's file for the hub and start it: nothing of ShareKeep's runs as root (review H3).
set -euo pipefail
HUB="/Applications/ShareKeep.app/Contents/MacOS/sharekeep-hub"
DATA="$HOME/Library/Application Support/ShareKeep Hub"
LABEL="org.moneykeep.sharekeep-hub"
PLIST="/Library/LaunchDaemons/$LABEL.plist"
if [ ! -x "$HUB" ]; then
  echo "Copy ShareKeep to Applications first, then open this again."
  exit 1
fi
if [ ! -f "$DATA/hub.json" ]; then
  "$HUB" setup --data "$DATA" --address auto
fi
xml() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'; }
HUB_X="$(xml "$HUB")"
DATA_X="$(xml "$DATA")"
LOG_X="$(xml "$DATA/hub.log")"
USER_X="$(xml "$(id -un)")"
# The same file as sk_hub::service::macos_plist (a test compares them).
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
cat > "$TMP" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$HUB_X</string>
    <string>run</string>
    <string>--data</string>
    <string>$DATA_X</string>
    <string>--log</string>
    <string>$LOG_X</string>
  </array>
  <key>UserName</key><string>$USER_X</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
</dict>
</plist>
PLIST
echo "Your password places the hub's launchd file and starts it."
sudo /bin/launchctl bootout "system/$LABEL" 2>/dev/null || true
sudo /usr/bin/install -o root -g wheel -m 644 "$TMP" "$PLIST"
sudo /bin/launchctl bootstrap system "$PLIST"
echo
echo "The hub is running. Open ShareKeep: its Hub tab sets the PIN, the book and the people."
echo "Give this Mac a fixed address on your router (a DHCP reservation) so devices can find it."
read -r -p "Press Return to close." _
