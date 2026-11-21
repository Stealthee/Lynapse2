#!/usr/bin/env python3
"""Build Lynapse Spatial Audio files from YOUR OWN Razer Synapse install (see dsp/synapse/README.md).

    python3 dsp/synapse/build.py /run/media/$USER/WINDOWS          # mounted Windows drive (C:)
    python3 dsp/synapse/build.py --list-rooms /run/media/$USER/WINDOWS
    python3 dsp/synapse/build.py --room <preset-id> /run/media/$USER/WINDOWS

Writes (nothing is uploaded or shared; keep these files to yourself):
  ~/.local/share/razerctl/synapse/lynapse-room.wav   14-ch HeSuVi HRIR: HRTF + early reflections + late reverb
  ~/.local/share/razerctl/spatial-demo/*             the Demo page's Spatial Audio / Stereo clip (if found)
and prints the ./install-with-spatial.sh command that installs it.
"""
import argparse, fnmatch, json, os, sys
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hrtf_set, room   # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__)); REPO = os.path.dirname(os.path.dirname(HERE))
DATA = os.path.expanduser('~/.local/share/razerctl')

def ci_glob(root, *parts):
    """Case-insensitive path glob (NTFS mounts are case-sensitive on Linux)."""
    cur = [root]
    for part in parts:
        nxt = []
        for d in cur:
            try: names = os.listdir(d)
            except OSError: continue
            nxt += [os.path.join(d, n) for n in names if fnmatch.fnmatch(n.lower(), part.lower())]
        cur = nxt
    return sorted(cur)

def find(win, device):
    bins = ci_glob(win, 'Windows', 'System32', 'DriverStore', 'FileRepository', '*', '*_apo4_data.bin')
    if device: bins = [b for b in bins if device.lower() in os.path.basename(b).lower()]
    if not bins: sys.exit('No *_apo4_data.bin under %s/Windows/System32/DriverStore/FileRepository.\n'
                          'Install Synapse on Windows, connect the headset once and turn Spatial Audio on, then retry.' % win)
    if len({os.path.basename(b) for b in bins}) > 1:
        sys.exit('Several headsets found, pick one with --device:\n  ' + '\n  '.join(sorted({os.path.basename(b) for b in bins})))
    data = max(bins, key=os.path.getmtime)
    key = os.path.basename(data)[:-len('_apo4_data.bin')].split('_', 1)[-1]     # e.g. razer_<model>_pc
    packs = ci_glob(win, 'ProgramData', '*', '*' + key + '*', '*' + key + '*_props.dat') or \
            ci_glob(win, 'Windows', 'System32', 'DriverStore', 'FileRepository', '*', '*' + key + '*_props.dat')
    sel = ci_glob(win, 'ProgramData', '*', '*' + key + '*', 'endpoints', 'headphones_game', 'presets', 'preset_selection.json')
    demos = ci_glob(win, 'Users', '*', 'AppData', 'Local', 'Razer', 'RazerAppEngine', 'User Data', 'Apps', 'Common', 'audiodata', 'DemoVideo', '*.mov')
    return data, (packs[0] if packs else None), (sel[0] if sel else None), (demos[0] if demos else None)

def pink_energy(x, pairs, gain=1.0, shelf_db=0.0):
    N = 1 << 15; f = np.fft.rfftfreq(N, 1/hrtf_set.FS); band = (f >= 20) & (f <= 20000); pink = 1/np.maximum(f, 1)
    sh = 1.0
    if shelf_db:
        b, a = room.rbj('HIGH_SHELF', 2500.0, shelf_db, 0.707); z = np.exp(-1j*2*np.pi*f/hrtf_set.FS)
        sh = np.abs((b[0] + b[1]*z + b[2]*z*z)/(1 + a[1]*z + a[2]*z*z))**2
    P = sum(np.abs(np.fft.rfft(x[:, c % x.shape[1]], N))**2 for c in pairs)*sh*gain**2
    return (P*pink)[band].sum()/pink[band].sum()

def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[1], formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    ap.add_argument('windows', nargs='?', help='mounted Windows drive (the folder that contains Windows/, ProgramData/, Users/)')
    ap.add_argument('--device', help='part of the headset file name, if several are installed')
    ap.add_argument('--data', help='path to *_apo4_data.bin (instead of searching the drive)')
    ap.add_argument('--presets', help='path to *_props.dat or an unpacked preset folder')
    ap.add_argument('--demo', help='path to the 12-channel demo .mov')
    ap.add_argument('--room', help='room preset id (default: the one active in Synapse, else "default")')
    ap.add_argument('--list-rooms', action='store_true')
    ap.add_argument('--no-demo', action='store_true')
    ap.add_argument('--out', default=os.path.join(DATA, 'synapse'))
    a = ap.parse_args()
    sel = None
    if a.windows:
        data, pack, sel, demo = find(a.windows, a.device)
        a.data = a.data or data; a.presets = a.presets or pack; a.demo = a.demo or demo
    if not (a.data and a.presets): ap.error('give the Windows drive, or both --data and --presets')
    presets = room.Presets(a.presets)
    if a.list_rooms: print('\n'.join(presets.rooms())); return
    rid = a.room or (room.active_room(sel) if sel else None) or 'default'
    p = presets.room(rid)

    print('dataset:', a.data); print('presets:', a.presets, ' room:', rid)
    D = hrtf_set.load(a.data); bad = [n for n, ok in hrtf_set.check(D) if not ok]
    for n, ok in hrtf_set.check(D): print('  %s %s' % ('ok  ' if ok else 'FAIL', n))
    if bad: sys.exit('the dataset failed %d check(s); not using it' % len(bad))
    hr = hrtf_set.Hrtf(D)

    irs = room.speaker_irs(hr, p, room.SPEAKERS_71)
    chans = [irs[s][e] for s, e in room.HESUVI]
    os.makedirs(a.out, exist_ok=True); wav = os.path.join(a.out, 'lynapse-room.wav')
    hrtf_set.write_wav(wav, chans)
    # gain that gives the same loudness as the bundled KEMAR file with the installer's default gain/shelf
    kemar, _ = hrtf_set.read_wav(os.path.join(REPO, 'assets', 'spatial', 'lynapse-kemar.wav'))
    new = np.array(chans).T
    gain = 3.17*np.sqrt(pink_energy(kemar, (0, 8), 1.0, -2.5)/pink_energy(new, (0, 8)))
    json.dump(dict(room=rid, frames=new.shape[0], layout='HeSuVi 14ch', install_gain=round(float(gain), 3), install_shelf_db=0.0),
              open(wav[:-4] + '.json', 'w'), indent=1)
    print('\nwrote %s (%.3f s, HRTF + early reflections + late reverb)' % (wav, new.shape[0]/hrtf_set.FS))

    if a.demo and not a.no_demo:
        try:
            import demo
            d = demo.build(a.demo, hr, p, os.path.join(DATA, 'spatial-demo'))
            print('wrote the Demo page clip (%.1f s) to %s' % (d, os.path.join(DATA, 'spatial-demo')))
        except Exception as e:
            print('demo clip skipped:', e)
    elif not a.no_demo:
        print('no demo clip found (optional; open the Spatial Audio demo in Synapse once to download it)')

    print('\nInstall it (re-run any time; your old filter config is kept as .bak):\n'
          '  ./install-with-spatial.sh --hrir %s --gain %.3f --shelf 0\n'
          'Keep these files to yourself: they are built from Razer data in your own install.' % (wav, gain))

if __name__ == '__main__': main()
