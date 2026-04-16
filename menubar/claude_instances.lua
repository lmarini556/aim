-- Claude Instances menu bar + popover for Hammerspoon
-- Drop a require("claude_instances") line into ~/.hammerspoon/init.lua
-- (or symlink this file into ~/.hammerspoon/ and require it by name)

local M = {}

local CIU_HOST = os.getenv("CIU_HOST") or "127.0.0.1"
local CIU_PORT = os.getenv("CIU_PORT") or "7878"
local BASE_URL = os.getenv("CIU_PUBLIC_URL") or ("http://" .. CIU_HOST .. ":" .. CIU_PORT)
local TOKEN_FILE = os.getenv("HOME") .. "/.claude-instances-ui/token"

local function readFileQuick(path)
  local f = io.open(path, "r")
  if not f then return nil end
  local s = f:read("*a")
  f:close()
  return s
end

local function loadToken()
  local raw = readFileQuick(TOKEN_FILE) or ""
  return (raw:gsub("%s+$", ""))
end

local AUTH_TOKEN = loadToken()
local AUTH_HEADERS = AUTH_TOKEN ~= "" and { Authorization = "Bearer " .. AUTH_TOKEN } or nil

-- WKWebView inside hs.webview does not reliably persist cookies across the
-- /auth → / redirect boundary, so we ship the token straight on the index
-- query string. server.py recognises `?t=<token>` on `/` and sets the cookie
-- inline before serving the SPA.
local function buildDashboardUrl()
  local token = loadToken()
  if token ~= "" then
    return BASE_URL .. "/?compact=1&t=" .. token
  end
  return BASE_URL .. "/?compact=1"
end

local DASHBOARD_URL = buildDashboardUrl()
local API_URL = BASE_URL .. "/api/instances"
local POLL_SECONDS = 2
local FRESH_WINDOW = 300
local TRAY_WIDTH = 460
local SLIDE_MS = 180
local SLIDE_FPS = 90
local ACK_FILE = os.getenv("HOME") .. "/.claude-instances-ui/menubar_acks.json"
local HOTKEY = { mods = { "cmd", "shift" }, key = "C" }

local menubar = nil
local webview = nil
local pollTimer = nil
local lastStates = {}
local acks = {}
local hotkey = nil
local clickWatcher = nil
local escHotkey = nil
local slideTimer = nil
local closing = false
local lastServerStart = nil

-- Sets of sids currently driving the menu icon. Detecting NEW entries here is
-- exactly the cue that flips the menu bar glyph to ⚠ / ✦ — whenever that
-- happens for a previously-unseen sid, fire a banner.
local prevFresh = {}
local prevNeeds = {}
local notifyInitialized = false
local pollOnce       -- forward declaration
local renderMenubar  -- forward declaration
local ackSid         -- forward declaration

local function readFile(path)
  local f = io.open(path, "r")
  if not f then return nil end
  local content = f:read("*a")
  f:close()
  return content
end

local function writeFile(path, content)
  local dir = path:match("(.+)/[^/]+$")
  if dir then hs.fs.mkdir(dir) end
  local f = io.open(path, "w")
  if not f then return end
  f:write(content)
  f:close()
end

local function loadAcks()
  local s = readFile(ACK_FILE)
  if not s then return {} end
  local ok, data = pcall(hs.json.decode, s)
  return (ok and data) or {}
end

local function saveAcks()
  writeFile(ACK_FILE, hs.json.encode(acks))
end

local function freshIntensity(inst, now)
  if not inst.alive or inst.status ~= "idle" then return 0 end
  if inst.last_event ~= "Stop" then return 0 end
  local ts = inst.hook_timestamp or 0
  if ts == 0 then return 0 end
  local localAck = acks[inst.session_id] or 0
  local serverAck = inst.ack_timestamp or 0
  if math.max(localAck, serverAck) >= ts then return 0 end
  local age = now - ts
  if age <= 0 then return 1 end
  if age >= FRESH_WINDOW then return 0 end
  return 1 - age / FRESH_WINDOW
end

-- ============================================================
-- Themed banner notifications (hs.canvas) — match card styling
-- ============================================================
local BANNER_W = 380
local BANNER_H = 108
local BANNER_GAP = 10
local BANNER_TTL = 30
local activeBanners = {}

local function reflowBanners()
  local screen = hs.screen.mainScreen()
  if not screen then return end
  local sf = screen:frame()
  for i, c in ipairs(activeBanners) do
    c.canvas:topLeft({
      x = sf.x + sf.w - BANNER_W - 16,
      y = sf.y + 16 + (BANNER_H + BANNER_GAP) * (i - 1),
    })
  end
end

local function dismissBanner(entry)
  if entry._dismissing then return end
  entry._dismissing = true
  for i, e in ipairs(activeBanners) do
    if e == entry then table.remove(activeBanners, i) break end
  end
  if entry.timer then entry.timer:stop() end
  entry.canvas:hide(0.2)
  hs.timer.doAfter(0.24, function()
    if entry.canvas then entry.canvas:delete() end
  end)
  reflowBanners()
end

local function truncCwd(cwd)
  if not cwd or cwd == "" then return "" end
  local home = os.getenv("HOME") or ""
  if home ~= "" and cwd:sub(1, #home) == home then
    cwd = "~" .. cwd:sub(#home + 1)
  end
  return cwd
end

local function themedBanner(kind, info, sid)
  local screen = hs.screen.mainScreen()
  if not screen then return end
  local sf = screen:frame()
  local idx = #activeBanners
  local x = sf.x + sf.w - BANNER_W - 16
  local y = sf.y + 16 + (BANNER_H + BANNER_GAP) * idx

  local title = info.name or (sid and sid:sub(1, 8)) or "Instance"
  local cwd = truncCwd(info.cwd or "")
  local mcpCount = info.mcp_count or 0
  local pid = info.pid
  local toolName = info.last_tool or ""
  local notifMsg = info.notification_message or ""

  -- Palette
  local muted = { red = 140/255, green = 140/255, blue = 140/255, alpha = 1 }
  local mutedStrong = { red = 180/255, green = 180/255, blue = 180/255, alpha = 1 }
  local surface = { red = 14/255, green = 14/255, blue = 14/255, alpha = 0.97 }
  local borderBase = { red = 34/255, green = 34/255, blue = 34/255, alpha = 1 }

  local accentColor, glowColor, statusText, statusGlyph
  if kind == "needs_input" then
    accentColor = { red = 1.0, green = 143/255, blue = 63/255, alpha = 1 }
    glowColor = { red = 1.0, green = 107/255, blue = 26/255, alpha = 0.2 }
    statusGlyph = "!"
    statusText = "NEEDS INPUT"
    if toolName ~= "" then statusText = statusText .. " · " .. toolName end
  else
    accentColor = { red = 34/255, green = 197/255, blue = 94/255, alpha = 1 }
    glowColor = { red = 34/255, green = 197/255, blue = 94/255, alpha = 0.2 }
    statusGlyph = "●"
    statusText = "IDLE · REPLY"
  end

  local borderColor = { red = accentColor.red, green = accentColor.green, blue = accentColor.blue, alpha = 0.5 }

  local canvas = hs.canvas.new({ x = x, y = y, w = BANNER_W, h = BANNER_H })
    :level("floating")
    :behavior({ "canJoinAllSpaces", "stationary" })
    :clickActivating(false)

  -- Outer glow
  canvas[#canvas + 1] = {
    type = "rectangle",
    action = "fill",
    fillColor = glowColor,
    roundedRectRadii = { xRadius = 14, yRadius = 14 },
    frame = { x = 0, y = 0, w = BANNER_W, h = BANNER_H },
  }
  -- Drop shadow
  canvas[#canvas + 1] = {
    type = "rectangle",
    action = "fill",
    fillColor = { red = 0, green = 0, blue = 0, alpha = 0.5 },
    roundedRectRadii = { xRadius = 12, yRadius = 12 },
    frame = { x = 3, y = 5, w = BANNER_W - 6, h = BANNER_H - 6 },
  }
  -- Card background
  canvas[#canvas + 1] = {
    type = "rectangle",
    action = "strokeAndFill",
    fillColor = surface,
    strokeColor = borderColor,
    strokeWidth = 1.5,
    roundedRectRadii = { xRadius = 10, yRadius = 10 },
    frame = { x = 4, y = 4, w = BANNER_W - 8, h = BANNER_H - 8 },
  }
  -- Left accent stripe
  canvas[#canvas + 1] = {
    type = "rectangle",
    action = "fill",
    fillColor = accentColor,
    roundedRectRadii = { xRadius = 2, yRadius = 2 },
    frame = { x = 4, y = 4, w = 4, h = BANNER_H - 8 },
  }

  local textX = 16
  local textW = BANNER_W - textX - 16
  local curY = 12

  -- Status row
  canvas[#canvas + 1] = {
    type = "text",
    text = hs.styledtext.new(statusGlyph .. "  " .. statusText, {
      color = accentColor,
      font = { name = ".SF NS Mono Light Bold", size = 10 },
    }),
    frame = { x = textX, y = curY, w = textW, h = 16 },
  }
  curY = curY + 18

  -- Instance name
  canvas[#canvas + 1] = {
    type = "text",
    text = hs.styledtext.new(title, {
      color = { white = 1, alpha = 1 },
      font = { name = ".SF NS Rounded Bold", size = 14 },
    }),
    frame = { x = textX, y = curY, w = textW, h = 20 },
  }
  curY = curY + 20

  -- CWD
  if cwd ~= "" then
    canvas[#canvas + 1] = {
      type = "text",
      text = hs.styledtext.new(cwd, {
        color = muted,
        font = { name = ".SF NS Mono Light Regular", size = 11 },
      }),
      frame = { x = textX, y = curY, w = textW, h = 16 },
    }
    curY = curY + 18
  end

  -- Notification message (for needs_input)
  if kind == "needs_input" and notifMsg ~= "" then
    local truncMsg = #notifMsg > 60 and notifMsg:sub(1, 57) .. "…" or notifMsg
    canvas[#canvas + 1] = {
      type = "text",
      text = hs.styledtext.new(truncMsg, {
        color = accentColor,
        font = { name = ".AppleSystemUIFont", size = 11 },
      }),
      frame = { x = textX, y = curY, w = textW, h = 16 },
    }
    curY = curY + 18
  end

  -- Tags row
  local tags = {}
  if mcpCount > 0 then table.insert(tags, mcpCount .. " MCP" .. (mcpCount == 1 and "" or "s")) end
  if pid then table.insert(tags, "pid " .. tostring(pid)) end
  if #tags > 0 then
    canvas[#canvas + 1] = {
      type = "text",
      text = hs.styledtext.new(table.concat(tags, "   "), {
        color = mutedStrong,
        font = { name = ".SF NS Mono Light Regular", size = 10 },
      }),
      frame = { x = textX, y = curY, w = textW, h = 14 },
    }
  end

  local entry = { canvas = canvas, sid = sid }
  canvas:mouseCallback(function(_, event)
    if event == "mouseUp" then
      if sid then
        ackSid(sid)
        hs.http.asyncPost(
          BASE_URL .. "/api/open-dashboard",
          hs.json.encode({ sid = sid }),
          (function()
            local h = { ["Content-Type"] = "application/json" }
            if AUTH_TOKEN ~= "" then h["Authorization"] = "Bearer " .. AUTH_TOKEN end
            return h
          end)(),
          function() end
        )
      end
      dismissBanner(entry)
    end
  end)
  canvas:canvasMouseEvents(true, true, false, false)

  canvas:show(0.2)
  entry.timer = hs.timer.doAfter(BANNER_TTL, function() dismissBanner(entry) end)
  table.insert(activeBanners, entry)
end

local lastPollData = nil

ackSid = function(sid)
  if not sid then return end
  local st = lastStates[sid]
  local ts = st and st.hook_timestamp or os.time()
  if (acks[sid] or 0) < ts then
    acks[sid] = ts
    saveAcks()
  end
  -- Immediately re-render menubar with updated acks so the badge clears
  if lastPollData then renderMenubar(lastPollData) end
  -- Push ack to server so the web UI also clears the fresh state
  hs.http.asyncPost(
    BASE_URL .. "/api/instances/" .. sid .. "/ack",
    hs.json.encode({ timestamp = ts }),
    (function()
      local h = { ["Content-Type"] = "application/json" }
      if AUTH_TOKEN ~= "" then h["Authorization"] = "Bearer " .. AUTH_TOKEN end
      return h
    end)(),
    function() pollOnce() end
  )
end

local function playChime(kind)
  local name = (kind == "ready") and "Glass" or "Funk"
  local s = hs.sound.getByName(name)
  if s then s:volume(0.5):play() end
end

local function notify(kind, info, sid)
  playChime(kind)
  themedBanner(kind, info, sid)
end

function M.testNotify()
  notify("ready", { name = "Test instance", cwd = "~/Documents/test", mcp_count = 2, pid = 12345 }, nil)
end

local function fetchInstances(callback)
  hs.http.asyncGet(API_URL, AUTH_HEADERS, function(status, body, _)
    if status ~= 200 or not body then callback(nil) return end
    local ok, data = pcall(hs.json.decode, body)
    callback(ok and data or nil)
  end)
end

-- Fire banners for sids that newly entered the fresh / needs_input buckets.
local function notifyFromSets(curFresh, curNeeds)
  if not notifyInitialized then
    prevFresh = curFresh
    prevNeeds = curNeeds
    notifyInitialized = true
    return
  end
  for sid, info in pairs(curNeeds) do
    if not prevNeeds[sid] then
      notify("needs_input", info, sid)
    end
  end
  for sid, info in pairs(curFresh) do
    if not prevFresh[sid] and not prevNeeds[sid] then
      notify("ready", info, sid)
    end
  end
  prevFresh = curFresh
  prevNeeds = curNeeds
end

renderMenubar = function(data)
  if not data then
    if menubar then
      menubar:setTitle("⚠")
      menubar:setTooltip("Server unreachable at " .. API_URL)
    end
    return
  end
  lastPollData = data

  -- Detect server restart: refresh token + reload webview if open.
  local curStart = data.server_start
  if curStart and lastServerStart and curStart ~= lastServerStart then
    local tok = loadToken()
    if tok ~= "" then
      AUTH_TOKEN = tok
      AUTH_HEADERS = { Authorization = "Bearer " .. tok }
    end
    DASHBOARD_URL = buildDashboardUrl()
    if webview and not closing then
      webview:url(DASHBOARD_URL .. "&_=" .. tostring(os.time()))
    end
    -- no banner for server restart — just refresh silently
  end
  lastServerStart = curStart or lastServerStart

  local now = os.time()
  local needs, fresh, running, idle = 0, 0, 0, 0
  local current = {}
  local freshSet = {}
  local needsSet = {}
  for _, i in ipairs(data.instances or {}) do
    if i.alive then
      local mcpCount = 0
      if i.mcps then
        mcpCount = (i.mcps.global and #i.mcps.global or 0)
                 + (i.mcps.project and #i.mcps.project or 0)
                 + (i.mcps.explicit and #i.mcps.explicit or 0)
      end
      local instInfo = {
        name = i.name,
        cwd = i.cwd,
        pid = i.pid,
        last_tool = i.last_tool,
        mcp_count = mcpCount,
        notification_message = i.notification_message,
      }
      current[i.session_id] = {
        status = i.status,
        hook_timestamp = i.hook_timestamp or 0,
        last_event = i.last_event,
        name = i.name,
        last_tool = i.last_tool,
      }
      if i.status == "needs_input" then
        needs = needs + 1
        needsSet[i.session_id] = instInfo
      elseif freshIntensity(i, now) > 0 then
        fresh = fresh + 1
        freshSet[i.session_id] = instInfo
      elseif i.status == "running" then
        running = running + 1
      else
        idle = idle + 1
      end
    end
  end

  notifyFromSets(freshSet, needsSet)
  lastStates = current

  local glyph = "◉"
  if needs > 0 then glyph = "⚠"
  elseif fresh > 0 then glyph = "✦"
  elseif running > 0 then glyph = "◉"
  end

  local count = 0
  if needs > 0 then count = needs
  elseif fresh > 0 then count = fresh
  end

  local title = count > 0 and (glyph .. " " .. count) or glyph

  local color
  if needs > 0 then
    color = { red = 1.0, green = 0.42, blue = 0.10 } -- vivid orange-red
  elseif fresh > 0 then
    color = { red = 0.18, green = 0.86, blue = 0.46 } -- vivid green
  elseif running > 0 then
    color = { red = 1.0, green = 0.55, blue = 0.18 } -- warm orange
  else
    color = { red = 0.95, green = 0.85, blue = 0.62 } -- warm cream — readable in dark menu bar
  end

  local styled = hs.styledtext.new(title, {
    color = color,
    font = { name = ".SF NS Rounded Bold", size = 15 },
  })
  if menubar then
    menubar:setTitle(styled)
    local bits = {}
    if needs > 0 then table.insert(bits, needs .. " needs input") end
    if fresh > 0 then table.insert(bits, fresh .. " new replies") end
    if running > 0 then table.insert(bits, running .. " running") end
    table.insert(bits, idle .. " idle")
    menubar:setTooltip(table.concat(bits, " · "))
  end
end

local function ackAllOnOpen()
  local now = os.time()
  local changed = false
  for sid, st in pairs(lastStates) do
    if st.status == "idle" and st.hook_timestamp > 0 and (now - st.hook_timestamp) < FRESH_WINDOW then
      if (acks[sid] or 0) < st.hook_timestamp then
        acks[sid] = st.hook_timestamp
        changed = true
      end
    end
  end
  if changed then saveAcks() end
end

local function trayFrame(screen)
  local f = screen:frame() -- excludes menu bar already
  return {
    onScreen = hs.geometry.rect(f.x + f.w - TRAY_WIDTH, f.y, TRAY_WIDTH, f.h),
    offScreen = hs.geometry.rect(f.x + f.w, f.y, TRAY_WIDTH, f.h),
  }
end

local function easeOut(t)
  return 1 - (1 - t) * (1 - t) * (1 - t)
end

local function animateSlide(fromX, toX, y, w, h, onDone)
  if slideTimer then slideTimer:stop(); slideTimer = nil end
  local startTime = hs.timer.absoluteTime() / 1e6
  local duration = SLIDE_MS
  slideTimer = hs.timer.doEvery(1 / SLIDE_FPS, function()
    if not webview then
      if slideTimer then slideTimer:stop(); slideTimer = nil end
      return
    end
    local now = hs.timer.absoluteTime() / 1e6
    local t = math.min(1, (now - startTime) / duration)
    local eased = easeOut(t)
    local curX = fromX + (toX - fromX) * eased
    webview:frame(hs.geometry.rect(curX, y, w, h))
    if t >= 1 then
      slideTimer:stop()
      slideTimer = nil
      if onDone then onDone() end
    end
  end)
end

local function closePopover()
  if not webview or closing then return end
  closing = true
  if clickWatcher then clickWatcher:stop(); clickWatcher = nil end
  if escHotkey then escHotkey:delete(); escHotkey = nil end

  local screen = hs.mouse.getCurrentScreen() or hs.screen.mainScreen()
  local frames = trayFrame(screen)
  local f = webview:frame()
  animateSlide(f.x, frames.offScreen.x, f.y, f.w, f.h, function()
    if webview then webview:delete(); webview = nil end
    closing = false
  end)
end

local function showPopover()
  if webview then
    closePopover()
    return
  end
  closing = false

  -- Re-read token in case server regenerated it since Hammerspoon start
  DASHBOARD_URL = buildDashboardUrl()
  local tok = loadToken()
  if tok ~= "" then
    AUTH_TOKEN = tok
    AUTH_HEADERS = { Authorization = "Bearer " .. tok }
  end

  local screen = hs.mouse.getCurrentScreen() or hs.screen.mainScreen()
  local frames = trayFrame(screen)

  webview = hs.webview.new(frames.offScreen, { developerExtrasEnabled = true })
    :windowStyle({ "borderless", "nonactivating", "utility" })
    :allowGestures(true)
    :allowNewWindows(false)
    :allowTextEntry(true)
    :transparent(true)
    :shadow(false)
    :policyCallback(function(action, _wv, navData, _frame)
      if action == "navigationAction" then
        local req = navData and (navData.request or navData) or {}
        local url = req.URL or req.url or ""
        if type(url) == "table" then url = url.url or url.absoluteString or "" end
        if type(url) == "string" and url:sub(1, 6) == "ciu://" then
          hs.timer.doAfter(0, closePopover)
          return false -- cancel navigation
        end
      end
      return true
    end)
    :level(hs.drawing.windowLevels.floating)
    :url(DASHBOARD_URL .. "&_=" .. tostring(os.time()))
    :show()
    :bringToFront(true)

  animateSlide(frames.offScreen.x, frames.onScreen.x, frames.onScreen.y,
               frames.onScreen.w, frames.onScreen.h)

  ackAllOnOpen()

  clickWatcher = hs.eventtap.new({ hs.eventtap.event.types.leftMouseDown,
                                   hs.eventtap.event.types.rightMouseDown }, function(event)
    if not webview then return false end
    local pt = event:location()
    local f = webview:frame()
    if not f then return false end
    local inside = pt.x >= f.x and pt.x <= (f.x + f.w)
                   and pt.y >= f.y and pt.y <= (f.y + f.h)
    if not inside then
      hs.timer.doAfter(0, closePopover)
    end
    return false
  end)
  clickWatcher:start()

  escHotkey = hs.hotkey.bind({}, "escape", closePopover)
end

pollOnce = function()
  fetchInstances(renderMenubar)
end

function M.start()
  hs.dockicon.hide()
  acks = loadAcks()
  menubar = hs.menubar.new()
  if menubar then
    menubar:setTitle(hs.styledtext.new("◉", {
      color = { red = 0.95, green = 0.85, blue = 0.62 },
      font = { name = ".SF NS Rounded Bold", size = 15 },
    }))
    menubar:setClickCallback(showPopover)
  end
  pollOnce()
  pollTimer = hs.timer.doEvery(POLL_SECONDS, pollOnce)
  hotkey = hs.hotkey.bind(HOTKEY.mods, HOTKEY.key, showPopover)
end

function M.stop()
  if pollTimer then pollTimer:stop(); pollTimer = nil end
  if menubar then menubar:delete(); menubar = nil end
  closePopover()
  if hotkey then hotkey:delete(); hotkey = nil end
  for _, entry in ipairs(activeBanners) do
    if entry.timer then entry.timer:stop() end
    if entry.canvas then entry.canvas:delete() end
  end
  activeBanners = {}
end

M.start()
return M
