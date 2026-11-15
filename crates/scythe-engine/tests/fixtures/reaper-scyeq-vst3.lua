-- Inserts the ScyEQ-Pro VST3 on a new track, gives it settings other than the defaults and
-- saves the project where OUT says: a project REAPER wrote with the plugin's own state.
local settings = {
  ["Band 1 Used"] = 1, ["Band 1 Shape"] = 2 / 9, ["Band 1 Frequency"] = 0.25, ["Band 1 Gain"] = 0.4,
  ["Band 2 Used"] = 1, ["Band 2 Frequency"] = 0.6, ["Band 2 Gain"] = 0.7, ["Band 2 Q"] = 0.5,
  ["Output gain"] = 0.55,
}
local out = os.getenv("OUT")
reaper.InsertTrackAtIndex(0, true)
local track = reaper.GetTrack(0, 0)
reaper.GetSetMediaTrackInfo_String(track, "P_NAME", "Vocals", true)
local fx = reaper.TrackFX_AddByName(track, "VST3: ScyEQ-Pro (Scythe)", false, -1)
local log = io.open(out .. ".log", "w")
log:write("fx " .. fx .. "\n")
for p = 0, reaper.TrackFX_GetNumParams(track, fx) - 1 do
  local _, name = reaper.TrackFX_GetParamName(track, fx, p)
  if settings[name] then
    reaper.TrackFX_SetParamNormalized(track, fx, p, settings[name])
    local _, shown = reaper.TrackFX_GetFormattedParamValue(track, fx, p)
    log:write(string.format("%d\t%s\t%.17g\t%s\n", p, name, settings[name], shown))
  end
end
log:close()
reaper.Main_SaveProjectEx(0, out, 0)
reaper.Main_OnCommand(40004, 0)
