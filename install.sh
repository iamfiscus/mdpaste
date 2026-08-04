#!/bin/sh
# mdpaste installer: builds the binary, installs it to ~/bin, and wires up
# the Hammerspoon hotkey binding if Hammerspoon has been launched at least
# once (which is what creates ~/.hammerspoon).
set -e

cd "$(dirname "$0")"

echo "==> Building mdpaste (release)"
cargo build --release

echo "==> Installing binary to ~/bin"
mkdir -p "$HOME/bin"
cp target/release/mdpaste "$HOME/bin/"

HSDIR="$HOME/.hammerspoon"

if [ -d "$HSDIR" ]; then
  echo "==> Installing Hammerspoon binding"
  cp hammerspoon/mdpaste.lua "$HSDIR/"

  INIT="$HSDIR/init.lua"
  touch "$INIT"
  if grep -q 'require("mdpaste")' "$INIT"; then
    echo "==> init.lua already requires mdpaste; leaving it alone"
  else
    echo 'require("mdpaste")' >> "$INIT"
    echo '==> Added require("mdpaste") to init.lua'
  fi
else
  echo ""
  echo "NOTE: ~/.hammerspoon does not exist yet — launch Hammerspoon once"
  echo "      (open -a Hammerspoon), then re-run this script to install the"
  echo "      hotkey binding."
fi

echo ""
echo "Done. Next steps:"
echo "  1. Launch Hammerspoon (open -a Hammerspoon) if it isn't running."
echo "  2. Hammerspoon menu bar icon > Reload Config."
echo "  3. Grant permissions when prompted:"
echo "     - Accessibility: System Settings > Privacy & Security > Accessibility"
echo "     - Automation: click Allow on the 'control System Events' prompt the"
echo "       first time you press Ctrl+Option+V."
