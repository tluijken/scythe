# Scythe on stage

How to run Scythe as a band's in-ear-monitor and playback rig: a machine in the rack that
starts on its own, plays setlists, gives every musician their own mix and the drummer a click,
and keeps going when something goes wrong.

## The session

`scythe-server` runs a session without a window (see the README's "Headless" section and
[control-api.md](control-api.md)). Put the settings in `~/.config/scythe/headless.toml`:

```toml
project = "/home/band/live/set.RPP"
control = "0.0.0.0:8770"
token = "change-me"

[audio]
output = "Scarlett 18i20"
input = "Scarlett 18i20"
sample_rate = 48000
buffer_size = 128
output_channels = 18
```

Musicians open `http://rack:8770/monitor` on their phone for their own mix, the drummer
`http://rack:8770/setlist` on a tablet; the desktop app or the web editor (`/`) does the rest.

## Running as a service

```sh
scythe-server --install-service --config ~/.config/scythe/headless.toml
systemctl --user daemon-reload
systemctl --user enable --now scythe-server.service
sudo loginctl enable-linger $USER        # start at boot, before anyone logs in
journalctl --user -u scythe-server -f    # its log: notices, lost devices, xruns
scythe-server --status                   # device, buffer, CPU, xruns, clients
```

`--install-service` writes `~/.config/systemd/user/scythe-server.service` running the same
program with the arguments given (the project and `--config` made absolute), restarting it two
seconds after a crash. On stop (SIGTERM) the session writes unsaved changes to the project's
`Backups` folder before it exits; the unit allows 20 seconds for that.

Without lingering, a user's services start when they log in and stop when they log out;
`loginctl enable-linger` keeps the user's service manager running from boot. With PipeWire or
PulseAudio as the audio host, their user services start the same way; with plain ALSA or JACK
nothing else is needed.

### Real-time audio

The thread that renders the audio (the device callback, or the silent clock without a device)
asks for real-time scheduling when it starts: `SCHED_FIFO` priority 70 when the user's rtprio
limit allows it (capped at the limit), otherwise the `SCHED_RR` priority RealtimeKit (`rtkit-daemon`, which
desktop distributions and PipeWire setups run) grants over the system D-Bus, usually priority
20. Without either the session plays on at normal priority and says so once in its log.
`--status` (and the `health` event's `realtime`) shows what it got, e.g.
`scheduling: SCHED_RR 20 (rtkit)`. Through RealtimeKit the process gets a CPU-time limit
(`RLIMIT_RTTIME`, 200 ms by default): a block that renders that long without a break ends the
process, which the service then restarts.

The unit asks for real-time priority (`LimitRTPRIO`, at most 95) and unlimited locked memory
(`LimitMEMLOCK`) only as far as the user's hard limits allow when it is written, because a user
unit asking for more than that does not start at all. To raise them, put the user in the
`audio` group and give the group the limits, then log in again and reinstall the unit:

```
# /etc/security/limits.d/95-audio.conf
@audio - rtprio 95
@audio - memlock unlimited
```

On NixOS: `security.pam.loginLimits` with the same entries, or `musnix`. Run `--status` during
a rehearsal: the CPU figure is the time spent rendering against the time a buffer lasts; above
roughly 70 % the buffer is too small for the project, and xruns say the audio dropped out.

## Click and count-in

The metronome's settings are REAPER's and live in the project (right-click the metronome
button, or `transport.metronome`, `transport.metronome_output` and `transport.count_in`):

- **Count-in** before playback and/or recording, in measures, at the tempo and time signature
  where playback starts, as long as the metronome is on (as in REAPER: with it off there is no
  count-in). The playhead waits at the start while it clicks, on the song's own beats carried
  back from there; the transport event counts `count_in.position` up to 0 with the beat
  number, which the setlist page shows. "Start count-in at start of measure" counts in from
  the bar line before a song that starts mid-bar.
- **Pre-roll** before playback and/or recording starts that many measures earlier (on a bar
  line by default); a recording keeps only what comes after the start. Pre-roll, "start
  count-in at start of measure" and the default metronome output are preferences, as in
  REAPER's reaper.ini, kept in `$XDG_CONFIG_HOME/scythe/preferences.ini` (`transport.preroll`,
  `transport.count_in` with `measure_start`, `transport.metronome_output` with `default`).
- **Per setlist**, "Count-in" makes every song start after the count-in, whatever the project
  setting and even with the metronome off, so the drummer does not have to count the band in.
- **Where the click goes.** By default the metronome plays with the master, which is the PA.
  Point it at the drummer's in-ear output instead (metronome output, REAPER's
  `METRONOMEOUT`), and/or give monitor mixes a click of their own: the click fader on the
  `/monitor` page or `mix.click` puts the metronome and every count-in into that mix, before
  its limiter, at that musician's level. Set the metronome's own level to -inf (or point it at
  an unused output) and only the mixes with a click hear it.
- Turning the metronome off silences the click during songs and drops the project's count-in;
  a setlist's count-in still clicks. For a click during count-ins only, leave the metronome on
  and turn off "run during playback" in the project (REAPER's metronome settings).

## When things go wrong

- **The audio interface is unplugged or crashes.** The session keeps running on a silent
  clock: the transport, setlist, monitor pages and control API carry on, a notice says the
  device was lost, and the `health` event's `status` turns `silent`. Every five seconds it
  looks for the device again and, once it is back, plays through it and reopens the inputs
  (another notice; `reconnects` counts it). An interface that is not there when the session
  starts is picked up the same way.
- **Xruns.** The `health` event (and the web editor's status bar, the desktop app's status
  pill, `--status`) counts the underruns the device reports and the callbacks that took too
  long; the service log gets a line when they go up.
- **Unsaved changes.** Every five minutes changes are written to the project's `Backups`
  folder (`<name>-<date>_<time>.rpp-bak`). While a song plays the backup waits for the stop,
  up to fifteen minutes; during a recording it waits until the recording ends, since the take
  streams to the same disk. Only the newest 50 backups of a project are kept. The project file
  itself is only written by Save.
