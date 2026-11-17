#!/bin/bash
# Stop this Mac being the household's hub. The books stay in
# ~/Library/Application Support/ShareKeep Hub. Asks for your password once, only to remove
# launchd's file for the hub: nothing of ShareKeep's runs as root.
set -euo pipefail
LABEL="org.moneykeep.sharekeep-hub"
sudo /bin/launchctl bootout "system/$LABEL" 2>/dev/null || true
sudo /bin/rm -f "/Library/LaunchDaemons/$LABEL.plist"
echo "The hub is stopped. Its books are still in ~/Library/Application Support/ShareKeep Hub."
read -r -p "Press Return to close." _
