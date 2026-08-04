# mdpaste

Copy rich text anywhere, hit a hotkey, and it pastes as Markdown. [Hammerspoon](https://www.hammerspoon.org/) owns the hotkey; a small Rust binary does the clipboard HTML -> Markdown conversion, writes the Markdown back to the clipboard, and simulates Cmd+V. Tested with tables, code blocks, nested lists, links, bold/italic.

Default hotkey: **Ctrl+Option+V** (Ctrl+Alt+V). Cmd+Option+V is avoided deliberately: Finder claims it for "Move Item Here" and JetBrains IDEs for "Introduce Variable" — the hotkey is global, so claiming it would silently break those apps.

## 1. Install prerequisites (one-time)

Two installs — Hammerspoon as a cask, Rust via rustup:

```sh
brew install --cask hammerspoon
```

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Rust is only needed if you build from source (Option B in section 2) — skip this entirely if you're using a prebuilt release. (`brew install rust` also works, but rustup is the standard way to manage Rust toolchains. Note that `brew install --cask hammerspoon rust` fails — `rust` is a formula, not a cask, so it can't ride along on the cask install.)

**Permissions — Hammerspoon needs two grants:**

1. **Accessibility** — System Settings > Privacy & Security > Accessibility > toggle Hammerspoon on. This is what lets it simulate Cmd+V. If you clicked "Don't Allow" at some point and the hotkey now silently does nothing, toggle Hammerspoon off and back on in that list, then quit and relaunch Hammerspoon.
2. **Automation** — the *first* time you press the hotkey, macOS shows a one-time prompt "Hammerspoon wants to control System Events". Click **Allow**. If you deny it, fix it later at System Settings > Privacy & Security > Automation > Hammerspoon, or run `tccutil reset AppleEvents org.hammerspoon.Hammerspoon` to get re-prompted.

The Rust binary and its `osascript` calls run under Hammerspoon's process identity (responsible-process attribution), so they inherit those grants — nothing else needs its own permission.

## 2. Install the binary

**Option A — prebuilt binary** (no Rust needed; macOS universal, runs on Apple Silicon and Intel):

```sh
# from https://github.com/iamfiscus/mdpaste/releases (adjust version):
curl -L -o /tmp/mdpaste.zip https://github.com/iamfiscus/mdpaste/releases/latest/download/mdpaste-v0.1.0-macos-universal.zip
unzip /tmp/mdpaste.zip -d /tmp/mdpaste-bin
mkdir -p ~/bin
cp /tmp/mdpaste-bin/mdpaste ~/bin/
chmod +x ~/bin/mdpaste
xattr -d com.apple.quarantine ~/bin/mdpaste   # bypass Gatekeeper's "unverified developer" block (binaries downloaded unsigned from the web get this until notarized)
```

The `xattr` line is required once: mdpaste is not signed with an Apple Developer ID, so macOS would otherwise refuse to run it. (If you'd rather not bypass that, use Option B and build it yourself.)

**Option B — build from source** (needs Rust from section 1):

```sh
git clone https://github.com/iamfiscus/mdpaste.git
cd mdpaste
cargo build --release
mkdir -p ~/bin
cp target/release/mdpaste ~/bin/
```

Note: `~/bin` is not on your PATH by default. That's fine — Hammerspoon invokes the binary by absolute path. When running it by hand, use the full path: `~/bin/mdpaste`.

## 3. Wire up the hotkey

1. Launch Hammerspoon once: `open -a Hammerspoon` (or Spotlight). Say yes if it offers to move itself to /Applications. **This first launch is what creates `~/.hammerspoon/`** — it doesn't exist until then.
2. Copy the binding and load it:

   ```sh
   cp hammerspoon/mdpaste.lua ~/.hammerspoon/
   echo 'require("mdpaste")' >> ~/.hammerspoon/init.lua
   ```

   (Or paste the contents of `hammerspoon/mdpaste.lua` into `~/.hammerspoon/init.lua` directly.)
3. Menu bar icon > **Reload Config**.

## 4. Use it

Copy rich text, click into your target, press **Ctrl+Option+V**. The Markdown version of what you copied gets pasted.

## Troubleshooting

- **"mdpaste: clipboard doesn't contain HTML (copy something rich-text first)"** means there's no HTML flavor on the clipboard — plain-text copies don't have one. Copy from a browser, rich-text editor, etc. (If the alert instead says just "nothing to convert (copy rich text first)" with no details, something failed silently enough that even stderr was empty.)
- **`~/bin/mdpaste --dry-run`** does the conversion and copies the Markdown to the clipboard, but skips the simulated paste. Useful for checking what you'll get before pasting.
- **`~/bin/mdpaste --test FILE.html`** converts an HTML file and prints the Markdown to stdout. Works on any OS, so it's the fastest way to iterate on conversion bugs.
- **Permission failures are no longer silent** — if the Automation prompt was denied or Accessibility is off, the binary's error message (captured from stderr) is shown in the Hammerspoon alert, e.g. an AppleScript "not authorized" error pointing at System Events.
- **iTerm2** may ask you to confirm multi-line pastes. Toggle "Confirm paste multiple lines" under Preferences > Advanced if you don't want the prompt.
- **Password fields and other secure-input contexts** silently drop synthetic keystrokes — macOS blocks them by design. The Markdown is still on your clipboard; press Cmd+V manually.

## Known limitations

- Copying a whole web page keeps nav bars, sidebars, and other site chrome — only tag-level junk (scripts, styles, etc.) is stripped for now; there's no readability-style main-content extraction.
- Code fences lose their language hints (```` ```rust ```` becomes a plain fence).
- Heading style may mix setext (`===`/`---` underlines) and ATX (`#`) within one paste.
- Some apps produce minor spacing quirks between inline elements (e.g. Slack timestamps abutting adjacent text).
- Blockquotes may pick up stray empty `>` lines around their paragraphs — an upstream `html2md` whitespace quirk; the quote's content and attribution are correct.

## Cross-platform note

The binary compiles on Linux/Windows, but only `--test` works there as shipped. Clipboard *reading* uses the `arboard` crate and is already cross-platform; the real port points are `set_clipboard_text` (shells out to `pbcopy`), `simulate_paste` (shells out to `osascript`), and the Hammerspoon hotkey binding. On Windows you'd write the Markdown back with the Unicode clipboard format, simulate paste with SendInput (e.g. the `enigo` crate), and use AutoHotkey instead of Hammerspoon.

## License

MIT — see [LICENSE](LICENSE).
