# Changes to cpal

This is [cpal](https://github.com/RustAudio/cpal) 0.18.2 (Apache-2.0, see `LICENSE`), changed
by the Scythe authors as follows:

- `src/host/alsa/mod.rs`: the number of periods in the ALSA ring is settable
  (`set_periods`, 2 to 32) instead of the fixed `DEFAULT_PERIODS`, and the period and ring size
  the last playback stream got can be read back (`negotiated`).
- `src/lib.rs`: re-exports both as `set_alsa_periods` and `alsa_negotiated` on the ALSA hosts.
