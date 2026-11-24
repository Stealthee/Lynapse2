# Changelog

## 2.1.1 (2026-11-24)

### New
- **BlackShark V3 Pro works without the OpenRazer driver.** On kernels that only load signed modules (for example
  CachyOS's EAC kernel for anti-cheat games), the OpenRazer driver can't load. Lynapse now talks to the headset
  directly instead, and every setting works the same: EQ presets and custom curves, mic EQ, sidetone, ANC/ambient,
  Ultra Low Latency, Spatial Audio, Game/Chat, in-call mix, audio prompts, FN button, battery and charging. The
  installer adds a small udev rule for this (it asks for your password once). With the driver loaded, nothing changes.
  Command line: `razerctl --hid` lists every setting, `razerctl --hid <setting> <value>` changes one.
- **Game/Chat balance lock.** A **Lock balance** switch under the Game/Chat Balance slider (Sound page). While it is
  on, turning the headset wheel no longer changes the balance: Lynapse puts it back right away and shows a pop-up
  saying the balance is locked. The slider and the tray menu still change it.
- **Update notice.** Lynapse tells you when a new version is released: a notification with **Copy update command**,
  **What's new** and **Ignore this version** buttons, plus an entry in the tray menu. Ignoring is remembered for that
  version only. Switch it off under Settings → Check for updates. It only asks GitHub for the latest release number.
- **`./update.sh`** gets the latest version, rebuilds, reinstalls and restarts Lynapse, keeping your settings,
  autostart choice and Spatial Audio filter.
- **What's New** on the start page shows the changes of the last 5 versions.

### Fixed
- Two places on the start page still said "Lynapse 2.0".
- BlackShark V3 Pro, OpenRazer driver: **Spatial Audio** and **audio prompts** showed as off after every connect,
  **Game/Chat** could show the wrong balance, and the **firmware version** often showed v1.0. These are driver bugs;
  the fixes are in the driver branch `v3pro-readback-getters` at
  [Stealthee/openrazer-v3pro-custom-driver](https://github.com/Stealthee/openrazer-v3pro-custom-driver) and show up
  in Lynapse once you update the driver. The new driver-free mode above reads all four correctly.

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
