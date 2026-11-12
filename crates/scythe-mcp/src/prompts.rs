//! Prompts: tasks an agent can be handed in one go, written as the steps a careful engineer
//! would take, with the tools for each and the checks between them.

use serde_json::{Value, json};

/// Every prompt ends with this, so the person watching Scythe sees what the agent is up to.
const STATUS_NOTE: &str = "Before each step, say in a few words what you are doing with set_status (such as \
    \"Balancing drums against the reference\"); Scythe shows it to the person at the session. Clear it \
    with an empty text when you are done.";

pub fn list() -> Value {
    json!([
        {"name": "mix_check", "title": "Mix check",
         "description": "Inspect levels, routing and effects of the open project and report problems.",
         "arguments": [{"name": "focus", "description": "What to look at most, such as \"vocals\" or \"the drum bus\".", "required": false}]},
        {"name": "monitor_mix", "title": "Monitor mix change",
         "description": "Change a musician's in-ear monitor mix the way they ask.",
         "arguments": [
            {"name": "musician", "description": "Whose mix, such as \"Drummer\".", "required": true},
            {"name": "request", "description": "What they asked for, such as \"more bass, less keys\".", "required": true}]},
        {"name": "record_setup", "title": "Recording setup",
         "description": "Check and prepare tracks, inputs and monitoring for recording.",
         "arguments": [{"name": "tracks", "description": "The tracks to record, such as \"Vox and Gtr\".", "required": false}]},
        {"name": "build_live_session", "title": "Build a live session",
         "description": "Build a complete live (in-ear monitoring) session step by step: inputs, tracks, amp sims, dynamics, groups, hardware outputs, monitor mixes, click, setlists, access links and a footswitch, checking each step.",
         "arguments": [
            {"name": "band", "description": "Who plays what and on which input, such as \"Vox on 1, Gtr DI on 2 (NAM), Bass DI on 3, kick 4, snare 5, overheads 6/7\".", "required": true},
            {"name": "outputs", "description": "The interface's outputs and who listens where, such as \"3/4 drummer IEM, 5/6 bass IEM, 7 click to drummer only\".", "required": false},
            {"name": "backing", "description": "Backing-track and click files and where they are, such as \"/gigs/2026/backing/*.wav\".", "required": false},
            {"name": "setlist", "description": "The set in order, one song per line or comma-separated.", "required": false}]},
        {"name": "setlist_from_backing_tracks", "title": "Setlist from backing tracks",
         "description": "Import backing-track and click files, find where each song starts and stops, place song and stop markers, and make a setlist from them.",
         "arguments": [
            {"name": "files", "description": "The files: one per song, or long files with several songs separated by silence.", "required": true},
            {"name": "songs", "description": "The song names in order, if known.", "required": false}]},
        {"name": "match_reference", "title": "Match a reference",
         "description": "Bring the mix closer to a reference track's tonal balance, loudness and dynamics in small, explained, measured steps.",
         "arguments": [
            {"name": "reference", "description": "The reference audio file on the machine running Scythe.", "required": true},
            {"name": "target", "description": "What to change: \"master\" (default), a bus or a track.", "required": false},
            {"name": "range", "description": "The part to compare, such as \"60-120\" seconds of the mix (the chorus); the whole project when left out.", "required": false}]},
    ])
}

pub fn get(params: &Value) -> Result<(String, String), String> {
    let argument = |name: &str| params["arguments"][name].as_str().filter(|s| !s.trim().is_empty());
    let (description, text) = match params["name"].as_str() {
        Some("mix_check") => {
            let focus = argument("focus").map(|f| format!(", paying most attention to {f}")).unwrap_or_default();
            let text = format!(
                "Check the mix of the open Scythe project{focus}. Read the resource scythe://docs/mixing first: \
                 field notes on what to measure and what usually goes wrong. Start with get_project; use fx_list, \
                 fx_params and send_list where you need more detail. Look for: tracks muted or soloed by \
                 accident; faders far above 0 dB or at -inf; pan at the extremes; sends to muted tracks or \
                 at odd levels; effects bypassed or not running; tracks left armed or monitoring their \
                 input; tracks without items; folder structure that hides tracks; effects and level on \
                 the master. Report findings as a short list naming tracks by name and id, then propose \
                 concrete changes as tool calls, but make none until I agree."
            );
            ("Inspect levels, routing and effects", text)
        }
        Some("monitor_mix") => {
            let musician = argument("musician").unwrap_or("the musician");
            let request = argument("request").unwrap_or("a better balance");
            let text = format!(
                "{musician} asks for this in their monitor mix: \"{request}\". Find their mix with mix_list \
                 (create one with mix_create only if there is none, and tell me). Change it with \
                 mix_level, mix_pan, mix_mute or mix_more_me in small steps, at most 3 dB at a time, \
                 and never touch the main mix (track faders) or other musicians' mixes. Then list what \
                 you changed, from and to, so it can be undone."
            );
            ("Adjust a monitor mix", text)
        }
        Some("record_setup") => {
            let tracks = argument("tracks").map(|t| format!(" for {t}")).unwrap_or_default();
            let text = format!(
                "Prepare the open Scythe project for recording{tracks}. Start with get_project. For each \
                 track to record: arm it, check its record input (track_input) and whether input \
                 monitoring should be on; disarm tracks that should not record. Check the metronome and \
                 where the edit cursor is, since recording starts there. Tell me what you set and what I \
                 still need to check by ear; do not start recording yourself."
            );
            ("Prepare tracks for recording", text)
        }
        Some("build_live_session") => {
            let band = argument("band").unwrap_or("(ask me who plays what on which input)");
            let outputs = argument("outputs").unwrap_or("(ask me which outputs feed which in-ears)");
            let backing =
                argument("backing").unwrap_or("(none given; ask whether there are backing tracks or a click file)");
            let setlist = argument("setlist").unwrap_or("(none given)");
            let text = format!(
                "Build a live in-ear monitoring session in the open Scythe project, one step at a time. \
                 After every step check the result (get_project, or the list tool of what you changed) and \
                 fix what is wrong before going on; tell me briefly what each step did. Never start the \
                 transport playing and never change levels by more than 6 dB at once. Save with \
                 project_save only when I say so.\n\n\
                 The band and inputs: {band}\nOutputs and listeners: {outputs}\nBacking tracks and click: \
                 {backing}\nSetlist: {setlist}\n\n\
                 1. Look first: get_project and events_meters (the device's health: inputs, xruns). If \
                 the project already has tracks, ask before rebuilding.\n\
                 2. Input tracks: track_add per source, named plainly (\"Vox\", \"Gtr\"); track_input for \
                 its input (0-based mono input n, 1024+n for a stereo pair n/n+1); arm with input \
                 monitoring through scythe_command {{\"cmd\": \"track.arm\"}} and \
                 {{\"cmd\": \"track.monitor\"}} so they are heard live. Check with get_project.\n\
                 3. Amp sims and effects: fx_catalog with a query (\"nam\", \"compressor\", \"gate\", \"eq\") to \
                 find plugins; fx_add per track: Neural Amp Modeler (NAM) on guitar and bass DIs when \
                 installed, a gate on toms and kick if bleeding, a compressor on vocals and bass, an EQ \
                 high-pass on everything but kick and bass. Set parameters with fx_params / fx_param; \
                 keep a limiter (builtin:limiter) at the end of every in-ear mix later.\n\
                 4. Groups and buses: folder tracks for drums and backing tracks (track_folder), track \
                 groups through scythe_command group.set for things moved together.\n\
                 5. Hardware outputs: send_hw_add for front-of-house or splits that bypass the mixes.\n\
                 6. Monitor mixes: mix_create per musician with their own sources (`own`), then \
                 mix_level per source, mix_output to their output pair, the mix's limiter on; mix_click \
                 or a click track routed only to who needs it. Read back with mix_get.\n\
                 7. Backing tracks and click: media_probe each file, media_import onto a \"Backing\" and a \
                 \"Click\" track (sequence: true for one file per song), mute the click's main send so the \
                 audience never hears it (send it to the monitor mixes only). For long files use \
                 analysis_segments to find the songs; follow the setlist_from_backing_tracks steps.\n\
                 8. Song markers: one marker per song start named after the song and a `!1016` stop \
                 marker where each song ends (marker_add); check with setlist_songs.\n\
                 9. Setlists: setlist_create with the songs in order; setlist_status to check.\n\
                 10. Access links: scythe_command access.create for each musician (role musician) and the \
                 stage tablet (role stage); give me their `urls` (the QR code payloads).\n\
                 11. Footswitch: set it up through scythe_command surface.* or learn (ask which pedal and \
                 what it should do: next song, play/stop).\n\
                 12. Finish with events_since to make sure nothing failed on the way, and a short summary \
                 of the session: tracks, mixes, outputs, songs, links, and what I should check by ear."
            );
            ("Build a live session step by step", text)
        }
        Some("setlist_from_backing_tracks") => {
            let files = argument("files").unwrap_or("(ask me for the files)");
            let songs = argument("songs").map(|s| format!("The songs, in order: {s}.")).unwrap_or_default();
            let text = format!(
                "Make the open project play these backing tracks as a setlist: {files}. {songs}\n\
                 1. media_probe every file: length, sample rate, loudness. Warn me about files at another \
                 sample rate than the project's or far louder than the others.\n\
                 2. media_import them onto one track (\"Backing\"; a click file onto \"Click\"), one file per \
                 song with `sequence: true` and a gap of a few seconds, or a long file as it is.\n\
                 3. For each long file (or the click), analysis_segments with the item's id to find \
                 where each song starts and ends; check the count against the songs I named and adjust \
                 `min_silence` or `threshold_db` if songs run together or split. A click's segments \
                 give each song's tempo (bpm).\n\
                 4. Place markers: a song marker at each start, named after the song, and a `!1016` stop \
                 marker just after each end (analysis_segments with `markers: true` does both, as \
                 \"Song n\"; rename them with scythe_command marker.edit). Each song must end at a stop \
                 marker before the next one starts.\n\
                 5. setlist_songs to check the songs Scythe sees, then setlist_create with them in order \
                 and setlist_status.\n\
                 6. Report the songs with start, length and tempo, and anything that looked odd."
            );
            ("Setlist from backing tracks", text)
        }
        Some("match_reference") => {
            let reference = argument("reference").unwrap_or("(ask me for the reference file)");
            let target = argument("target").unwrap_or("master");
            let range = argument("range").map(|r| format!(" over {r} seconds of the project")).unwrap_or_default();
            let text = format!(
                "Bring the mix closer to the reference {reference}, changing {target}{range}. Nothing \
                 plays while you work: every measurement is an offline render. The reference is \
                 measured on this machine only. The resource scythe://docs/mixing has field notes on \
                 measuring and fixing a mix in Scythe; read it first.\n\
                 1. analysis_file on the reference (its job number lets you reuse it as {{\"job\": n}}).\n\
                 2. analysis_mix of {target} over the same musical part, then analysis_compare (reference \
                 against mix). Read the summary: loudness, peak-to-loudness ratio, tonal regions, width, \
                 the low end's mono check.\n\
                 3. Tonal balance: analysis_match_eq with the reference and the mix, max_gain_db 4 and \
                 max_bands 5. Explain each proposed band in words (\"+2 dB low shelf at 100 Hz: the \
                 reference has more low end\"). Apply it with `apply`: \"{target}\" (one undo step, a \
                 new ReaEQ), or leave it out if the differences are under 1 dB.\n\
                 4. Loudness and dynamics: follow the `dynamics` advice of match_eq only as far as it \
                 is safe: at most 6 dB more limiter drive per step, a ceiling of -1 dBTP, a compressor \
                 only for a peak-to-loudness ratio much above the reference's. Add effects with fx_add \
                 and set them with fx_param; say what each does.\n\
                 5. Render and measure again (analysis_mix, analysis_compare with the reference's job). \
                 Stop when the loudness is within 1 LU, the true peak at or below -1 dBTP, and every \
                 tonal region within 1.5 dB; or after three rounds; or when a change makes things worse \
                 (undo it with project_undo).\n\
                 Never boost a band more than 6 dB in total, never raise a fader above +6 dB, never \
                 remove effects the mix had. End with a table of what you changed and the numbers before \
                 and after, and say what to judge by ear (a measurement cannot tell taste)."
            );
            ("Match a reference", text)
        }
        Some(other) => return Err(format!("no prompt \"{other}\"")),
        None => return Err("give the prompt's \"name\"".into()),
    };
    Ok((description.to_owned(), format!("{text}\n\n{STATUS_NOTE}")))
}
