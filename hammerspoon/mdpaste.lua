-- mdpaste hotkey binding.
--
-- Usage: copy this file to ~/.hammerspoon/mdpaste.lua and add
--   require("mdpaste")
-- to ~/.hammerspoon/init.lua (or paste this file's contents into init.lua
-- directly). Then reload Hammerspoon (menu bar icon > Reload Config).

-- Point this at wherever you put the built binary.
local mdpaste = os.getenv("HOME") .. "/bin/mdpaste"

-- Default hotkey: Ctrl+Option+V. Cmd+Option+V is avoided on purpose --
-- Finder claims it for "Move Item Here" and JetBrains IDEs for
-- "Introduce Variable", and a Hammerspoon binding is global, so claiming
-- it would silently break those apps.
--
-- The handler runs on key RELEASE (the nil pressed-callback), so your
-- physical modifier keys are back up when the simulated Cmd+V fires --
-- otherwise a still-held Ctrl/Option would turn it into something other
-- than a plain paste.
--
-- Alternative binding if Ctrl+Option+V clashes for you:
--   hs.hotkey.bind({"cmd", "alt"}, "v", nil, function() ... end)
hs.hotkey.bind({"ctrl", "alt"}, "v", nil, function()
  -- Capture stderr too, so failure alerts show the real error instead of
  -- a generic guess.
  local out, status = hs.execute(mdpaste .. " 2>&1")
  if not status then
    local msg = (out or ""):gsub("%s+$", "")
    -- "doesn't contain HTML" = plain-text copy, screenshot, etc. Not an
    -- error worth alerting on — stay silent. Alert only on real failures
    -- (permissions, binary crash).
    if msg ~= "" and not msg:find("doesn't contain HTML", 1, true) then
      hs.alert.show("mdpaste: " .. msg)
    end
  end
end)
