#!/bin/sh
# mdpaste uninstaller: stops and removes the LaunchAgent, deletes the plist
# and the ~/bin/mdpaste binary. Safe to run when partially (or never)
# installed — every step tolerates "not present".
#
# Not touched automatically: the Accessibility grant for 'mdpaste' (a stale
# entry may remain in System Settings > Privacy & Security > Accessibility —
# harmless) and the optional ~/.hammerspoon wiring (see the note at the end).

set -e

LABEL="com.iamfiscus.mdpaste"
PLIST_DEST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"
BIN="$HOME/bin/mdpaste"

echo "==> Unloading LaunchAgent $LABEL"
# bootout of an unloaded label fails with "Boot-out failed: 3: No such
# process" — expected on a partial install, so ignore errors.
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true

if [ -f "$PLIST_DEST" ]; then
  rm "$PLIST_DEST"
  echo "==> Removed $PLIST_DEST"
else
  echo "==> No plist at $PLIST_DEST (already removed)"
fi

if [ -f "$BIN" ]; then
  rm "$BIN"
  echo "==> Removed $BIN"
else
  echo "==> No binary at $BIN (already removed)"
fi

APP="$HOME/Applications/mdpaste.app"
if [ -d "$APP" ]; then
  rm -rf "$APP"
  echo "==> Removed $APP"
else
  echo "==> No app bundle at $APP (already removed)"
fi

echo ""
echo "Done. Optional leftovers you may want to clean by hand:"
echo "  - Hammerspoon binding (only if you installed it): remove the line"
echo "    require(\"mdpaste\") from ~/.hammerspoon/init.lua and delete"
echo "    ~/.hammerspoon/mdpaste.lua, then Reload Config in Hammerspoon."
echo "  - Log file: ~/Library/Logs/mdpaste.log"
echo "  - Stale 'mdpaste' entry in System Settings > Privacy & Security >"
echo "    Accessibility (macOS keeps it; harmless)."
