# Mixing with Scythe: field notes

What worked, what did not, and how to tell the difference, from mixing and mastering a modern
metal song (DI guitars and bass through NAM, a multi-mic drum kit, screamed and sung vocals,
piano and strings) against a commercial reference album over several dozen measured rounds.
Agents get this page as the MCP resource `scythe://docs/mixing`; the
[control API](control-api.md) has the commands.

The short version: measure against a reference, change one thing at a time, render and measure
again, and let the band judge by ear. Numbers find problems; they do not decide taste.

## Working method

- **Reference first.** Pick one or two tracks of the record the band wants to sound like and
  compare against them all along (`analysis.file`, `analysis.mix`, `analysis.compare`). Compare
  the same kind of section: the loud chorus against the loud chorus.
- **Version every render** (`render.settings` `pattern`, a number per version) and keep a short
  changelog per version. Musicians answer "v15 had more balls, v17 more sub": you need to know
  what differed.
- **Small steps, one subject per version.** When a version is judged worse, go back to the last
  good one for that part instead of piling fixes on top.
- **Keep alternatives reversible** without stacking items: see the pitfalls below. Rig snapshots
  (`rig.snapshot`) are the clean way to keep amp variants.
- **Ask what a vague word means.** "Older", "warmer", "less dry" have several readings. "Old
  piano" turned out to mean "an old gramophone record", not "a horror-film hall"; a reference
  link from the musician settles it fastest. Measure the reference they give.
- **Say what you could not do.** Automatic timing correction of a loose bass part failed (see
  below); saying so and offering targeted fixes beat shipping edits that made it worse.

## Measuring

| What | How | Notes |
|---|---|---|
| Loudness, peak-to-loudness | `analysis.mix` / `analysis.file`: LUFS, PLR, true peak | Modern metal masters sit around -8 LUFS with a PLR of about 8 |
| Tonal balance | `analysis.compare`, regions sub / low end / low mids / mids / presence / air | Within ~1 dB per region of the reference is a good match; RMS over the bands under 0.7 dB is very close |
| Guitar tone | the side channel's spectrum against the reference's | Hard-panned double-tracked guitars dominate the side channel, so it compares guitars without the drums and vocals in the way |
| Kick punch | per 10 ms: level under 120 Hz (90th percentile) minus level above 1 kHz (median) | The reference measured about +6.7 dB. More sub raises it too, so read it together with the sub region |
| Pumping | correlation between the under-120 Hz and the over-1 kHz envelopes per 10 ms | Strongly negative means the low end ducks everything else (a limiter or clipper reacting to the kick). The reference stayed between -0.12 and +0.08 |
| Masking | solo-render each instrument group, then per 100 ms and third-octave band the share of time a group's important bands lie more than 3 dB under another group's | Shows who buries whom, and where (bass over guitars in 160-400 Hz, snare crack under guitars and cymbals in 2.5-4 kHz). Frame averages understate short transients like a snare hit |

`analysis.mix` spectra are normalised: compare shapes and use LUFS for level. A measurement
renders from the start of its range, so whatever happens at render start lands in it (see
pitfalls). Read a render's sample rate from the file header before indexing samples by time.

## Master and loudness

- A ReaLimit-style limiter alone could not reach the reference's loudness without either barely
  moving (slow release) or crunching cymbals (fast release). What worked: a multiband stage on
  the lows, a clipper (LSP Clipper) for the transients, then a limiter (LSP Limiter) for the
  rest. Turn off LSP Limiter's automatic level regulation and LSP Clipper's LUFS limitation,
  overdrive protection and boosting, or they undo the drive.
- A clipper on the drum and bass buses as well takes the kick and snare peaks before the master
  has to.
- Match the reference's tonal balance with a master EQ built from `analysis.match_eq` against
  two reference tracks, averaged, entered as bands by hand (`apply` would add it after the
  limiters). Re-check it after every big change upstream: a snare sample or a bass cut shifts
  the whole balance.

## Kick and bass

- **One low end, not two.** Send the kick mics into the bass bus (main send off) and put a
  multiband compressor there whose lowest band (under ~150 Hz) only works when the kick hits:
  kick and bass then share the low end instead of fighting for it.
- **Bass in two layers**: a clean low path (a SansAmp-style NAM capture, high cut around 1 kHz)
  and a grit path (a Darkglass-style capture, high-passed around 700 Hz).
- **Low mids**: in a dense mix the bass was the loudest group in 160-400 Hz and buried the
  rhythm guitars two thirds of the time. A 3 dB cut around 250 Hz on the bass bus, before its
  clipper, halved that. An EQ cut on the bass DI did almost nothing: the limiter after it
  pulled the level back up. Cut after the dynamics that would undo it.
- **Kick samples**: blend a trigger sample under the kick mics for the click and a controlled
  low end. Line its click up with the close mic (within a millisecond), test polarity on the
  bus (keep the polarity with more energy under 100 Hz), and shorten the sample's tail
  (full level for ~30 ms, then a ~25 ms decay): the punch then comes from the hit, not from sub
  that hangs on. A long sample tail raised the sub region 3-5 dB over the reference.

## Snare

- A recorded snare's crack (2.5-4 kHz) sat ~18 dB under its body (~200 Hz); guitars and cymbals
  covered it nearly all the time. EQ helps a little; a sample does more.
- **Snare reinforcement**: detect hits on the top mic (a 10 dB rise within 20 ms, at most 15 dB
  under the loudest hit) and confirm them on the bottom mic (the wires): that rejects hi-hat and
  kick bleed. A simpler detector found only half the hits in fast passages, so the sample came
  and went ("sometimes it's there, sometimes not"). Count hits against the bottom mic before
  trusting a trigger.
- Pick a velocity layer per hit from the hit's level, keep the sample well under the real snare
  (it reinforces, it does not replace), and keep some of its body: a crack-only sample sounds
  artificial.
- With sidechain routing (below) the snare can key a short dip in the guitars' crack range.

## Drums as a kit

- **Phase**: measure the delay of overheads, room and back mics against the snare top (cross-
  correlation in 150-1000 Hz). Back and trash mics facing the kit are often in opposite
  polarity; flip them. Align overheads and close rooms to the snare (1-3 ms) to remove comb
  filtering at 150-400 Hz; leave a distant room late, that is its job.
- **EQ per element**: snare less body boost and a box cut around 500 Hz, crack at 3.5 kHz; toms
  box cut at 300-400 Hz and attack at 4 kHz, low cut against kick bleed; overheads high-passed
  around 300 Hz for cymbals and image; rooms and back mics a cut around 320 Hz.
- **Gates**: closing tom and kick gates completely sounds "gated". Let them fall to about
  -18 dB instead of silence, with a longer hold and release, and bring the room mics up a little
  so the kit sounds like one instrument.

## Guitars (NAM)

- **Level-match before comparing captures**: captures differ by 20 dB and more. Scythe brings
  models to -18 dB loudness from their metadata; models without loudness metadata play as they
  are.
- **Check that a capture has a cab**: a capture without one (often unlabelled) sounds like a
  telephone: little low end and fizz on top. Enable a cab IR on it.
- **Blending**: two captures in parallel on the same DI comb-filter (a 4-5 dB dip around
  250-400 Hz). Different captures on the left and right takes blend cleanly because the takes
  are uncorrelated.
- **Side-only EQ** on the guitar bus shapes the hard-panned guitars without touching the centre
  (vocals, kick, snare, bass).
- **Leads**: their own capture and cab, some warmth (a little low-mid, less around 2 kHz, a
  lower high cut), and delay and reverb on a return with the reverb on the echoes only. Keep
  the return well under the dry lead; matching it to the dry level was far too much.
- **Trackspacer**: send the keys into channels 3/4 of the guitar bus (`track.channels`,
  `send.channels`) and give the bus a Scythe EQ dynamic band (`external_sidechain`, broad, a
  few dB range) where the keys live. The guitars then make room only while the keys play.

## Vocals

- **Delay you can hear**: a delay that only feeds a reverb is not heard as echoes. Put the delay
  in parallel with the reverb on the return (`fx.parallel`), a quarter note at the song's tempo,
  two or three repeats, low and high cut, around 13 dB under the vocal bus.
- **Doubles**: time-align a double to the lead by dynamic time warping on log band energies, a
  slope limit (stretch 0.75-1.4) and a time map for Rubber Band (`-M`). Check the result per
  phrase afterwards: the warp can drift in short silences and pull the next phrase early, which
  a whole-song average hides.
- **Lo-fi lines** (a whisper through a transistor radio): a steep band-pass around 600 Hz-3 kHz
  plus a clipper, switched on for the passage by automating the plugins' bypass, and pan
  automation to place each repetition left, right and centre. Band-passes lose a lot of level:
  make it up before the clipper, or it stops distorting.

## Effects for character

- **Old gramophone**: mono, 250 Hz-1.6 kHz with a steep top roll-off, slight distortion, and
  unsteady speed: pitch and timing moving together (a swing per revolution, slow irregular drift,
  a little flutter; ±18-36 cents) by resampling the audio with a zero-mean speed curve, so it
  lands back on time. A chorus as "wow" sounded like a chorus. For the surface noise, real
  noise from a record's run-in or run-out groove beat anything synthesised; build a longer bed
  from it with overlapping random grains, and check that the spectrum matches.
- **Contrast**: an old-sounding intro that switches to the full modern sound where the band
  comes in made the entry hit harder than a gradual transition.

## Timing edits

- Automatic timing correction of a bass or guitar DI by onset detection was not reliable: low
  notes produce a "new onset" every period, and fast riffs make every note match its neighbour
  as well as its partner. Even two tracks of the same guitarist did not line up in the
  measurement. Edit only spots the musicians point at.
- Vocal doubles align well (above); the lead vocal's timing is part of the performance.

## Pitfalls in Scythe (as of November 2026)

- Items that overlap on one track crossfade over the whole overlap, even when one is muted:
  do not stack muted alternatives on an active item, put them on another track. A
  crossfade ends when the overlap does; `item.fade` with `"auto_length": 0` clears one by hand.
  Check a project with `grep 'FADE\(IN\|OUT\) [0-9.]* [0-9.]* [1-9]'` on the RPP.
- `track.phase` toggles unless given `"phase"` (or `"invert"`) `true|false`; like most
  commands it ignores fields it does not know, so a misspelt field toggles too. Read the answer.
- The first `fx.param` write to a CLAP plugin after a pause can be ignored: write until the
  answer shows the value. Many LSP parameters are normalised 0..1 (find values by bisecting on
  the answer's text); gains and thresholds use real units. Save before rendering after plugin
  changes.
- LSP Filter's "APO" filter mode ignores width and slope.
- NAM rigs give a short burst at the start of every render; keep the guitar and bass buses
  muted for the first few tenths of a second with a volume envelope.
- Recalling a rig snapshot replaces unsaved changes to the live rig: store first.
- `track.add` with an `index` inside a folder puts the new track in that folder; place it with
  `track.move` and `parent`, and check that muting a bus changes the master.
- Overwriting a media file under the same name can leave Scythe playing the cached old one:
  write a new name.
