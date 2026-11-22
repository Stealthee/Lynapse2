# Changelog

## 2.1.0 (2026-11-21)

### New
- **Razer BlackShark V3 support** (the non-Pro model): full support including the tray EQ menu. Contributed in
  pull request #1 by NoMacSec.
- **Spatial Audio from your own Synapse install (optional).** If you dual-boot with Razer Synapse,
  `python3 dsp/synapse/build.py /run/media/$USER/<windows-drive>` builds a Spatial Audio filter from the files
  Synapse installed for your headset: its measured HRTF (7,414 directions), the early reflections and late reverb of
  your active Synapse room, and the Spatial Audio demo clip. It prints the install command with the right level.
  Nothing from Razer is included in this repository; see [dsp/synapse/README.md](dsp/synapse/README.md).
  Without it, Spatial Audio keeps using the free KEMAR HRIR as before.
- **New Demo page.** It has a live speaker map that shows where each sound comes from (including the height
  speakers) and a **Stereo / Spatial Audio** switch to compare the two while the clip plays. It uses the demo clip built
  above and falls back to the original 2.0 demo if that isn't there.

### Improved: Audio and Mic Enhancement now use Synapse's own settings
These used to be estimated from measurements of Synapse. They now use Synapse's actual settings, checked against
the same Windows recordings (about 0.5-1 dB).
- **Sound Normalization** (headphones, the "footsteps boost") and **mic Volume Normalization**: Synapse's compressor
  settings for every slider position. Mic normalization now has Synapse's slow, smooth 1.2-2.7 s release.
- **Voice Gate**: Low / Medium / High = -80 / -55 / -30 dB, opening over 100 ms and closing in 10 ms, like Synapse.
  Just above the threshold it also turns quiet sounds down a little, as Synapse does.
- **Mic Vocal Clarity**: Synapse's 10-band EQ (cuts the lows, lifts the voice range and the top end).
- **Mic Noise Cancellation**: Synapse's expander (it is not an AI noise filter), with the threshold following the
  slider from -60 dB to -16 dB.
- Bass Boost and headphone Voice Clarity are unchanged (they already matched within about 0.1 dB).

### Fixed
- **Ultra Low Latency** turned itself back on after the BlackShark V3 Pro was power-cycled or reconnected, causing
  audio dropouts. Lynapse now restores your saved setting.
- **Reinstalling** while Lynapse was running force-killed it, which made systemd mark the autostart entry as failed.
  The installer now asks the app to quit first.

### Notes
- The first time you turn on Audio or Mic Enhancement after updating, PipeWire restarts once to load the updated
  audio plugin. Reload any browser tab that was playing.
- README: new Supported devices section and screenshots.

## 2.0.0 (2026-11-03)
First release of Lynapse 2.0: Synapse-style Razer settings for Linux.
