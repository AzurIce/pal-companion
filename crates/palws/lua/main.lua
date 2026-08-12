-- Palws: sync pal terminal (PalBox) contents to external app over WebSocket.
-- v4: container-direct enumeration + slot-form probing + callback hardening.
--
-- Crash hardening: EVERY callback (NotifyOnNewObject / keybind / timer) is
-- wrapped in xpcall(debug.traceback); errors are logged, never propagate
-- into UE4SS's C++ dispatch (0xe06d7363). The base-class wide capture is
-- registered LAZILY on first F6 (it fires per-widget and is the prime
-- suspect for the post-dump crash during terminal navigation).
--
-- Slot access (kit PalIndividualCharacterSlot.h): slot:IsEmpty(),
-- slot:GetHandle(), slot.ReplicateIndividualParameter; container:Get(i) is
-- 0-based; GetSlots() Lua array is 1-based.

-- field switches for crash bisecting: off = never invoked, outputs default
local READ_PASSIVES = true   -- RE-VERIFY on UE4SS Experimental: FName array marshaling
local READ_GENDER   = true   -- RE-VERIFY on UE4SS Experimental (Palworld): UEnum::Names 0x48 layout
local READ_NICKNAME = true   -- struct FString member, suspected safe
local MAX_DUMP_PALS = 600

print("[Palws] mod loading\n")

-- ---------- callback guard ----------
local function guarded(name, fn)
    return function(...)
        print("[Palws] cb enter: " .. name .. "\n")
        local tb = (type(debug) == "table" and debug.traceback) and debug.traceback or function(e) return tostring(e) end
        local ok, err = xpcall(fn, tb, ...)
        if not ok then
            print("[Palws] CALLBACK-ERROR in " .. name .. ": " .. tostring(err) .. "\n")
        end
        print("[Palws] cb exit: " .. name .. "\n")
    end
end

-- ---------- native module ----------
local palws = nil
do
    local ok, res = pcall(require, "palws")
    print("[Palws] require 'palws': ok=" .. tostring(ok) .. " res=" .. tostring(res) .. "\n")
    if ok and type(res) == "table" then palws = res end
    if not palws then
        local dll = [[G:\SteamLibrary\steamapps\common\Palworld\Pal\Binaries\Win64\Mods\Palws\scripts\palws.dll]]
        local f, err = package.loadlib(dll, "luaopen_palws")
        print("[Palws] loadlib: f=" .. tostring(f) .. " err=" .. tostring(err) .. "\n")
        if f then
            local ok2, res2 = pcall(f)
            print("[Palws] loadlib call: ok=" .. tostring(ok2) .. " res=" .. tostring(res2) .. "\n")
            if ok2 and type(res2) == "table" then palws = res2 end
        end
    end
end
if not palws then
    print("[Palws] FATAL: palws native module unavailable, mod disabled\n")
    return
end

local okStart, startRes = pcall(palws.start_server, 32123)
print("[Palws] start_server: ok=" .. tostring(okStart) .. " -> " .. tostring(startRes) .. "\n")

-- ---------- helpers ----------
local function isValid(obj)
    if obj == nil then return false end
    local ok, valid = pcall(function() return obj:IsValid() end)
    return ok and valid == true
end

local function className(obj)
    -- isValid first: GetClass on a null object wrapper derefs null+offset
    -- natively (AV, uncatchable); struct wrappers have no IsValid -> pcall
    -- fails -> treated as not-an-object. Both safe.
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

local PAYLOAD_PATH = [[C:\Users\xiaob\palworld-dump\palws-payload.json]]
local PAYLOAD_TMP  = PAYLOAD_PATH .. ".tmp"

local function broadcastJson(json)
    local f = io.open(PAYLOAD_TMP, "w")
    if not f then
        print("[Palws] broadcast: cannot open tmp payload file\n")
        return
    end
    f:write(json)
    f:close()
    os.remove(PAYLOAD_PATH)
    local okRen, renErr = os.rename(PAYLOAD_TMP, PAYLOAD_PATH)
    if not okRen then
        print("[Palws] broadcast: rename failed: " .. tostring(renErr) .. "\n")
        return
    end
    local okN, res = pcall(palws.notify)
    print("[Palws] notify: ok=" .. tostring(okN) .. " res=" .. tostring(res)
        .. " bytes=" .. #json .. "\n")
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
-- Reading a nonexistent member on a UE4SS object returns a shell wrapper;
-- touching the shell's members AVs. So: enumerate first, read only known names.
local classCache = {} -- className -> { props={name->true}, funcs={name->true} }

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

-- struct wrapper (e.g. SaveParameter) field whitelist via its UScriptStruct
local structCache = {} -- struct path -> {name->true}
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

local SAVEPARAM_STRUCT = "/Script/Pal.PalIndividualCharacterSaveParameter"

-- validated member access: member enumeration in this UE4SS build is
-- incomplete, so whitelists only inform logs; calls go through pcall and
-- results are validated by expected shape. Shell wrappers (from nonexistent
-- members) are rejected via isValid / tostring-prefix checks.
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
    return true -- "any"
end

-- functions are pcall-safe in this build: always attempt
local function safeCall(obj, name)
    return tryCall(obj, name)
end

local function safeProp(obj, name, expect)
    local v = tryProp(obj, name)
    if not validShape(v, expect or "any") then return nil end
    return v
end

local function structPropType(name)
    local cache = buildStructCache(SAVEPARAM_STRUCT)
    return cache.types and cache.types[name] or nil
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

-- unwrap RemoteUnrealParam / similar wrappers (UE4SS returns these for some
-- UFunction return values and array elements)
local function unwrap(v)
    -- only genuine wrappers: userdata that exposes a callable .get
    -- (a RemoteUnrealParam wrapping a PRIMITIVE may AV inside :get();
    -- callers must only unwrap values known to wrap names/objects)
    -- iterate: wrappers can nest (RemoteUnrealParam > RemoteUnrealParam > FName)
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

-- ---------- field readers ----------
local function looksLikeObjectDump(s)
    return s ~= nil and (s:find("^UObject:") or s:find("^UFunction:") or s:find("^Property")
        or s:find("^RemoteUnrealParam:"))
end

local function mapGenderNumber(n)
    n = math.floor(n)
    if n == 1 then return "male" end
    if n == 2 then return "female" end
    return "unknown"
end

local function mapGenderString(s)
    if s == nil then return "unknown" end
    if s:find("Female") then return "female" end -- check Female first!
    if s:find("Male") then return "male" end
    return "unknown"
end

local function readSpecies(param)
    -- method first (kit: FName GetCharacterID() const)
    local v = safeCall(param, "GetCharacterID")
    if v == nil then v = safeProp(param, "CharacterID", "fname") end
    local s = fnameToString(v)
    if looksLikeObjectDump(s) then return nil end
    return s
end

local function readNickname(param)
    if not READ_NICKNAME then return nil end
    -- struct member read: FString props may come back wrapped (RemoteUnrealParam)
    -- on the experimental build; unwrap + tostring, then filter garbage.
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
    -- function-call only: enum PROPERTY/MEMBER reads AV in this UE4SS build.
    -- UFUNCTION enum returns arrive as plain numbers or FName-ish; no :get().
    local v = safeCall(param, "GetGenderType") or safeCall(param, "GetGender")
    if type(v) == "number" then return mapGenderNumber(v) end
    if type(v) == "string" then return mapGenderString(v) end
    if type(v) == "userdata" then
        local s = fnameToString(v)
        if s and not looksLikeObjectDump(s) then return mapGenderString(s) end
    end
    return "unknown"
end

-- favorite index: 0=none, 1/2/3 = I/II/III (FavoriteIndex is the live field;
-- IsFavoritePal is stale/always-false in v0.7, verified by favscan)
local function readFavorite(param)
    local sp = safeProp(param, "SaveParameter", "struct")
    if sp == nil then return 0 end
    local ok, v = pcall(function() return sp.FavoriteIndex end)
    if ok and type(v) == "number" and v >= 0 and v <= 3 then return math.floor(v) end
    return 0
end

-- lucky (稀有/闪光) flag: IsRarePal — live field, verified by favscan
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
    -- function entry first (kit: UFUNCTION TArray<FName> GetPassiveSkillList);
    -- struct array member as fallback (array != enum, may be safe)
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
    if list == nil then return nil end
    local okN, n = pcall(function() return #list end)
    if not okN or type(n) ~= "number" or n <= 0 then return nil end
    local out = {}
    for i = 1, math.min(n, 16) do
        local okE, e = pcall(function() return list[i] end)
        if okE and e ~= nil then
            -- element is FName per kit (TArray<FName>): try direct ToString
            -- first; only if it is a wrapper do a guarded unwrap
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

-- ---------- pal json ----------
local function readField(name, verbose, fn)
    -- the invoking line prints BEFORE the native call: on a crash the last
    -- invoking line names the killer field
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

local function readMemFields(param)
    -- rust read_saveparam was removed (reflection authoritative since the
    -- UE4SS Experimental Palworld build fixed enum/FName marshaling);
    -- guard so a stale dll still degrades gracefully instead of erroring.
    if type(palws.read_saveparam) ~= "function" then return nil end
    local okA, addr = pcall(function() return param:GetAddress() end)
    if not okA or type(addr) ~= "number" or addr == 0 then
        return nil
    end
    local okR, frag = pcall(palws.read_saveparam, addr)
    if okR and type(frag) == "string" and frag ~= "" then return frag end
    return nil
end

local function buildPalJson(param, idx, verbose)
    if not isValid(param) then return nil end
    if verbose then print("[Palws] step: slot " .. idx .. " read fields\n") end
    local species  = readField("species", verbose, function() return readSpecies(param) end)
    if species == nil or species == "" then return nil end -- app: species required
    local gender   = readField("gender", verbose, function() return readGender(param) end)
    local nickname = readField("nickname", verbose, function() return readNickname(param) end)
    local level    = readField("level", verbose, function() return readLevel(param) end)
    local passives = readField("passives", verbose, function() return readPassives(param) end)
    local favorite = readField("favorite", verbose, function() return readFavorite(param) end)
    local lucky = readField("lucky", verbose, function() return readLucky(param) end)
    local memfrag  = readField("memfields", verbose, function() return readMemFields(param) end)
    -- NOTE: reflection reads are authoritative now (UE4SS Experimental
    -- Palworld build: UEnum::Names 0x48 layout fixed enum + FName marshaling).
    -- memfrag (rust raw-memory path) no longer feeds the payload; the call is
    -- kept so F5 deep-diag can still cross-check the two paths.

    local parts = {}
    parts[#parts + 1] = '"species":' .. jsonStr(species)
    parts[#parts + 1] = '"gender":' .. jsonStr(gender or "unknown")
    local ps = {}
    if passives then
        for _, p in ipairs(passives) do ps[#ps + 1] = jsonStr(p) end
    end
    parts[#parts + 1] = '"passives":[' .. table.concat(ps, ",") .. "]" -- always array
    parts[#parts + 1] = '"nickname":' .. jsonStr(nickname)
    parts[#parts + 1] = '"level":' .. (level and tostring(level) or "null")
    parts[#parts + 1] = '"favorite":' .. tostring(favorite or 0)
    parts[#parts + 1] = '"lucky":' .. tostring(lucky == true)
    return "{" .. table.concat(parts, ",") .. "}"
end

-- slot -> parameter (several routes, kit-named)
local function slotParam(slot)
    if not isValid(slot) then return nil end
    local empty = tryCall(slot, "IsEmpty")
    if empty == true then return nil end
    -- route 1: GetHandle() -> TryGetIndividualParameter()
    local okH, handle = pcall(function() return slot:GetHandle() end)
    if okH and isValid(handle) then
        local okP, param = pcall(function() return handle:TryGetIndividualParameter() end)
        if okP and isValid(param) then return param end
    end
    -- route 2: GetLastHandleForClient()
    local okH2, h2 = pcall(function() return slot:GetLastHandleForClient() end)
    if okH2 and isValid(h2) then
        local okP2, p2 = pcall(function() return h2:TryGetIndividualParameter() end)
        if okP2 and isValid(p2) then return p2 end
    end
    -- route 3: replicated parameter directly on the slot
    local p3 = tryProp(slot, "ReplicateIndividualParameter")
    if isValid(p3) then return p3 end
    -- route 4: raw Handle property
    local h4 = tryProp(slot, "Handle")
    if isValid(h4) then
        local okP4, p4 = pcall(function() return h4:TryGetIndividualParameter() end)
        if okP4 and isValid(p4) then return p4 end
    end
    return nil
end

-- ---------- slot form probe (runs once when a container yields 0 pals) ----------
local probeDone = false
local function probeSlot(container)
    print("[Palws] step: probe slot form\n")
    local slot = nil
    local ok0, s0 = pcall(function() return container:Get(0) end)
    print("[Palws]   Get(0): ok=" .. tostring(ok0) .. " luatype=" .. type(s0) .. "\n")
    if ok0 and s0 ~= nil then slot = s0 end
    if slot == nil then
        local slots = tryCall(container, "GetSlots")
        if slots ~= nil then
            local ok1, s1 = pcall(function() return slots[1] end)
            print("[Palws]   GetSlots()[1]: ok=" .. tostring(ok1) .. " luatype=" .. type(s1) .. "\n")
            if ok1 and s1 ~= nil then slot = s1 end
        end
    end
    if slot == nil then
        print("[Palws]   no slot object obtained at all\n")
        return
    end
    local okT, tt = pcall(function() return slot:type() end)
    print("[Palws]   slot:type() ok=" .. tostring(okT) .. " -> " .. tostring(tt) .. "\n")
    print("[Palws]   className: " .. tostring(className(slot)) .. "\n")
    print("[Palws]   isValid: " .. tostring(isValid(slot)) .. "\n")
    for _, m in ipairs({ "IsEmpty", "GetSlotIndex", "GetHandle", "GetLastHandleForClient" }) do
        local okM, r = pcall(function() return slot[m](slot) end)
        local rinfo = tostring(r)
        if okM and type(r) == "userdata" then rinfo = "userdata valid=" .. tostring(isValid(r)) end
        print("[Palws]   " .. m .. "(): ok=" .. tostring(okM) .. " -> " .. rinfo .. "\n")
    end
    for _, pn in ipairs({ "Handle", "ReplicateIndividualParameter", "SlotIndex", "ContainerId" }) do
        local okP, v = pcall(function() return slot[pn] end)
        local info
        if not okP then info = "ERR"
        elseif v == nil then info = "nil"
        elseif type(v) == "userdata" then
            local vv = isValid(v)
            info = "userdata valid=" .. tostring(vv)
            if vv then info = info .. " class=" .. tostring(className(v)) end
        else info = type(v) .. "=" .. tostring(v) end
        print("[Palws]   prop " .. pn .. ": " .. info .. "\n")
    end
    local param = slotParam(slot)
    print("[Palws]   slotParam -> " .. tostring(isValid(param)) .. "\n")
    if isValid(param) then
        print("[Palws]   species=" .. tostring(readSpecies(param))
            .. " nick=" .. tostring(readNickname(param))
            .. " level=" .. tostring(readLevel(param)) .. "\n")
    end
end

-- ---------- containers ----------
local function getContainers()
    local ok, t = pcall(function() return FindAllOf("PalIndividualCharacterContainer") end)
    if ok and type(t) == "table" then return t end
    return {}
end

local function containerSummary(container)
    local num = tryCall(container, "Num")
    local slots = tryCall(container, "GetSlots")
    local nslots = nil
    if slots ~= nil then
        local ok, n = pcall(function() return #slots end)
        if ok then nslots = n end
    end
    local firstPal = nil
    if nslots and nslots > 0 then
        for i = 0, math.min(nslots - 1, 29) do
            local okS, slot = pcall(function() return container:Get(i) end)
            if okS and isValid(slot) then
                local param = slotParam(slot)
                if isValid(param) then
                    firstPal = readSpecies(param) or readNickname(param) or "?"
                    break
                end
            end
        end
    end
    return num, nslots, firstPal
end

local baseCampSeq = 0  -- per-dump base container ordinal (reset in dumpAll)

local function dumpContainerPals(container, cidx)
    local num = tryCall(container, "Num")
    local slots = tryCall(container, "GetSlots")
    local n = nil
    if slots ~= nil then
        local okN, cnt = pcall(function() return #slots end)
        if okN then n = cnt end
    end
    if n == nil and type(num) == "number" then n = math.floor(num) end
    if n == nil then
        print("[Palws] container " .. cidx .. ": size unknown\n")
        return {}
    end
    -- group by container size: 5=party, 960=palbox, else=base facility
    local group = "base"
    if n == 5 then group = "party" elseif n == 960 then group = "box" end
    local pals = {}
    for i = 0, math.min(n, MAX_DUMP_PALS) - 1 do
        local okSlot, slot = pcall(function() return container:Get(i) end)
        if okSlot and isValid(slot) then
            local param = slotParam(slot)
            -- verbose for the first 3 non-empty slots (field-level detail)
            local verbose = isValid(param) and #pals < 3
            local pj = buildPalJson(param, i, verbose)
            if pj then
                -- tag each pal with its container index + group (party/box/base)
                pj = pj:sub(1, 1) .. '"container":' .. cidx .. ',"group":"' .. group .. '",' .. pj:sub(2)
                if group == "base" then
                    -- base camps are one container each; tag an ordinal so the
                    -- app can group them into per-camp tabs
                    baseCampSeq = baseCampSeq + 1
                    pj = pj:sub(1, 1) .. '"basecamp":' .. baseCampSeq .. ',' .. pj:sub(2)
                end
                pals[#pals + 1] = pj
                if verbose then print("[Palws]   pal[" .. i .. "]: " .. pj .. "\n") end
            end
        end
    end
    if #pals == 0 and not probeDone then
        probeDone = true
        pcall(probeSlot, container)
    end
    print("[Palws] container " .. cidx .. ": dumped " .. #pals .. "/" .. n .. " pals\n")
    return pals
end

local dumpAllPages

local function dumpAll(reason)
    baseCampSeq = 0
    print("[Palws] dumpAll enter (" .. reason .. ")\n")
    local containers = getContainers()
    print("[Palws] containers found: " .. #containers .. "\n")
    if #containers == 0 then
        print("[Palws] dumpAll exit: no containers\n")
        return
    end

    for i, c in ipairs(containers) do
        local num, nslots, firstPal = containerSummary(c)
        print(string.format("[Palws]   container %d: num=%s slots=%s first=%s\n",
            i, tostring(num), tostring(nslots), tostring(firstPal)))
    end

    -- aggregate every container into ONE message so the app's pending
    -- list is never overwritten by rapid successive broadcasts
    local seen = {}   -- container address -> true (dedup across page turns)
    local all = {}
    local cidx = -1
    local function collect(container)
        local addr = container:GetAddress()
        if seen[addr] then return false end
        seen[addr] = true
        cidx = cidx + 1
        local okC, res = pcall(dumpContainerPals, container, cidx)
        if okC and type(res) == "table" then
            for _, pj in ipairs(res) do all[#all + 1] = pj end
            return true
        elseif not okC then
            print("[Palws] container " .. cidx .. " dump ERROR: " .. tostring(res) .. "\n")
        end
        return false
    end
    for _, c in ipairs(containers) do collect(c) end
    -- PalBox pages beyond page 1 are lazily-loaded containers: turn pages on
    -- the box UI and collect any new containers that materialize.
    local extra = dumpAllPages(collect)
    broadcastJson('{"version":1,"source":"palws","event":"' .. reason
        .. '","pals":[' .. table.concat(all, ",") .. "]}")
    print("[Palws] dumpAll exit ok, total " .. #all .. " pals (box pages +" .. extra .. ")\n")
end

-- PalBox is paged (30 slots/page, up to 32 pages = 960). Only the current
-- page's container is materialized; other pages are loaded lazily via UI page
-- turns. Find the box UI (maxPage > 1), turn every page, and collect any
-- newly-appeared PalIndividualCharacterContainer through `collect`.
dumpAllPages = function(collect)
    local okU, uis = pcall(function() return FindAllOf("PalUIPalBoxBase") end)
    if not (okU and type(uis) == "table") then return 0 end
    local boxUI = nil
    for _, ui in ipairs(uis) do
        local okN, n = pcall(function() return ui:GetBoxMaxPageNum() end)
        if okN and type(n) == "number" and n > 1 then boxUI = ui break end
    end
    if boxUI == nil then return 0 end
    local okMax, maxPage = pcall(function() return boxUI:GetBoxMaxPageNum() end)
    if not okMax or type(maxPage) ~= "number" or maxPage <= 1 then return 0 end
    local before = 0
    local okB, cons0 = pcall(function() return FindAllOf("PalIndividualCharacterContainer") end)
    if okB and type(cons0) == "table" then before = #cons0 end
    local extra = 0
    for page = 2, maxPage do
        pcall(function() boxUI:ChangeNextPagePalBoxList() end)
        local okC, cons = pcall(function() return FindAllOf("PalIndividualCharacterContainer") end)
        if okC and type(cons) == "table" then
            for _, c in ipairs(cons) do
                local okA, addr = pcall(function() return c:GetAddress() end)
                if okA and collect(c) then extra = extra + 1 end
            end
        end
    end
    pcall(function() boxUI:SetPagePalBoxList(0) end)  -- back to page 1
    local after = 0
    local okA2, cons2 = pcall(function() return FindAllOf("PalIndividualCharacterContainer") end)
    if okA2 and type(cons2) == "table" then after = #cons2 end
    print(string.format("[Palws] dumpAllPages: maxPage=%d containers %d->%d\n",
        maxPage, before, after))
    return extra
end

-- ---------- deep diagnostics (F5) ----------
local function walkObjectProps(obj, label)
    print("[Palws] step: property walk of " .. label .. "\n")
    local okCls, cls = pcall(function() return obj:GetClass() end)
    if not okCls or cls == nil then
        print("[Palws]   GetClass failed\n")
        return
    end
    local hops = 0
    while cls and hops < 32 do
        hops = hops + 1
        pcall(function()
            cls:ForEachProperty(function(prop)
                local okFull, full = pcall(function() return prop:GetFullName() end)
                if not okFull or full == nil then return false end
                if full:find("^ObjectProperty") or full:find("^ClassProperty") or full:find("^SoftObjectProperty") then
                    local pname = full:match("[:%s]([%w_]+)$") or full
                    local v = tryProp(obj, pname)
                    if isValid(v) then
                        print("[Palws]     prop " .. pname .. " -> " .. (className(v) or "?") .. "\n")
                    end
                end
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

local function census()
    print("[Palws] step: class census\n")
    for _, cn in ipairs({
        "PalUIPalStorageModel", "PalUIPalBoxModel", "PalMapObjectPalStorageModel",
        "PalCharacterContainerManager", "PalIndividualCharacterContainer",
        "PalGlobalPalStorageSubsystem", "PalBaseCampManager",
    }) do
        local ok, t = pcall(function() return FindAllOf(cn) end)
        local cnt = (ok and type(t) == "table") and #t or 0
        print("[Palws]   census " .. cn .. ": " .. cnt .. "\n")
    end
end

-- ---------- triggers ----------
local lastWidget = nil
local dirty = false

local CANDIDATES = {
    "WBP_PalStorageMenu",
    "WBP_IngameMenu_PalBox",
    "WBP_GlobalPalStorage_ForDisplay",
    "WBP_DimensionPalStorage_ForDisplay",
}
-- NotifyOnNewObject requires the FULL class path (e.g. /Script/Engine.Actor)
-- and the class to EXIST at registration time. BP widget classes load lazily
-- (not present at main-menu), so static registration is pointless and noisy.
-- Instead we register dynamically once an instance is observed: resolve the
-- class path from the instance's UClass and hook it then. Registration
-- succeeds because the class now exists.
local dynTried = {}    -- simple class name -> true (attempted)
local dynHooked = {}   -- full path      -> true (registered)
local function tryRegisterByInstance(inst)
    local okC, clsObj = pcall(function() return inst:GetClass() end)
    if not (okC and isValid(clsObj)) then return false end
    local okF, full = pcall(function() return clsObj:GetFullName() end)
    -- UClass GetFullName: "Class /Game/.../WBP_X.WBP_X_C" -> take path part
    local path = okF and tostring(full):match("^%S+%s+(.+)$") or nil
    if not path then return false end
    if dynHooked[path] then return true end
    local ok, err = pcall(function()
        NotifyOnNewObject(path, guarded("notify-" .. path, function(obj)
            if isValid(obj) then
                lastWidget = obj
                dirty = true
                print("[Palws] widget created: " .. path .. "\n")
            end
        end))
    end)
    if ok then
        dynHooked[path] = true
        print("[Palws] NotifyOnNewObject registered: " .. path .. "\n")
    else
        print("[Palws] NotifyOnNewObject FAILED " .. path .. ": " .. tostring(err) .. "\n")
    end
    return ok
end
-- lazy hook sweep: probe each candidate's class once its first instance shows
-- up in the world; cheap FindFirstOf per pump tick until class is loaded.
local function sweepDynamicHooks()
    for _, cls in ipairs(CANDIDATES) do
        if not dynTried[cls] then
            local ok, obj = pcall(function() return FindFirstOf(cls .. "_C") end)
            if ok and isValid(obj) then
                dynTried[cls] = true
                tryRegisterByInstance(obj)
            end
        end
    end
end

-- wide base-class capture: registered LAZILY on first F6 (prime suspect for
-- the post-dump crash during terminal navigation; terminal class is known
-- now, so it is not needed for normal operation)
local captureMode = false
local wideRegistered = false
local seenClasses = {}
local function registerWideCapture()
    if wideRegistered then return end
    wideRegistered = true
    local ok, err = pcall(function()
        NotifyOnNewObject("/Script/UMG.UserWidget", guarded("wide-capture", function(obj)
            local cn = className(obj)
            if cn == nil then return end
            if not seenClasses[cn] then
                seenClasses[cn] = true
                print("[Palws] CAPTURE widget: " .. cn .. "\n")
            end
        end))
    end)
    print("[Palws] wide capture register: " .. (ok and "ok" or ("FAILED " .. tostring(err))) .. "\n")
end

-- ---------- polling trigger (creation events proved unreliable) ----------
local terminalActive = false
local pollNoteLogged = false

local function probeActive(w)
    -- CommonActivatableWidget: IsActivated(); generic: IsVisible(); Visibility prop
    local okA, a = pcall(function() return w:IsActivated() end)
    if okA and type(a) == "boolean" then return a, "IsActivated" end
    local okV, v = pcall(function() return w:IsVisible() end)
    if okV and type(v) == "boolean" then return v, "IsVisible" end
    local okP, p = pcall(function() return w.Visibility end)
    if okP and p ~= nil then
        p = unwrap(p)
        if type(p) == "number" then
            -- ESlateVisibility: 0=Visible 1=Collapsed 2=Hidden 3/4=HitTestInvisible
            return p == 0, "Visibility=" .. p
        end
        local s = fnameToString(p)
        if s then
            if s:find("Collapsed") or s:find("Hidden") then return false, "Visibility=" .. s end
            if s:find("Visible") then return true, "Visibility=" .. s end
        end
    end
    return nil, nil
end

local function pollTerminal()
    local ok, obj = pcall(function() return FindFirstOf("WBP_PalStorageMenu_C") end)
    if not (ok and isValid(obj)) then
        if terminalActive then terminalActive = false end
        if not pollNoteLogged then
            pollNoteLogged = true
            print("[Palws] poll: WBP_PalStorageMenu_C not found (yet)\n")
        end
        return
    end
    if pollNoteLogged then
        pollNoteLogged = false
        print("[Palws] poll: terminal widget instance acquired\n")
    end
    local active, how = probeActive(obj)
    if active == nil then
        if not pollNoteLogged then
            pollNoteLogged = true
            print("[Palws] poll: no activation probe worked (IsActivated/IsVisible/Visibility all failed)\n")
        end
        return
    end
    if active and not terminalActive then
        terminalActive = true
        lastWidget = obj
        dirty = true
        print("[Palws] poll: terminal OPEN detected via " .. tostring(how) .. "\n")
    elseif (not active) and terminalActive then
        terminalActive = false
        print("[Palws] poll: terminal closed\n")
    end
end

local function pump()
    -- lazy class hooks: register NotifyOnNewObject once each candidate's class
    -- is observed in the world (dynamic full-path registration)
    sweepDynamicHooks()
    pcall(pollTerminal)
    if dirty then
        dirty = false
        ExecuteWithDelay(1500, guarded("pump-dump", function()
            ExecuteInGameThread(guarded("dumpAll-timer", function()
                local okD, errD = pcall(dumpAll, "terminal-open")
                if not okD then print("[Palws] dumpAll ERROR: " .. tostring(errD) .. "\n") end
            end))
        end))
    end
    ExecuteWithDelay(500, pump)
end
ExecuteWithDelay(500, pump)

-- ---------- keys ----------
RegisterKeyBind(Key.F6, guarded("F6", function()
    captureMode = not captureMode
    if captureMode then registerWideCapture() end
    print("[Palws] capture " .. (captureMode and "ON (wide capture registered)" or "OFF") .. "\n")
end))

RegisterKeyBind(Key.F7, guarded("F7", function()
    ExecuteInGameThread(guarded("dumpAll-f7", function()
        local okD, errD = pcall(dumpAll, "manual-f7")
        if not okD then print("[Palws] dumpAll ERROR: " .. tostring(errD) .. "\n") end
    end))
end))

-- ---------- F4: runtime introspection of one real pal parameter ----------
local function describeValue(label, v)
    local t = type(v)
    local line = "[Palws]   introspect " .. label .. ": type=" .. t
    if t == "userdata" then
        line = line .. " valid=" .. tostring(isValid(v))
        local okS, str = pcall(tostring, v)
        if okS then line = line .. " tostring=" .. str end
        local okT, ts = pcall(function() return v:ToString() end)
        if okT then line = line .. " ToString=" .. tostring(ts) end
    else
        line = line .. " value=" .. tostring(v)
    end
    print(line .. "\n")
end

local function dissectElement(e, tag)
    print("[Palws]   dissect " .. tag .. ": type=" .. type(e) .. "\n")
    local ok1, r1 = pcall(tostring, e)
    print("[Palws]     tostring: ok=" .. tostring(ok1) .. " -> " .. tostring(r1) .. "\n")
    local ok2, r2 = pcall(function() return e:ToString() end)
    print("[Palws]     :ToString(): ok=" .. tostring(ok2) .. " -> " .. tostring(r2) .. "\n")
    local okG, g = pcall(function() return e.get end)
    print("[Palws]     .get exists: " .. tostring(okG and type(g) == "function") .. "\n")
    if okG and type(g) == "function" then
        local ok3, r3 = pcall(function() return e:get() end)
        print("[Palws]     :get(): ok=" .. tostring(ok3) .. " type=" .. type(r3) .. "\n")
        if ok3 and r3 ~= nil then
            local ok4, r4 = pcall(function() return r3:ToString() end)
            print("[Palws]     :get():ToString(): ok=" .. tostring(ok4) .. " -> " .. tostring(r4) .. "\n")
            local ok5, r5 = pcall(tostring, r3)
            print("[Palws]     tostring(get()): ok=" .. tostring(ok5) .. " -> " .. tostring(r5) .. "\n")
        end
    end
    for _, fn in ipairs({ "Name", "Value", "text", "Text" }) do
        local okF, rf = pcall(function() return e[fn] end)
        print("[Palws]     ." .. fn .. ": ok=" .. tostring(okF) .. " -> " .. tostring(rf) .. "\n")
    end
    local okP, rp = pcall(function()
        local acc = {}
        for k, v in pairs(e) do acc[#acc + 1] = tostring(k) .. "=" .. tostring(v) end
        return table.concat(acc, ", ")
    end)
    print("[Palws]     pairs(): ok=" .. tostring(okP) .. " -> " .. tostring(rp) .. "\n")
    local okTy, rTy = pcall(function() return e:type() end)
    print("[Palws]     :type(): ok=" .. tostring(okTy) .. " -> " .. tostring(rTy) .. "\n")
end

local function introspectParam(param)
    print("[Palws] introspect: param class=" .. tostring(className(param)) .. "\n")
    -- GROUND TRUTH FIRST: full member lists of the parameter class
    local cache = buildClassCache(param)
    if cache then
        local pn = {}
        for k in pairs(cache.props) do pn[#pn + 1] = k end
        table.sort(pn)
        print("[Palws] PROP-LIST (" .. #pn .. "): " .. table.concat(pn, ", ") .. "\n")
        local fn = {}
        for k in pairs(cache.funcs) do fn[#fn + 1] = k end
        table.sort(fn)
        print("[Palws] FUNC-LIST (" .. #fn .. "): " .. table.concat(fn, ", ") .. "\n")
    else
        print("[Palws] buildClassCache failed\n")
    end
    local sc = buildStructCache(SAVEPARAM_STRUCT)
    local sn = {}
    for k in pairs(sc) do sn[#sn + 1] = k end
    table.sort(sn)
    print("[Palws] SAVEPARAM-FIELDS (" .. #sn .. "): " .. table.concat(sn, ", ") .. "\n")
    -- candidate properties on the parameter object
    for _, pn in ipairs({ "SaveParameter", "SaveParameterMirror", "Gender", "GenderType",
        "NickName", "Nickname", "PassiveSkillList", "CharacterID", "Level" }) do
        local cache0 = buildClassCache(param)
        local pt = cache0 and cache0.ptypes[pn] or nil
        if pt == "EnumProperty" or pt == "ByteProperty" then
            print("[Palws]   introspect param." .. pn .. ": skipped-enum" .. "\n")
        elseif cache0 and not cache0.props[pn] then
            print("[Palws]   introspect param." .. pn .. ": not-in-whitelist" .. "\n")
        else
            print("[Palws]   introspect param." .. pn .. " reading (" .. tostring(pt) .. ")..." .. "\n")
            local ok, v = pcall(function() return param[pn] end)
            if ok then describeValue("param." .. pn, v) else print("[Palws]   introspect param." .. pn .. ": ERR" .. "\n") end
        end
    end
    -- SaveParameter struct members
    local sp = tryProp(param, "SaveParameter")
    if sp ~= nil then
        print("[Palws]   SaveParameter acquired, type=" .. type(sp) .. "\n")
        for _, mn in ipairs({ "Gender", "NickName", "FilteredNickName", "PassiveSkillList",
            "Talent_HP", "Talent_Melee", "Talent_Shot", "Talent_Defense", "Level", "Rank",
            "Rank_HP", "Rank_Attack", "Rank_CraftSpeed" }) do
            local ptype = structPropType(mn)
            if ptype == "EnumProperty" or ptype == "ByteProperty" then
                print("[Palws]   introspect sp." .. mn .. ": skipped-enum (" .. tostring(ptype) .. ")" .. "\n")
            else
                print("[Palws]   introspect sp." .. mn .. " reading (" .. tostring(ptype) .. ")..." .. "\n")
                local ok, v = pcall(function() return sp[mn] end)
                if ok then describeValue("sp." .. mn, v) else print("[Palws]   introspect sp." .. mn .. ": ERR" .. "\n") end
            end
        end
        -- passive array dissection
        local pl = tryProp(sp, "PassiveSkillList")
        if pl ~= nil then
            local okN, n = pcall(function() return #pl end)
            print("[Palws]   sp.PassiveSkillList len: ok=" .. tostring(okN) .. " n=" .. tostring(n) .. "\n")
            if okN and type(n) == "number" and n > 0 then
                for i = 1, math.min(n, 2) do
                    local okE, e = pcall(function() return pl[i] end)
                    if okE then dissectElement(e, "passive[" .. i .. "]") end
                end
                -- pairs/ipairs directly on the array
                local okP, rp = pcall(function()
                    local acc = {}
                    for i, v in ipairs(pl) do
                        acc[#acc + 1] = i .. ":" .. type(v) .. "=" .. tostring(v)
                        if #acc >= 4 then break end
                    end
                    return table.concat(acc, " | ")
                end)
                print("[Palws]   ipairs(array): ok=" .. tostring(okP) .. " -> " .. tostring(rp) .. "\n")
            end
        end
    else
        print("[Palws]   SaveParameter NOT readable\n")
    end
    -- candidate methods
    for _, m in ipairs({ "GetGenderType", "GetGender", "GetNickname", "GetLevel", "GetSaveParameter", "GetPassiveSkillList" }) do
        local ok, v = pcall(function() return param[m](param) end)
        if ok then describeValue("param:" .. m .. "()", v) else print("[Palws]   introspect param:" .. m .. "(): ERR\n") end
    end
end

local function introspectFirstPal()
    local containers = getContainers()
    for _, c in ipairs(containers) do
        local num = tryCall(c, "Num")
        local n = type(num) == "number" and math.floor(num) or 0
        for i = 0, math.min(n, 50) - 1 do
            local okS, slot = pcall(function() return c:Get(i) end)
            if okS and isValid(slot) then
                local param = slotParam(slot)
                if isValid(param) then
                    introspectParam(param)
                    return
                end
            end
        end
    end
    print("[Palws] introspect: no pal found in any container\n")
end

-- ---------- F4: memory mapping dump (calibrate struct layout by anchors) ----------
local function memmapFirstPal()
    if not palws or type(palws.hexdump) ~= "function" then
        print("[Palws] memmap: hexdump not in dll\n")
        return
    end
    local containers = getContainers()
    for ci, c in ipairs(containers) do
        local okN, num = pcall(function() return c:Num() end)
        if okN and type(num) == "number" and num > 0 then
            for i = 0, num - 1 do
                local okS, slot = pcall(function() return c:Get(i) end)
                if okS and isValid(slot) and tryCall(slot, "IsEmpty") == false then
                    local param = slotParam(slot)
                    if isValid(param) then
                        print("[Palws] memmap: container " .. ci .. " slot " .. i .. "\n")
                        print("[Palws] memmap anchor species=" .. tostring(readSpecies(param))
                            .. " level=" .. tostring(readLevel(param)) .. "\n")
                        local okPA, paddr = pcall(function() return param:GetAddress() end)
                        print("[Palws] memmap param addr: ok=" .. tostring(okPA)
                            .. " addr=" .. tostring(paddr) .. "\n")
                        if okPA and type(paddr) == "number" and paddr > 0 then
                            pcall(palws.hexdump, paddr, 0x800)
                        end
                        local sp = safeProp(param, "SaveParameter", "struct")
                        if sp ~= nil then
                            local okSA, saddr = pcall(function() return sp:GetAddress() end)
                            print("[Palws] memmap saveparam addr: ok=" .. tostring(okSA)
                                .. " addr=" .. tostring(saddr) .. "\n")
                            if okSA and type(saddr) == "number" and saddr > 0 then
                                pcall(palws.hexdump, saddr, 0x100)
                            end
                            -- favorite/rare fields: try direct struct-member reads
                            -- (BoolProperty/ByteProperty — not enum, may work)
                            for _, f in ipairs({ "IsFavoritePal", "FavoriteIndex", "IsRarePal" }) do
                                local okF, v = pcall(function() return sp[f] end)
                                print("[Palws] memmap sp." .. f .. ": ok=" .. tostring(okF)
                                    .. " v=" .. tostring(v) .. " type=" .. type(v) .. "\n")
                            end
                        end
                        return
                    end
                end
            end
        end
    end
    print("[Palws] memmap: no pal found\n")
end

-- ---------- F3: favorite field scan (are IsFavoritePal/FavoriteIndex live?) ----------
local function favoriteScan()
    local containers = getContainers()
    local total, favTrue, idxNonZero, rareTrue, samples = 0, 0, 0, 0, {}
    for _, c in ipairs(containers) do
        local okN, num = pcall(function() return c:Num() end)
        if okN and type(num) == "number" and num > 0 then
            for i = 0, math.min(num, MAX_DUMP_PALS) - 1 do
                local okS, slot = pcall(function() return c:Get(i) end)
                if okS and isValid(slot) and tryCall(slot, "IsEmpty") == false then
                    local param = slotParam(slot)
                    if isValid(param) then
                        total = total + 1
                        local sp = safeProp(param, "SaveParameter", "struct")
                        if sp ~= nil then
                            local _, fav = pcall(function() return sp.IsFavoritePal end)
                            local _, idx = pcall(function() return sp.FavoriteIndex end)
                            local _, rare = pcall(function() return sp.IsRarePal end)
                            local species = tostring(readSpecies(param))
                            if rare == true then
                                rareTrue = rareTrue + 1
                                -- sample rare pals WITHOUT boss prefix (decides rare=alpha or lucky)
                                if not species:match("^BOSS_") and not species:match("^Boss_")
                                    and #samples < 12 then
                                    samples[#samples + 1] = species .. " rare=true idx=" .. tostring(idx)
                                end
                            end
                            if fav == true then
                                favTrue = favTrue + 1
                            elseif type(idx) == "number" and idx ~= 0 then
                                idxNonZero = idxNonZero + 1
                            end
                        end
                    end
                end
            end
        end
    end
    print("[Palws] favscan: total=" .. total .. " favTrue=" .. favTrue
        .. " idxNonZero=" .. idxNonZero .. " rareTrue=" .. rareTrue .. "\n")
    for _, s in ipairs(samples) do print("[Palws]   " .. s .. "\n") end
end

RegisterKeyBind(Key.F3, guarded("F3", function()
    ExecuteInGameThread(guarded("favscan", function()
        pcall(favoriteScan)
    end))
end))

RegisterKeyBind(Key.F4, guarded("F4", function()
    ExecuteInGameThread(guarded("memmap", function()
        pcall(memmapFirstPal)
    end))
end))

RegisterKeyBind(Key.F5, guarded("F5", function()
    ExecuteInGameThread(guarded("deepdiag", function()
        if isValid(lastWidget) then
            walkObjectProps(lastWidget, className(lastWidget) or "widget")
        end
        local okM, mgr = pcall(function() return FindFirstOf("PalCharacterContainerManager") end)
        if okM and isValid(mgr) then walkObjectProps(mgr, "PalCharacterContainerManager") end
        local okS, sub = pcall(function() return FindFirstOf("PalGlobalPalStorageSubsystem") end)
        if okS and isValid(sub) then walkObjectProps(sub, "PalGlobalPalStorageSubsystem") end
        -- box paging diagnostics: how many UI models, page counts, containers
        local okU, uis = pcall(function() return FindAllOf("PalUIPalBoxBase") end)
        print("[Palws] F5: PalUIPalBoxBase count=" .. tostring(okU and #uis or "err") .. "\n")
        if okU then
            for _, ui in ipairs(uis) do
                local okN, n = pcall(function() return ui:GetBoxMaxPageNum() end)
                print("[Palws]   boxUI addr=" .. tostring(ui:GetAddress())
                    .. " maxPage=" .. tostring(okN and n or "?"))
            end
        end
        local okP, models = pcall(function() return FindAllOf("PalUIPalStorageModel") end)
        print("[Palws] F5: PalUIPalStorageModel count=" .. tostring(okP and models and #models or "err") .. "\n")
        if okP and type(models) == "table" then
            for _, m in ipairs(models) do
                local okN, n = pcall(function() return m:GetWholePageCount() end)
                local okT, tgt = pcall(function() return m:GetTargetContainerId() end)
                print("[Palws]   model addr=" .. tostring(m:GetAddress())
                    .. " wholePages=" .. tostring(okN and n or "?")
                    .. " target=" .. tostring(okT and tostring(tgt) or "?"))
            end
        end
        local okC, cons = pcall(function() return FindAllOf("PalIndividualCharacterContainer") end)
        print("[Palws] F5: containers=" .. tostring(okC and #cons or "err") .. "\n")
        if okC then
            for _, c in ipairs(cons) do
                local num = tryCall(c, "Num")
                local slots = tryCall(c, "GetSlots")
                local n = slots and #slots or num
                print("[Palws]   container addr=" .. tostring(c:GetAddress()) .. " n=" .. tostring(n))
            end
        end
        census()
    end))
end))

RegisterKeyBind(Key.F8, guarded("F8", function()
    local json = '{"version":1,"source":"palws","event":"fake-f8","pals":['
        .. '{"species":"PinkCat","gender":"male","passives":["Brave","BurlyBody"],"nickname":"测试猫","level":12},'
        .. '{"species":"CaptainPenguin","gender":"female","passives":["Workaholic"],"nickname":"Penking","level":34},'
        .. '{"species":"NegativeKoala","gender":"unknown","passives":[],"nickname":null,"level":5}'
        .. "]}"
    broadcastJson(json)
end))

-- ---------- F9: server-side PalBox replication experiment ----------
-- Ask the owning PlayerState to force PalBox slot replication, then observe
-- the native OnRep callback for five seconds. This deliberately does not read
-- pal fields, scan the 960-slot container, turn pages, or broadcast a payload.
local forceSyncExperiment = {
    active = false,
    callbacks = 0,
    uniqueSlots = {},
    pages = {},
    minSlot = nil,
    maxSlot = nil,
}

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

local function resetForceSyncExperiment()
    forceSyncExperiment.callbacks = 0
    forceSyncExperiment.uniqueSlots = {}
    forceSyncExperiment.pages = {}
    forceSyncExperiment.minSlot = nil
    forceSyncExperiment.maxSlot = nil
end

local okRepHook, repHookErr = pcall(function()
    RegisterHook("/Script/Pal.PalIndividualCharacterSlot:OnRep_Parameter",
        guarded("force-sync-onrep", function(Context)
            if not forceSyncExperiment.active then return end
            local okSlot, slot = pcall(function() return Context:get() end)
            if not (okSlot and isValid(slot)) then return end

            forceSyncExperiment.callbacks = forceSyncExperiment.callbacks + 1
            local okAddr, addr = pcall(function() return slot:GetAddress() end)
            if okAddr then forceSyncExperiment.uniqueSlots[tostring(addr)] = true end

            local okIndex, index = pcall(function() return slot:GetSlotIndex() end)
            if okIndex and type(index) == "number" and index >= 0 then
                local page = math.floor(index / 30)
                forceSyncExperiment.pages[page] = true
                if forceSyncExperiment.minSlot == nil or index < forceSyncExperiment.minSlot then
                    forceSyncExperiment.minSlot = index
                end
                if forceSyncExperiment.maxSlot == nil or index > forceSyncExperiment.maxSlot then
                    forceSyncExperiment.maxSlot = index
                end
            end
        end))
end)
print("[Palws] force-sync OnRep hook: "
    .. (okRepHook and "ok" or ("FAILED " .. tostring(repHookErr))) .. "\n")

RegisterKeyBind(Key.F9, guarded("F9", function()
    if forceSyncExperiment.active then
        print("[Palws] force-sync experiment already active\n")
        return
    end
    ExecuteInGameThread(guarded("force-sync-start", function()
        local playerState = getLocalPlayerState()
        if not isValid(playerState) then
            print("[Palws] force-sync: local PlayerState not found\n")
            return
        end

        resetForceSyncExperiment()
        forceSyncExperiment.active = true
        local okEnable, enableErr = pcall(function()
            playerState:RequestForceSyncPalBoxSlot_ToServer(true)
        end)
        if not okEnable then
            forceSyncExperiment.active = false
            print("[Palws] force-sync enable FAILED: " .. tostring(enableErr) .. "\n")
            return
        end
        print("[Palws] force-sync enabled; observing OnRep_Parameter for 5s\n")

        ExecuteWithDelay(5000, guarded("force-sync-finish", function()
            local okDisable, disableErr = pcall(function()
                if isValid(playerState) then
                    playerState:RequestForceSyncPalBoxSlot_ToServer(false)
                end
            end)
            forceSyncExperiment.active = false
            print(string.format(
                "[Palws] force-sync result: callbacks=%d uniqueSlots=%d pages=%d slotRange=%s..%s disable=%s\n",
                forceSyncExperiment.callbacks,
                countTable(forceSyncExperiment.uniqueSlots),
                countTable(forceSyncExperiment.pages),
                tostring(forceSyncExperiment.minSlot),
                tostring(forceSyncExperiment.maxSlot),
                okDisable and "ok" or ("FAILED " .. tostring(disableErr))))
        end))
    end))
end))

-- ---------- load-time self-check: every critical function must be callable ----------
do
    local required = {
        "isValid", "className", "jsonEscape", "jsonStr", "broadcastJson",
        "tryCall", "tryProp", "fnameToString", "unwrap", "looksLikeObjectDump",
        "isShell", "validShape", "safeCall", "safeProp", "safeStructProp",
        "structPropType", "mapGenderNumber", "mapGenderString",
        "readNickname", "readSpecies", "readGender", "readLevel", "readPassives",
        "readField", "buildPalJson", "slotParam", "probeSlot",
        "getContainers", "containerSummary", "dumpContainerPals", "dumpAll",
        "walkObjectProps", "census", "pollTerminal", "pump",
        "getLocalPlayerState", "resetForceSyncExperiment",
        "buildClassCache", "buildStructCache",
    }
    local scope = {
        isValid=isValid, className=className, jsonEscape=jsonEscape, jsonStr=jsonStr,
        broadcastJson=broadcastJson, tryCall=tryCall, tryProp=tryProp,
        fnameToString=fnameToString, unwrap=unwrap, looksLikeObjectDump=looksLikeObjectDump,
        isShell=isShell, validShape=validShape, safeCall=safeCall, safeProp=safeProp,
        safeStructProp=safeStructProp, structPropType=structPropType,
        mapGenderNumber=mapGenderNumber, mapGenderString=mapGenderString,
        readNickname=readNickname, readSpecies=readSpecies, readGender=readGender,
        readLevel=readLevel, readPassives=readPassives, readField=readField,
        buildPalJson=buildPalJson, slotParam=slotParam, probeSlot=probeSlot,
        getContainers=getContainers, containerSummary=containerSummary,
        dumpContainerPals=dumpContainerPals, dumpAll=dumpAll,
        walkObjectProps=walkObjectProps, census=census, pollTerminal=pollTerminal, pump=pump,
        getLocalPlayerState=getLocalPlayerState,
        resetForceSyncExperiment=resetForceSyncExperiment,
        buildClassCache=buildClassCache, buildStructCache=buildStructCache,
    }
    local fails = {}
    for _, n in ipairs(required) do
        if type(scope[n]) ~= "function" then fails[#fails + 1] = n end
    end
    if #fails == 0 then
        print("[Palws] SELF-CHECK PASS (" .. #required .. " functions)" .. "\n")
    else
        print("[Palws] SELF-CHECK FAIL: " .. table.concat(fails, ", ") .. "\n")
    end
end

print("[Palws] loaded. F5=deep-diag F6=capture F7=dump F8=fake\n")
