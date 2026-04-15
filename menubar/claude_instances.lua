-- Claude Instances menu bar + popover for Hammerspoon
-- Drop a require("claude_instances") line into ~/.hammerspoon/init.lua
-- (or symlink this file into ~/.hammerspoon/ and require it by name)

local M = {}

local CIU_HOST = os.getenv("CIU_HOST") or "127.0.0.1"
local CIU_PORT = os.getenv("CIU_PORT") or "7878"
local BASE_URL = os.getenv("CIU_PUBLIC_URL") or ("http://" .. CIU_HOST .. ":" .. CIU_PORT)
local DASHBOARD_URL = BASE_URL .. "/?compact=1"
local API_URL = BASE_URL .. "/api/instances"
local POLL_SECONDS = 4
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
  if (acks[inst.session_id] or 0) >= ts then return 0 end
  local age = now - ts
  if age <= 0 then return 1 end
  if age >= FRESH_WINDOW then return 0 end
  return 1 - age / FRESH_WINDOW
end

local function notify(title, body)
  hs.notify.new({
    title = title,
    informativeText = body,
    soundName = hs.notify.defaultNotificationSound,
  }):send()
end

local function fetchInstances(callback)
  hs.http.asyncGet(API_URL, nil, function(status, body, _)
    if status ~= 200 or not body then callback(nil) return end
    local ok, data = pcall(hs.json.decode, body)
    callback(ok and data or nil)
  end)
end

local function diffAndNotify(prev, current)
  for sid, cur in pairs(current) do
    local p = prev[sid]
    if p and p.status ~= cur.status then
      if cur.status == "needs_input" then
        local tool = cur.last_tool or ""
        local msg = "Approval needed"
        if tool ~= "" then msg = msg .. " for " .. tool end
        notify(cur.name, msg)
      elseif p.status == "running" and cur.status == "idle" then
        notify(cur.name, "Reply ready")
      end
    end
  end
end

local function renderMenubar(data)
  if not data then
    if menubar then
      menubar:setTitle("⚠")
      menubar:setTooltip("Server unreachable at " .. API_URL)
    end
    return
  end

  local now = os.time()
  local needs, fresh, running, idle = 0, 0, 0, 0
  local current = {}
  for _, i in ipairs(data.instances or {}) do
    if i.alive then
      current[i.session_id] = {
        status = i.status,
        hook_timestamp = i.hook_timestamp or 0,
        name = i.name,
        last_tool = i.last_tool,
      }
      if i.status == "needs_input" then
        needs = needs + 1
      elseif freshIntensity(i, now) > 0 then
        fresh = fresh + 1
      elseif i.status == "running" then
        running = running + 1
      else
        idle = idle + 1
      end
    end
  end

  diffAndNotify(lastStates, current)
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
    font = { name = "SF Pro Rounded Bold", size = 15 },
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
    :url(DASHBOARD_URL)
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

local function pollOnce()
  fetchInstances(renderMenubar)
end

function M.start()
  hs.dockicon.hide()
  acks = loadAcks()
  menubar = hs.menubar.new()
  if menubar then
    menubar:setTitle(hs.styledtext.new("◉", {
      color = { red = 0.95, green = 0.85, blue = 0.62 },
      font = { name = "SF Pro Rounded Bold", size = 15 },
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
end

M.start()
return M
