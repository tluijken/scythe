local path = os.getenv and os.getenv("OUT")
local f = path and io.open(path, "w")
local function fmt(v)
  if type(v) == "number" then return string.format("%.6g", v) end
  if type(v) == "userdata" then return "ptr" end
  return tostring(v)
end
local function out(label, ...)
  local t = {label}
  for i = 1, select("#", ...) do t[#t + 1] = fmt((select(i, ...))) end
  local line = table.concat(t, " ") .. "\n"
  if f then f:write(line) f:flush() else reaper.ShowConsoleMsg(line) end
end
local sec = reaper.SectionFromUniqueID(0)
local two, loop, mix = reaper.NamedCommandLookup("_wp12two"), reaper.NamedCommandLookup("_wp12loop"), reaper.NamedCommandLookup("_wp12mix")
out("lookup", two > 0, loop > two, mix > loop, reaper.NamedCommandLookup("wp12two"), reaper.NamedCommandLookup("_nope"), reaper.NamedCommandLookup("40001"))
out("reverse", reaper.ReverseNamedCommandLookup(two), reaper.ReverseNamedCommandLookup(loop), reaper.ReverseNamedCommandLookup(40001))
out("names", reaper.kbd_getTextFromCmd(two, sec), reaper.kbd_getTextFromCmd(40001, sec), reaper.kbd_getTextFromCmd(1068, 0), reaper.kbd_getTextFromCmd(40364, nil))
local function shortcuts(id)
  local t = {}
  for i = 0, reaper.CountActionShortcuts(sec, id) - 1 do
    local ok, d = reaper.GetActionShortcutDesc(sec, id, i)
    t[#t + 1] = d
  end
  return reaper.CountActionShortcuts(sec, id), table.concat(t, "|")
end
out("keys40001", shortcuts(40001))
out("keysloop", shortcuts(loop))
out("keys1068", shortcuts(1068))
out("tog0", reaper.GetToggleCommandState(1068), reaper.GetToggleCommandState(loop), reaper.GetToggleCommandState(mix), reaper.GetToggleCommandState(two), reaper.GetToggleCommandStateEx(0, 1068), reaper.GetToggleCommandState(40001))
reaper.Main_OnCommand(loop, 0)
out("tog1", reaper.GetToggleCommandState(1068), reaper.GetToggleCommandState(loop), reaper.GetToggleCommandState(mix))
reaper.Main_OnCommand(loop, 0)
out("tog2", reaper.GetToggleCommandState(1068), reaper.GetToggleCommandState(loop))
reaper.Main_OnCommand(two, 0)
out("two", reaper.CountTracks(0))
reaper.Main_OnCommandEx(mix, 0, 0)
out("mix", reaper.CountTracks(0), reaper.GetToggleCommandState(1068))
reaper.Main_OnCommand(1068, 0)
local is_new, fn, secid, cmd, mode, res, val = reaper.get_action_context()
out("context", is_new, fn:match("[^/]*$"), secid, cmd > 0, mode, res, val)
-- state chunks
local tr = reaper.GetTrack(0, 0)
reaper.GetSetMediaTrackInfo_String(tr, "P_NAME", "Lead", true)
reaper.SetMediaTrackInfo_Value(tr, "D_VOL", 0.5)
local ok, chunk = reaper.GetTrackStateChunk(tr, "", false)
out("tchunk", ok, chunk:sub(1, 6), chunk:match("\nNAME ([^\n]*)"), chunk:match("\nVOLPAN ([%d%.]+)"), chunk:sub(-2) == ">\n")
chunk = chunk:gsub("\nNAME [^\n]*", "\nNAME Renamed", 1)
out("tset", reaper.SetTrackStateChunk(tr, chunk, false), reaper.GetTrackName(tr), reaper.GetMediaTrackInfo_Value(tr, "D_VOL"), reaper.CountTracks(0))
local envchunk = "<TRACK\nNAME Env\nVOLPAN 1 0 -1 -1 1\n<VOLENV2\nACT 1 -1\nVIS 1 1 1\nARM 0\nDEFSHAPE 0 -1 -1\nPT 0 1 0\nPT 2 0.5 0\n>\n<ITEM\nPOSITION 1\nLENGTH 2\nNAME Clip\n>\n>\n"
local tr2 = reaper.GetTrack(0, 1)
out("tset2", reaper.SetTrackStateChunk(tr2, envchunk, false), reaper.GetTrackName(tr2), reaper.CountTrackMediaItems(tr2), reaper.CountTrackEnvelopes(tr2))
local env = reaper.GetTrackEnvelopeByName(tr2, "Volume")
out("env", env ~= nil, env and reaper.CountEnvelopePoints(env))
local eok, echunk = reaper.GetEnvelopeStateChunk(env, "", false)
out("echunk", eok, echunk:sub(1, 9), echunk:match("\nPT 2 0.5 0") ~= nil, echunk:match("\nACT ([^\n]*)"))
local newenv = echunk:gsub("\nPT 2 0.5 0[^\n]*", "\nPT 2 0.25 0\nPT 3 0.75 0", 1)
out("eset", reaper.SetEnvelopeStateChunk(env, newenv, false), reaper.CountEnvelopePoints(env), select(3, reaper.GetEnvelopePoint(env, 2)))
local item = reaper.GetTrackMediaItem(tr2, 0)
local iok, ichunk = reaper.GetItemStateChunk(item, "", false)
out("ichunk", iok, ichunk:sub(1, 5), ichunk:match("\nPOSITION ([^\n]*)"), ichunk:match("\nLENGTH ([^\n]*)"), ichunk:match("\nNAME ([^\n]*)"))
ichunk = ichunk:gsub("\nPOSITION [^\n]*", "\nPOSITION 3", 1)
out("iset", reaper.SetItemStateChunk(item, ichunk, false), reaper.GetMediaItemInfo_Value(item, "D_POSITION"), reaper.GetMediaItemInfo_Value(item, "D_LENGTH"), reaper.CountMediaItems(0))
-- mouse modifiers
reaper.SetMouseModifier("MM_CTX_ITEM_DBLCLK", 4, "_wp12loop")
out("mm2", reaper.GetMouseModifier("MM_CTX_ITEM_DBLCLK", 4))
reaper.SetMouseModifier("MM_CTX_ITEM_DBLCLK", 4, "40001")
out("mm3", reaper.GetMouseModifier("MM_CTX_ITEM_DBLCLK", 4))
reaper.SetMouseModifier("MM_CTX_ITEM_DBLCLK", 4, -1)
-- toggles of scripts
out("settog", reaper.SetToggleCommandState(0, cmd, 1), reaper.GetToggleCommandState(cmd))
reaper.RefreshToolbar2(0, cmd)
reaper.RefreshToolbar(cmd)
-- misc
local g = reaper.genGuid("")
out("guid", #g, g:sub(1, 1), g:sub(-1))
out("solo", reaper.AnyTrackSolo(0))
reaper.SetMediaTrackInfo_Value(tr, "I_SOLO", 1)
out("solo2", reaper.AnyTrackSolo(0), reaper.GetNumTracks())
reaper.SoloAllTracks(0)
out("solo3", reaper.AnyTrackSolo(0))
reaper.MuteAllTracks(true)
out("mute", reaper.GetMediaTrackInfo_Value(tr, "B_MUTE"), reaper.GetMediaTrackInfo_Value(tr2, "B_MUTE"))
reaper.MuteAllTracks(false)
reaper.SetMediaTrackInfo_Value(tr, "I_RECARM", 1)
reaper.ClearAllRecArmed()
out("arm", reaper.GetMediaTrackInfo_Value(tr, "I_RECARM"))
local gtr = reaper.GetTrackGUID(tr)
out("trguid", #gtr, reaper.GetMediaTrackInfo_Value(reaper.BR_GetMediaTrackByGUID and tr or tr, "IP_TRACKNUMBER"))
out("items", reaper.CountTrackMediaItems(tr2))
local tk = reaper.CountTrackMediaItems(tr2) > 0 and reaper.GetActiveTake(reaper.GetTrackMediaItem(tr2, 0))
out("take", tk ~= nil)
out("file", reaper.file_exists(os.getenv("HOME") or "/"), reaper.file_exists("/nonexistent/x"))
reaper.SetEditCurPos2(0, 2.5, false, false)
out("cur", reaper.GetCursorPosition())
out("ts", reaper.GetProjectTimeSignature2(0))
out("bad", reaper.SetTrackStateChunk(tr2, "garbage", false), reaper.GetTrackName(tr2))
out("after_bad", reaper.CountTrackMediaItems(tr2), reaper.CountTrackEnvelopes(tr2))
if f then f:close() reaper.Main_OnCommand(40004, 0) end
