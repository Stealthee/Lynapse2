# Spatial Audio from your own Synapse install (optional)

Lynapse's Spatial Audio works out of the box with the free KEMAR HRIR. If you also use **Razer Synapse on Windows**
(dual boot) with a headset that has Spatial Audio, you can build a much closer match to how it sounds there, from
the files Synapse already installed on your Windows drive:

- the **measured HRTF** your headset's Spatial Audio driver uses (7,414 directions),
- the **room** from your active Synapse room preset: early reflections off the floor, walls and ceiling, plus the
  late reverb, re-created with the same algorithm and settings,
- the **Spatial Audio demo clip** for the app's Demo page (speaker map + Stereo / Spatial Audio switch).

Nothing from Razer is included in this repository. These scripts only read **your own** copy. Keep the generated
files to yourself; don't upload or share them.

## Requirements

- A Windows install where Synapse has set up Spatial Audio for your headset at least once (connect the headset and
  switch Spatial Audio on in Synapse). Open the Spatial Audio demo in Synapse once if you want the demo clip.
- That Windows drive mounted on Linux (your file manager mounts it when you click it, usually under
  `/run/media/$USER/<label>`). It is only read, never written.
- `python3` with `numpy`, and `ffmpeg` (for the demo clip).

Tested with the BlackShark V3 Pro. Other headsets with Synapse Spatial Audio probably use the same file format. The
script checks the data and stops if anything doesn't match.

## Build and install

```sh
cd Lynapse2
python3 dsp/synapse/build.py /run/media/$USER/<windows-drive>
```

It finds the files by itself, checks them, and writes:

| file | what |
|---|---|
| `~/.local/share/razerctl/synapse/lynapse-room.wav` | 14-channel HeSuVi HRIR: measured HRTF + room reflections + reverb |
| `~/.local/share/razerctl/spatial-demo/` | the Demo page clip (Spatial Audio + plain stereo versions, speaker-map data) |

At the end it prints the install command with the right level for that file, for example:

```sh
./install-with-spatial.sh --hrir ~/.local/share/razerctl/synapse/lynapse-room.wav --gain 1.966 --shelf 0
```

That replaces the Spatial Audio filter (your old config is kept as `60-razerctl-thx.conf.bak`) and restarts PipeWire.
Reload any browser tab that was playing afterwards. The Demo page switches to the new demo by itself once its files
exist.

**Going back to KEMAR:** run `./install-with-spatial.sh` again with no extra options.

## Options

```sh
python3 dsp/synapse/build.py --list-rooms <drive>      # room presets in your install (Synapse has one per game)
python3 dsp/synapse/build.py --room <id> <drive>       # build a different room instead of the active one
python3 dsp/synapse/build.py --device <name> <drive>   # pick a headset if several are installed
python3 dsp/synapse/build.py --data FILE --presets FILE [--demo FILE]   # use copied files instead of a drive
```

## How it works

- `hrtf_set.py` reads the driver's HRTF data: 7,414 measured directions, each a 512-point packed FFT per ear
  plus the per-ear arrival delays. It blends the three measured directions around each virtual speaker.
- `room.py` re-creates the room stage. The early reflections are a shoebox image-source model (room size and
  reflection count from the preset, floor 1.07 m below the ears, cardioid speakers facing you). The late reverb is
  a Freeverb variant fed with the sum of all channels. The preset's EQ bands are RBJ biquads.
- `demo.py` renders the 12-channel (7.1.4) demo through all of that for headphones, plus a plain stereo version for
  the comparison switch.

The output is a plain convolution file, so it runs in PipeWire's built-in convolver like any other HRIR. Not
covered: head tracking, and EQ curves applied outside the room (Synapse's per-profile EQ). Add those in your own
filter config if you like.
