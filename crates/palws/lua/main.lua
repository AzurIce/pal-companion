-- Palws: sync PalBox contents to the companion web app over WebSocket.
--
-- Production v1 contract:
--   * no automatic sync (no UI/Widget/map-load/object-creation triggers)
--   * single explicit entry point `requestSnapshot` shared by F7 and web refresh
--   * every state-machine step re-fetches PlayerState; no UObject held across timers
--   * Lua -> Rust goes through `palws.broadcast(json)` only (no file transport)
--
-- Crash hardening: every callback is wrapped in xpcall(debug.traceback); errors are
-- logged, never propagated into UE4SS's C++ dispatch.

local READ_PASSIVES = true
local READ_GENDER   = true
local READ_NICKNAME = true
local MAX_DUMP_PALS = 960
local PALBOX_PAGE_COUNT = 32
local PALBOX_PAGE_REQUEST_DELAY_MS = 200
local PALBOX_REPLICATION_SETTLE_MS = 3000
local SYNC_COOLDOWN_SEC = 15
local COMMAND_PUMP_INTERVAL_MS = 250

print("[Palws] mod loading\n")

-- ---------- callback guard ----------
local function guarded(name, fn)
    return function(...)
        local tb = (type(debug) == "table" and debug.traceback) and debug.traceback or function(e) return tostring(e) end
        local ok, err = xpcall(fn, tb, ...)
        if not ok then
            print("[Palws] ERROR in " .. name .. ": " .. tostring(err) .. "\n")
        end
    end
end

-- ---------- native module ----------
local palws = nil
do
    local ok, res = pcall(require, "palws")
    print("[Palws] require 'palws': ok=" .. tostring(ok) .. " res=" .. tostring(res) .. "\n")
    if ok and type(res) == "table" then palws = res end
end
if not palws then
    print("[Palws] FATAL: palws native module unavailable, mod disabled\n")
    return
end

local okStart, startRes = pcall(palws.start_server, 32123)
print("[Palws] start_server: ok=" .. tostring(okStart) .. " -> " .. tostring(startRes) .. "\n")

-- Session boundary: after a UE4SS reload the Rust side may still hold a stale
-- pending command from the previous Lua state. Drop it, reset sync state/cooldown,
-- and keep the cached snapshot.
if type(palws.begin_session) == "function" then
    pcall(palws.begin_session)
end

-- ---------- helpers ----------
local function isValid(obj)
    if obj == nil then return false end
    local ok, valid = pcall(function() return obj:IsValid() end)
    return ok and valid == true
end

local function className(obj)
    if not isValid(obj) then return nil end
    local ok, n = pcall(function() return obj:GetClass():GetFName():ToString() end)
    if ok then return n end
    return nil
end

local function jsonEscape(s)
    s = tostring(s)
    s = s:gsub('\\', '\\\\'):gsub('"', '\\"'):gsub('\n', '\\n'):gsub('\r', '\\r')
    return s
end

local function jsonStr(v)
    if v == nil then return "null" end
    return '"' .. jsonEscape(v) .. '"'
end

-- extract a string field from a small JSON object (used for Rust command envelopes)
local function jsonField(json, key)
    return json:match('"' .. key .. '"%s*:%s*"([^"]*)"')
end

-- ---------- protocol event helpers ----------
-- Rust stamps the server-side `seq` and `timestamp_ms`; Lua provides the
-- envelope header (protocol/version/type/id/request_id) and the payload.
local eventSeq = 0

local function emitEvent(eventType, requestId, payloadJson)
    if not palws or type(palws.broadcast) ~= "function" then return nil end
    eventSeq = eventSeq + 1
    local parts = {}
    parts[#parts + 1] = '"protocol":"palws"'
    parts[#parts + 1] = '"version":1'
    parts[#parts + 1] = '"type":' .. jsonStr(eventType)
    parts[#parts + 1] = '"id":' .. jsonStr("lua-" .. eventSeq)
    if requestId ~= nil then
        parts[#parts + 1] = '"request_id":' .. jsonStr(requestId)
    end
    parts[#parts + 1] = '"payload":' .. (payloadJson or "{}")
    local json = "{" .. table.concat(parts, ",") .. "}"
    local ok, res = pcall(palws.broadcast, json)
    if not ok then
        print("[Palws] broadcast error: " .. tostring(res) .. "\n")
    end
    return json
end

local function emitStatus(phase, requestId, requestedPages, totalPages, trigger)
    local payload = '{'
        .. '"phase":' .. jsonStr(phase)
        .. ',"requested_pages":' .. tostring(requestedPages or 0)
        .. ',"total_pages":' .. tostring(totalPages or 0)
        .. ',"trigger":' .. jsonStr(trigger or "")
        .. '}'
    return emitEvent("sync.status", requestId, payload)
end

local function emitLog(level, message, requestId)
    local payload = '{'
        .. '"level":' .. jsonStr(level)
        .. ',"source":"lua"'
        .. ',"message":' .. jsonStr(message)
        .. '}'
    return emitEvent("log", requestId, payload)
end

local function emitError(code, message, requestId, retryable)
    local payload = '{'
        .. '"code":' .. jsonStr(code)
        .. ',"message":' .. jsonStr(message)
        .. ',"retryable":' .. tostring(retryable == true)
        .. '}'
    return emitEvent("error", requestId, payload)
end

local function emitSnapshot(requestId, palsJson, total, requestedPages, requestErrors, containers)
    local payload = '{'
        .. '"mode":"replace"'
        .. ',"pals":[' .. table.concat(palsJson, ",") .. ']'
        .. ',"stats":{"total":' .. tostring(total)
        .. ',"requested_pages":' .. tostring(requestedPages)
        .. ',"request_errors":' .. tostring(requestErrors)
        .. ',"containers":' .. tostring(containers)
        .. '}'
        .. '}'
    return emitEvent("snapshot", requestId, payload)
end

local function tryCall(obj, fname)
    local ok, a = pcall(function() return obj[fname](obj) end)
    if ok then return a end
    return nil
end

local function tryProp(obj, pname)
    local ok, v = pcall(function() return obj[pname] end)
    if ok then return v end
    return nil
end

-- ---------- reflection whitelist (never touch nonexistent members) ----------
local classCache = {}
local function buildClassCache(obj)
    local cn = className(obj)
    if cn == nil then return nil end
    if classCache[cn] then return classCache[cn] end
    local entry = { props = {}, ptypes = {}, funcs = {} }
    local okCls, cls = pcall(function() return obj:GetClass() end)
    if okCls and cls then
        local hops = 0
        while cls and hops < 32 do
            hops = hops + 1
            pcall(function()
                cls:ForEachProperty(function(prop)
                    local okN, name = pcall(function() return prop:GetFName():ToString() end)
                    if okN and name then
                        entry.props[name] = true
                        local okF, full = pcall(function() return prop:GetFullName() end)
                        if okF and full then entry.ptypes[name] = full:match("^(%w+)") end
                    end
                    return false
                end)
            end)
            pcall(function()
                cls:ForEachFunction(function(fn)
                    local okN, name = pcall(function() return fn:GetFName():ToString() end)
                    if okN and name then entry.funcs[name] = true end
                    return false
                end)
            end)
            local okS, sup = pcall(function() return cls:GetSuperStruct() end)
            if okS and sup then
                local okV, valid = pcall(function() return sup:IsValid() end)
                cls = (okV and valid) and sup or nil
            else
                cls = nil
            end
        end
    end
    classCache[cn] = entry
    return entry
end

local structCache = {}
local SAVEPARAM_STRUCT = "/Script/Pal.PalIndividualCharacterSaveParameter"
local function buildStructCache(structPath)
    if structCache[structPath] then return structCache[structPath] end
    local entry = { set = {}, types = {} }
    local ok, ss = pcall(function() return StaticFindObject(structPath) end)
    if ok and isValid(ss) then
        pcall(function()
            ss:ForEachProperty(function(prop)
                local okN, name = pcall(function() return prop:GetFName():ToString() end)
                if okN and name then
                    entry.set[name] = true
                    local okF, full = pcall(function() return prop:GetFullName() end)
                    if okF and full then entry.types[name] = full:match("^(%w+)") end
                end
                return false
            end)
        end)
    end
    structCache[structPath] = entry
    return entry
end

local function isShell(v)
    if type(v) ~= "userdata" then return false end
    if isValid(v) then return false end
    local ok, s = pcall(tostring, v)
    if not ok or s == nil then return true end
    return s:find("^UObject:") ~= nil or s:find("^UFunction:") ~= nil
        or s:find("^Property") ~= nil
end

local function validShape(v, expect)
    if v == nil then return false end
    if expect == "string" then return type(v) == "string" end
    if expect == "number" then return type(v) == "number" end
    if expect == "object" then return type(v) == "userdata" and isValid(v) end
    if expect == "struct" then return type(v) == "userdata" and isValid(v) end
    if expect == "fname" then
        if type(v) == "string" then return true end
        if type(v) == "userdata" then
            if isShell(v) then return false end
            local ok, s = pcall(function() return v:ToString() end)
            return ok and s ~= nil
        end
        return false
    end
    return true
end

local function safeCall(obj, name)
    return tryCall(obj, name)
end

local function safeProp(obj, name, expect)
    local v = tryProp(obj, name)
    if not validShape(v, expect or "any") then return nil end
    return v
end

local function safeStructProp(structWrapper, name, expect)
    local v = tryProp(structWrapper, name)
    if not validShape(v, expect or "any") then return nil end
    return v
end

local function fnameToString(v)
    if v == nil then return nil end
    if type(v) == "string" then return v end
    local ok, s = pcall(function() return v:ToString() end)
    if ok then return s end
    local ok2, s2 = pcall(tostring, v)
    if ok2 then return s2 end
    return nil
end

local function unwrap(v)
    local cur = v
    for _ = 1, 4 do
        if type(cur) ~= "userdata" then return cur end
        local okG, g = pcall(function() return cur.get end)
        if not okG or type(g) ~= "function" then return cur end
        local ok, inner = pcall(function() return cur:get() end)
        if not (ok and inner ~= nil) then return cur end
        cur = inner
    end
    return cur
end

local function looksLikeObjectDump(s)
    return s ~= nil and (s:find("^UObject:") or s:find("^UFunction:") or s:find("^Property")
        or s:find("^RemoteUnrealParam:"))
end

-- ---------- field readers ----------
local function mapGenderNumber(n)
    n = math.floor(n)
    if n == 1 then return "male" end
    if n == 2 then return "female" end
    return "unknown"
end

local function mapGenderString(s)
    if s == nil then return "unknown" end
    if s:find("Female") then return "female" end
    if s:find("Male") then return "male" end
    return "unknown"
end

local function readSpecies(param)
    local v = safeCall(param, "GetCharacterID")
    if v == nil then v = safeProp(param, "CharacterID", "fname") end
    local s = fnameToString(v)
    if looksLikeObjectDump(s) then return nil end
    return s
end

local function readNickname(param)
    if not READ_NICKNAME then return nil end
    local function s2str(v)
        if v == nil then return nil end
        if type(v) == "string" then return v end
        local s = fnameToString(unwrap(v))
        if s and s ~= "" and not looksLikeObjectDump(s) then return s end
        return nil
    end
    local sp = safeProp(param, "SaveParameter", "struct")
    if sp ~= nil then
        local tb = (type(debug) == "table" and debug.traceback) and debug.traceback
            or function(e) return tostring(e) end
        local ok, nn = xpcall(function() return safeStructProp(sp, "NickName", "any") end, tb)
        if ok then
            local s = s2str(nn)
            if s then return s end
        end
        local ok2, fn = xpcall(function() return safeStructProp(sp, "FilteredNickName", "any") end, tb)
        if ok2 then
            local s = s2str(fn)
            if s then return s end
        end
    end
    local v = safeCall(param, "GetNickname")
    local s = s2str(v)
    if s then return s end
    return nil
end

local function readGender(param)
    if not READ_GENDER then return "unknown" end
    local v = safeCall(param, "GetGenderType") or safeCall(param, "GetGender")
    if type(v) == "number" then return mapGenderNumber(v) end
    if type(v) == "string" then return mapGenderString(v) end
    if type(v) == "userdata" then
        local s = fnameToString(v)
        if s and not looksLikeObjectDump(s) then return mapGenderString(s) end
    end
    return "unknown"
end

local function readFavorite(param)
    local sp = safeProp(param, "SaveParameter", "struct")
    if sp == nil then return 0 end
    local ok, v = pcall(function() return sp.FavoriteIndex end)
    if ok and type(v) == "number" and v >= 0 and v <= 3 then return math.floor(v) end
    return 0
end

local function readLucky(param)
    local sp = safeProp(param, "SaveParameter", "struct")
    if sp == nil then return false end
    local ok, v = pcall(function() return sp.IsRarePal end)
    if ok and type(v) == "boolean" then return v end
    return false
end

local function readLevel(param)
    local v = safeCall(param, "GetLevel")
    if type(v) == "number" then return math.floor(v) end
    v = safeProp(param, "Level", "number")
    if type(v) == "number" then return math.floor(v) end
    return nil
end

local function readPassives(param)
    if not READ_PASSIVES then return nil end
    local list = safeCall(param, "GetPassiveSkillList")
    if list == nil then
        local sp = safeProp(param, "SaveParameter", "struct")
        if sp ~= nil then
            local ok, l2 = xpcall(function() return safeStructProp(sp, "PassiveSkillList", "any") end,
                function(e) return tostring(e) end)
            if ok then list = l2 end
        end
    end
    if list == nil then return nil end
    local okN, n = pcall(function() return #list end)
    if not okN or type(n) ~= "number" or n <= 0 then return nil end
    local out = {}
    for i = 1, math.min(n, 16) do
        local okE, e = pcall(function() return list[i] end)
        if okE and e ~= nil then
            local s = fnameToString(e)
            if (s == nil or looksLikeObjectDump(s)) and type(e) == "userdata" then
                s = fnameToString(unwrap(e))
            end
            if s and s ~= "" and s ~= "None" and not looksLikeObjectDump(s) then
                out[#out + 1] = s
            end
        end
    end
    return out
end

local function readField(name, verbose, fn)
    if verbose then print("[Palws]   field " .. name .. " invoking" .. "\n") end
    local tb = (type(debug) == "table" and debug.traceback) and debug.traceback
        or function(e) return tostring(e) end
    local ok, v = xpcall(fn, tb)
    if verbose then
        local vs = type(v) == "table" and ("table(" .. #v .. ")") or tostring(v)
        print("[Palws]   field " .. name .. " -> " .. (ok and vs or ("ERR " .. tostring(v))) .. "\n")
    end
    if ok then return v end
    return nil
end

local function buildPalJson(param, idx, verbose)
    if not isValid(param) then return nil end
    if verbose then print("[Palws] step: slot " .. idx .. " read fields\n") end
    local species  = readField("species", verbose, function() return readSpecies(param) end)
    if species == nil or species == "" then return nil end
    local gender   = readField("gender", verbose, function() return readGender(param) end)
    local nickname = readField("nickname", verbose, function() return readNickname(param) end)
    local level    = readField("level", verbose, function() return readLevel(param) end)
    local passives = readField("passives", verbose, function() return readPassives(param) end)
    local favorite = readField("favorite", verbose, function() return readFavorite(param) end)
    local lucky    = readField("lucky", verbose, function() return readLucky(param) end)

    local parts = {}
    parts[#parts + 1] = '"species":' .. jsonStr(species)
    parts[#parts + 1] = '"gender":' .. jsonStr(gender or "unknown")
    local ps = {}
    if passives then
        for _, p in ipairs(passives) do ps[#ps + 1] = jsonStr(p) end
    end
    parts[#parts + 1] = '"passives":[' .. table.concat(ps, ",") .. "]"
    parts[#parts + 1] = '"nickname":' .. jsonStr(nickname)
    parts[#parts + 1] = '"level":' .. (level and tostring(level) or "null")
    parts[#parts + 1] = '"favorite":' .. tostring(favorite or 0)
    parts[#parts + 1] = '"lucky":' .. tostring(lucky == true)
    return "{" .. table.concat(parts, ",") .. "}"
end

local function slotParam(slot)
    if not isValid(slot) then return nil end
    local empty = tryCall(slot, "IsEmpty")
    if empty == true then return nil end
    local okH, handle = pcall(function() return slot:GetHandle() end)
    if okH and isValid(handle) then
        local okP, param = pcall(function() return handle:TryGetIndividualParameter() end)
        if okP and isValid(param) then return param end
    end
    local okH2, h2 = pcall(function() return slot:GetLastHandleForClient() end)
    if okH2 and isValid(h2) then
        local okP2, p2 = pcall(function() return h2:TryGetIndividualParameter() end)
        if okP2 and isValid(p2) then return p2 end
    end
    local p3 = tryProp(slot, "ReplicateIndividualParameter")
    if isValid(p3) then return p3 end
    local h4 = tryProp(slot, "Handle")
    if isValid(h4) then
        local okP4, p4 = pcall(function() return h4:TryGetIndividualParameter() end)
        if okP4 and isValid(p4) then return p4 end
    end
    return nil
end

-- ---------- containers ----------
local function getContainers()
    local ok, t = pcall(function() return FindAllOf("PalIndividualCharacterContainer") end)
    if ok and type(t) == "table" then return t end
    return {}
end

local baseCampSeq = 0

local function dumpContainerPals(container, cidx)
    local num = tryCall(container, "Num")
    local slots = tryCall(container, "GetSlots")
    local n = nil
    if slots ~= nil then
        local okN, cnt = pcall(function() return #slots end)
        if okN then n = cnt end
    end
    if n == nil and type(num) == "number" then n = math.floor(num) end
    if n == nil then return {} end
    local group = "base"
    if n == 5 then group = "party" elseif n == 960 then group = "box" end
    local basecamp = nil
    if group == "base" then
        baseCampSeq = baseCampSeq + 1
        basecamp = baseCampSeq
    end
    local pals = {}
    for i = 0, math.min(n, MAX_DUMP_PALS) - 1 do
        local okSlot, slot = pcall(function() return container:Get(i) end)
        if okSlot and isValid(slot) then
            local param = slotParam(slot)
            local pj = buildPalJson(param, i, false)
            if pj then
                pj = pj:sub(1, 1) .. '"container":' .. cidx .. ',"group":"' .. group .. '",' .. pj:sub(2)
                if basecamp ~= nil then
                    pj = pj:sub(1, 1) .. '"basecamp":' .. basecamp .. ',' .. pj:sub(2)
                end
                pals[#pals + 1] = pj
            end
        end
    end
    return pals
end

-- Collect every readable container into a single full snapshot (no broadcast here).
-- Returns (palsJsonArray, statsTable).
local function collectAllPals()
    baseCampSeq = 0
    local containers = getContainers()
    local seen = {}
    local all = {}
    local cidx = -1
    for _, c in ipairs(containers) do
        if isValid(c) then
            local addr = c:GetAddress()
            if addr and not seen[addr] then
                seen[addr] = true
                cidx = cidx + 1
                local okC, res = pcall(dumpContainerPals, c, cidx)
                if okC and type(res) == "table" then
                    for _, pj in ipairs(res) do all[#all + 1] = pj end
                else
                    print("[Palws] container " .. cidx .. " dump ERROR: " .. tostring(res) .. "\n")
                end
            end
        end
    end
    return all, { total = #all, containers = cidx + 1 }
end

-- ---------- local player state ----------
local function getLocalPlayerState()
    local ok, controllers = pcall(function() return FindAllOf("PlayerController") end)
    if not (ok and type(controllers) == "table") then return nil end
    for _, controller in ipairs(controllers) do
        if isValid(controller) then
            local okLocal, isLocal = pcall(function()
                if controller.IsLocalPlayerController ~= nil then
                    return controller:IsLocalPlayerController()
                end
                return controller:IsPlayerController()
            end)
            if okLocal and isLocal then
                local okState, state = pcall(function() return controller.PlayerState end)
                if okState and isValid(state) then return state end
            end
        end
    end
    return nil
end

-- ---------- sync state machine ----------
-- syncState only ever holds plain values; never a UObject across timer callbacks.
local syncState = {
    phase = "idle",
    runId = 0,
    requestId = nil,
    trigger = nil,
    nextPage = 0,
    requestedPages = 0,
    requestErrors = 0,
}
local lastAttemptAtSec = -SYNC_COOLDOWN_SEC

local function scheduleOnGameThread(delayMs, fn)
    ExecuteWithDelay(delayMs, guarded("delay", function()
        ExecuteInGameThread(guarded("step", fn))
    end))
end

local function failSync(runId, code, message, retryable)
    if syncState.runId ~= runId then return end
    local ps = getLocalPlayerState()
    if isValid(ps) then
        pcall(function() ps:RequestForceSyncPalBoxSlot_ToServer(false) end)
    end
    emitError(code, message, syncState.requestId, retryable)
    emitStatus("failed", syncState.requestId, syncState.requestedPages, PALBOX_PAGE_COUNT, syncState.trigger)
    print("[Palws] sync failed: " .. code .. " - " .. message .. "\n")
    syncState.phase = "idle"
    syncState.requestId = nil
    syncState.trigger = nil
end

local function finishSync(runId)
    if syncState.runId ~= runId then return end
    local ps = getLocalPlayerState()
    if isValid(ps) then
        pcall(function() ps:RequestForceSyncPalBoxSlot_ToServer(false) end)
    end
    syncState.phase = "collecting"
    emitStatus("collecting", syncState.requestId, syncState.requestedPages, PALBOX_PAGE_COUNT, syncState.trigger)
    local okColl, pals, stats = pcall(collectAllPals)
    if not okColl then
        failSync(runId, "collect-failed", tostring(pals), true)
        return
    end
    syncState.phase = "broadcasting"
    emitStatus("broadcasting", syncState.requestId, syncState.requestedPages, PALBOX_PAGE_COUNT, syncState.trigger)
    emitSnapshot(syncState.requestId, pals, stats.total, syncState.requestedPages, syncState.requestErrors, stats.containers)
    emitStatus("complete", syncState.requestId, syncState.requestedPages, PALBOX_PAGE_COUNT, syncState.trigger)
    print("[Palws] sync complete: " .. stats.total .. " pals from " .. stats.containers .. " containers\n")
    syncState.phase = "idle"
    syncState.requestId = nil
    syncState.trigger = nil
end

local function requestNextPage(runId)
    if syncState.runId ~= runId or syncState.phase ~= "requesting" then return end
    local playerState = getLocalPlayerState()
    if not isValid(playerState) then
        failSync(runId, "player-state-unavailable", "当前未进入可同步的游戏世界", true)
        return
    end
    local page = syncState.nextPage
    local okRequest, reqErr = pcall(function() playerState:RequestPalBoxSyncPage_ToServer(page) end)
    if okRequest then
        syncState.requestedPages = syncState.requestedPages + 1
    else
        syncState.requestErrors = syncState.requestErrors + 1
        emitLog("warn", "page " .. page .. " request failed: " .. tostring(reqErr), syncState.requestId)
    end
    emitStatus("requesting", syncState.requestId, syncState.requestedPages, PALBOX_PAGE_COUNT, syncState.trigger)
    syncState.nextPage = page + 1
    if syncState.nextPage < PALBOX_PAGE_COUNT then
        scheduleOnGameThread(PALBOX_PAGE_REQUEST_DELAY_MS, function() requestNextPage(runId) end)
    else
        syncState.phase = "settling"
        emitStatus("settling", syncState.requestId, syncState.requestedPages, PALBOX_PAGE_COUNT, syncState.trigger)
        scheduleOnGameThread(PALBOX_REPLICATION_SETTLE_MS, function() finishSync(runId) end)
    end
end

local function startSyncRun(runId)
    if syncState.runId ~= runId or syncState.phase ~= "requesting" then return end
    local playerState = getLocalPlayerState()
    if not isValid(playerState) then
        failSync(runId, "player-state-unavailable", "当前未进入可同步的游戏世界", true)
        return
    end
    local okEnable, enableErr = pcall(function() playerState:RequestForceSyncPalBoxSlot_ToServer(true) end)
    if not okEnable then
        failSync(runId, "force-sync-enable-failed", tostring(enableErr), true)
        return
    end
    emitStatus("requesting", syncState.requestId, 0, PALBOX_PAGE_COUNT, syncState.trigger)
    requestNextPage(runId)
end

-- The single explicit sync entry point. Returns whether the async run started.
local function requestSnapshot(opts)
    opts = opts or {}
    if not palws or type(palws.broadcast) ~= "function" then
        return false, "native-error"
    end
    if syncState.phase ~= "idle" then
        return false, "busy"
    end
    local now = os.time()
    if now - lastAttemptAtSec < SYNC_COOLDOWN_SEC then
        return false, "cooldown"
    end
    lastAttemptAtSec = now
    syncState.runId = syncState.runId + 1
    local runId = syncState.runId
    syncState.phase = "requesting"
    syncState.trigger = opts.trigger or "keybind"
    syncState.requestId = opts.requestId
    syncState.nextPage = 0
    syncState.requestedPages = 0
    syncState.requestErrors = 0
    emitStatus("queued", syncState.requestId, 0, PALBOX_PAGE_COUNT, syncState.trigger)
    ExecuteInGameThread(guarded("request-snapshot", function() startSyncRun(runId) end))
    return true
end

-- ---------- command pump (web refresh) ----------
local function reasonMessage(why)
    if why == "busy" then return "已有同步任务进行中" end
    if why == "cooldown" then return "触发过于频繁，请稍后再试" end
    if why == "native-error" then return "原生模块不可用" end
    if why == "not-ready" then return "当前未进入可同步的游戏世界" end
    return "同步请求被拒绝"
end

local function commandPump()
    if palws and type(palws.take_command) == "function" then
        local ok, cmdJson = pcall(palws.take_command)
        if ok and type(cmdJson) == "string" and cmdJson ~= "" then
            local ctype = jsonField(cmdJson, "type")
            local cid = jsonField(cmdJson, "id")
            if ctype == "snapshot.request" then
                local okReq, why = requestSnapshot({ trigger = "web", requestId = cid })
                if okReq then
                    print("[Palws] web snapshot accepted (id=" .. tostring(cid) .. ")\n")
                else
                    print("[Palws] web snapshot rejected: " .. tostring(why) .. "\n")
                    emitError(why or "busy", reasonMessage(why), cid,
                        why == "busy" or why == "cooldown" or why == "not-ready")
                end
            else
                print("[Palws] command ignored: " .. tostring(ctype) .. "\n")
            end
        end
    end
    -- TODO: newer UE4SS exposes LoopInGameThreadWithDelay(delay, fn) which runs
    -- directly on the game thread with a cancellable handle. Prefer it once its
    -- availability/signature is verified against the installed Workshop build.
    ExecuteWithDelay(COMMAND_PUMP_INTERVAL_MS, commandPump)
end
ExecuteWithDelay(COMMAND_PUMP_INTERVAL_MS, commandPump)

-- ---------- keys ----------
RegisterKeyBind(Key.F7, guarded("F7", function()
    local ok, why = requestSnapshot({ trigger = "keybind" })
    if not ok then
        print("[Palws] F7 sync not started: " .. tostring(why) .. "\n")
        emitLog("warn", "同步未启动: " .. tostring(why), nil)
    end
end))

print("[Palws] loaded. F7 = request snapshot\n")
