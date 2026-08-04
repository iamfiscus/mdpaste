#!/bin/sh
# mdpaste installer (v0.2): installs the binary to ~/bin, signs it so the
# macOS Accessibility grant survives rebuilds, installs and starts the
# `mdpaste daemon` LaunchAgent (the daemon itself listens for Ctrl+Option+V --
# Hammerspoon is no longer required), and optionally wires up the legacy
# Hammerspoon binding if Hammerspoon has been launched at least once (which is
# what creates ~/.hammerspoon) and the binding file is available locally.
#
# Fully self-bootstrapping: it works both from a repo checkout and standalone:
#
#   bash -c "$(curl -fsSL https://raw.githubusercontent.com/iamfiscus/mdpaste/main/install.sh)"
#
# Binary source, in order:
#   1. cargo + repo checkout (Cargo.toml) next to this script -> build from source
#   2. ~/bin/mdpaste already present -> reuse it
#   3. otherwise -> download the universal binary from the latest GitHub release
# The LaunchAgent plist template is read from ./launchd when available;
# standalone runs download it (falling back to the copy embedded below).
#
# Idempotent: safe to re-run. Re-running also reloads the LaunchAgent, which is
# how you pick up a replaced binary.
#
# Env overrides:
#   RELEASE_ASSET  exact release asset name to download instead of the default
#                  (default: mdpaste-<latest tag>-macos-universal.zip, where the
#                  tag is discovered from the /releases/latest redirect)

set -e

LABEL="com.iamfiscus.mdpaste"
PLIST_DEST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"
BIN="$HOME/bin/mdpaste"
APP="$HOME/Applications/mdpaste.app"
RAW_BASE="https://raw.githubusercontent.com/iamfiscus/mdpaste/main"
RELEASES_BASE="https://github.com/iamfiscus/mdpaste/releases"

# --- Where does this script live? -----------------------------------------
# Empty SCRIPT_DIR means "standalone" (piped from curl, run via bash -c, or
# read from stdin): ${BASH_SOURCE:-$0} is then unset, "bash"/"sh", or a
# /dev/fd path, none of which have a usable directory.
SRC="${BASH_SOURCE:-$0}"
case "$SRC" in
  ""|bash|sh|-bash|-sh|/dev/stdin|/dev/fd/*)
    SCRIPT_DIR=""
    ;;
  *)
    SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$SRC")" 2>/dev/null && pwd)" || SCRIPT_DIR=""
    ;;
esac

TMP_DIR=""
cleanup() {
  if [ -n "$TMP_DIR" ]; then
    rm -rf "$TMP_DIR"
  fi
  return 0
}
trap cleanup EXIT

# --- LaunchAgent plist template -------------------------------------------
# launchd does not expand ~ or env vars, so the template stores literal
# __HOME__ tokens; they are filled in with sed when the plist is installed.
if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/launchd/$LABEL.plist" ]; then
  PLIST_TEMPLATE="$SCRIPT_DIR/launchd/$LABEL.plist"
else
  echo "==> No local plist template (standalone run); downloading it"
  TMP_DIR="$(mktemp -d /tmp/mdpaste-install.XXXXXX)"
  PLIST_TEMPLATE="$TMP_DIR/$LABEL.plist"
  if ! curl -fsSL "$RAW_BASE/launchd/$LABEL.plist" -o "$PLIST_TEMPLATE"; then
    # Last resort: the template is small enough to carry inside this script,
    # so a network hiccup (or an unpushed launchd/ dir) can't strand the
    # install. Keep this copy in sync with launchd/$LABEL.plist.
    echo "    (download failed; using the template embedded in this script)"
    cat > "$PLIST_TEMPLATE" <<'PLIST_EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.iamfiscus.mdpaste</string>
    <key>ProgramArguments</key>
    <array>
        <string>__HOME__/Applications/mdpaste.app/Contents/MacOS/mdpaste</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>__HOME__/Library/Logs/mdpaste.log</string>
    <key>StandardErrorPath</key>
    <string>__HOME__/Library/Logs/mdpaste.log</string>
</dict>
</plist>
PLIST_EOF
  fi
fi

# --- Binary ---------------------------------------------------------------
echo "==> Installing binary to ~/bin"
mkdir -p "$HOME/bin"
if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ] && command -v cargo >/dev/null 2>&1; then
  echo "==> Building mdpaste from source (release)"
  (cd "$SCRIPT_DIR" && cargo build --release)
  cp "$SCRIPT_DIR/target/release/mdpaste" "$BIN"
elif [ -f "$BIN" ]; then
  echo "==> using existing ~/bin/mdpaste"
else
  # Download the universal binary attached to the latest GitHub release. The
  # asset name embeds the tag (mdpaste-<tag>-macos-universal.zip), so discover
  # the tag from the /releases/latest redirect rather than hardcoding it.
  [ -n "$TMP_DIR" ] || TMP_DIR="$(mktemp -d /tmp/mdpaste-install.XXXXXX)"
  if [ -n "${RELEASE_ASSET:-}" ]; then
    ASSET="$RELEASE_ASSET"
  else
    TAG="$(curl -fsSI "$RELEASES_BASE/latest" 2>/dev/null \
      | sed -n 's|^[Ll]ocation:.*/tag/\(.*\)$|\1|p' | tr -d '\r' | head -n 1)"
    if [ -z "$TAG" ]; then
      TAG="v0.2.0"
      echo "    (could not discover the latest release tag; assuming $TAG)"
    fi
    ASSET="mdpaste-$TAG-macos-universal.zip"
  fi
  ASSET_URL="$RELEASES_BASE/latest/download/$ASSET"
  echo "==> Downloading $ASSET"
  if ! curl -fsSL "$ASSET_URL" -o "$TMP_DIR/mdpaste.zip"; then
    echo "" >&2
    echo "ERROR: could not download the release binary:" >&2
    echo "  $ASSET_URL" >&2
    echo "Check https://github.com/iamfiscus/mdpaste/releases for the exact" >&2
    echo "asset name, then retry with e.g.:" >&2
    echo "  RELEASE_ASSET=mdpaste-vX.Y.Z-macos-universal.zip bash install.sh" >&2
    exit 1
  fi
  unzip -o -q "$TMP_DIR/mdpaste.zip" -d "$TMP_DIR/dist"
  cp "$TMP_DIR/dist/mdpaste" "$BIN"
  # curl doesn't set the quarantine attribute, but strip it anyway in case the
  # zip was produced by something that did -- otherwise Gatekeeper blocks the
  # unsigned binary.
  xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
fi
chmod +x "$BIN"

# --- App bundle -----------------------------------------------------------
# The daemon runs from ~/Applications/mdpaste.app (an LSUIElement background
# agent bundle) so macOS attributes the identity to a real app: the mdpaste
# icon + name show up in the Accessibility list and Login Items instead of a
# generic binary tile. ~/bin/mdpaste stays installed for hand-run CLI modes.
echo "==> Assembling ~/Applications/mdpaste.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/mdpaste"
chmod +x "$APP/Contents/MacOS/mdpaste"

cat > "$APP/Contents/Info.plist" <<'INFO_EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>mdpaste</string>
    <key>CFBundleIdentifier</key>
    <string>com.iamfiscus.mdpaste</string>
    <key>CFBundleName</key>
    <string>mdpaste</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.2.0</string>
    <key>CFBundleVersion</key>
    <string>0.2.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <!-- Background agent: no Dock icon, no menu bar entry unless added later. -->
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
INFO_EOF

# Icon: repo checkout copy first, then download from main; skip with a warning
# if neither is reachable (bundle works iconless, just looks generic).
if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/assets/icon/mdpaste.icns" ]; then
  cp "$SCRIPT_DIR/assets/icon/mdpaste.icns" "$APP/Contents/Resources/mdpaste.icns"
else
  [ -n "$TMP_DIR" ] || TMP_DIR="$(mktemp -d /tmp/mdpaste-install.XXXXXX)"
  if curl -fsSL "$RAW_BASE/assets/icon/mdpaste.icns" -o "$TMP_DIR/mdpaste.icns"; then
    cp "$TMP_DIR/mdpaste.icns" "$APP/Contents/Resources/mdpaste.icns"
  else
    echo "    (icon download failed; bundle will show a generic icon)"
  fi
fi
if [ -f "$APP/Contents/Resources/mdpaste.icns" ]; then
  /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string mdpaste.icns" \
    "$APP/Contents/Info.plist" 2>/dev/null || true
fi

# --- codesign -------------------------------------------------------------
# macOS TCC (Accessibility) keys its grant to the binary's code-signing
# identity. Every ad-hoc signed (or Developer-ID-less) arm64 binary has a
# cdhash-only designated requirement, so replacing ~/bin/mdpaste silently
# REVOKES the Accessibility grant (verified on macOS 15.5). Signing with a
# persistent self-signed "mdpaste" certificate gives a stable, anchor-based
# designated requirement, so the one-time grant survives rebuilds/reinstalls.
#
# One-time setup for grant stability (optional but recommended):
#   Keychain Access > Certificate Assistant > Create a Certificate...
#   Name: mdpaste, Identity Type: Self Signed Root, Certificate Type: Code Signing
# Without that cert we fall back to ad-hoc signing — fine, but you'll be
# re-prompted for Accessibility after each update. Failures here never abort
# the install; an unsigned binary installs and runs identically, it just needs
# a fresh Accessibility grant.
if command -v codesign >/dev/null 2>&1; then
  if security find-identity -v -p codesigning 2>/dev/null | grep -q '"mdpaste"'; then
    SIGNER="mdpaste"
    echo "==> Signing with persistent self-signed 'mdpaste' certificate"
  else
    SIGNER="-"
    echo "==> No 'mdpaste' code-signing certificate found; ad-hoc signing"
    echo "    (Accessibility grant will need re-approval after each update."
    echo "     Optional: create a self-signed 'mdpaste' Code Signing cert in"
    echo "     Keychain Access to make the grant stable across rebuilds.)"
  fi
  # Sign the CLI copy and the app bundle with the same identity.
  codesign --force --sign "$SIGNER" --identifier "$LABEL" "$BIN" 2>/dev/null \
    || echo "    (codesign failed; continuing — run: codesign --force --sign \"$SIGNER\" --identifier $LABEL \"$BIN\")"
  codesign --force --sign "$SIGNER" "$APP" 2>/dev/null \
    || echo "    (bundle codesign failed; continuing — run: codesign --force --sign \"$SIGNER\" \"$APP\")"
fi

# --- LaunchAgent ----------------------------------------------------------
echo "==> Installing LaunchAgent plist"
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
sed "s|__HOME__|$HOME|g" "$PLIST_TEMPLATE" > "$PLIST_DEST"

echo "==> (Re)loading LaunchAgent $LABEL"
# bootout first so re-running after an edit/binary replacement doesn't hit
# "Bootstrap failed: 5: Input/output error" (already loaded). bootout of an
# unloaded label errors with "No such process" — that's fine, ignore it.
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
if launchctl bootstrap "$DOMAIN" "$PLIST_DEST" 2>/dev/null; then
  launchctl enable "$DOMAIN/$LABEL" 2>/dev/null || true
  echo "==> Daemon started (com.iamfiscus.mdpaste). Log: ~/Library/Logs/mdpaste.log"
else
  # Per research: only bootstrap/bootout are used (stable since Catalina);
  # don't take the whole install down if the load step fails.
  echo ""
  echo "WARNING: launchctl bootstrap failed. The binary is installed; load the"
  echo "daemon manually with:"
  echo "  launchctl bootstrap \"$DOMAIN\" \"$PLIST_DEST\""
  echo "  launchctl enable \"$DOMAIN/$LABEL\""
  echo "Then check: launchctl print \"$DOMAIN/$LABEL\" | head"
fi

# --- Hammerspoon (OPTIONAL alternative trigger) ---------------------------
HSDIR="$HOME/.hammerspoon"

if [ -d "$HSDIR" ] && [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/hammerspoon/mdpaste.lua" ]; then
  echo "==> Hammerspoon detected; installing optional alternative binding"
  cp "$SCRIPT_DIR/hammerspoon/mdpaste.lua" "$HSDIR/"

  INIT="$HSDIR/init.lua"
  touch "$INIT"
  if grep -q 'require("mdpaste")' "$INIT"; then
    echo "==> init.lua already requires mdpaste; leaving it alone"
  else
    echo 'require("mdpaste")' >> "$INIT"
    echo '==> Added require("mdpaste") to init.lua'
    echo "    NOTE: with the daemon installed, both the daemon and Hammerspoon"
    echo "    will answer Ctrl+Option+V — pick one. Comment out this require()"
    echo "    to use the daemon, or unload the LaunchAgent to use Hammerspoon."
  fi
elif [ -d "$HSDIR" ]; then
  echo ""
  echo "NOTE: ~/.hammerspoon found but no local hammerspoon/mdpaste.lua"
  echo "      (standalone run) — skipping the optional binding. Get it from"
  echo "      $RAW_BASE/hammerspoon/mdpaste.lua if you want it."
else
  echo ""
  echo "NOTE: ~/.hammerspoon not found — skipping the optional Hammerspoon"
  echo "      binding. You don't need it: the daemon handles the hotkey."
fi

echo ""
echo "Done. Next steps:"
echo "  1. Grant Accessibility (one time, the ONLY permission the daemon needs):"
echo "     the daemon will prompt; if no prompt appears, open"
echo "     System Settings > Privacy & Security > Accessibility and toggle"
echo "     'mdpaste' on. Clicked 'Don't Allow'? Toggle it on in that list —"
echo "     the entry is named after the binary: 'mdpaste'."
echo "  2. Copy rich text, click into a target app, press Ctrl+Option+V:"
echo "     the Markdown version is pasted at the cursor."
echo "  3. Check the daemon is alive:"
echo "       pgrep -f 'mdpaste daemon'"
echo "       launchctl print \"$DOMAIN/$LABEL\" | head"
echo "       tail ~/Library/Logs/mdpaste.log"
echo ""
echo "  Alternative trigger (optional): Hammerspoon — launch it, Reload Config,"
echo "  and grant it Accessibility + Automation instead. See README.md."
