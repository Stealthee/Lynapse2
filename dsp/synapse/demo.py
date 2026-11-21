"""Build the Demo page's "Spatial Audio vs Stereo" clip from the 12-channel (7.1.4) demo that Synapse
downloads to  ...\\AppData\\Local\\Razer\\RazerAppEngine\\User Data\\Apps\\Common\\audiodata\\DemoVideo\\  (an
audio-only .mov). Outputs (in ~/.local/share/razerctl/spatial-demo/, read by the app):
  demo.flac         binaural render: every channel through its own virtual speaker (HRTF + room)
  demo_stereo.flac  plain stereo fold-down of the same clip, loudness-matched (the "Stereo" side of the switch)
  demo.json         30 fps per-channel levels, overall direction and height share (the speaker map)
Needs ffmpeg."""
import json, os, subprocess
import numpy as np
from hrtf_set import FS, direction_vec, write_wav
import room

# FFmpeg's 7.1.4 channel order at the engine's default emitter positions (az + = left, el up, distance m)
CH = [('FL', 30, 0, 1.5), ('FR', -30, 0, 1.5), ('FC', 0, 0, 1.5), ('LFE', 0, 0, 1.5),
      ('BL', 142.5, 0, 1.5), ('BR', -142.5, 0, 1.5), ('SL', 90, 0, 1.5), ('SR', -90, 0, 1.5),
      ('TFL', 30, 30, 2.0), ('TFR', -30, 30, 2.0), ('TBL', 142.5, 60, 2.0), ('TBR', -142.5, 60, 2.0)]
FPS = 30

def _decode(path, ch):
    raw = subprocess.run(['ffmpeg', '-v', 'error', '-i', path, '-f', 'f32le', '-ar', str(FS)] + (['-ac', '2'] if ch == 2 else []) + ['-'],
                         check=True, capture_output=True).stdout
    return np.frombuffer(raw, '<f4').reshape(-1, ch).astype(np.float64)

def _flac(path, x):
    subprocess.run(['ffmpeg', '-v', 'error', '-y', '-f', 'f32le', '-ar', str(FS), '-ac', '2', '-i', '-', '-c:a', 'flac', '-sample_fmt', 's32', path],
                   input=np.ascontiguousarray(x.T, dtype='<f4').tobytes(), check=True)

def build(src, hrtf, preset, outdir):
    probe = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'a:0', '-show_entries', 'stream=channels', '-of', 'csv=p=0', src],
                           capture_output=True, text=True).stdout.strip()
    if probe != '12': raise ValueError('expected a 12-channel (7.1.4) demo clip, %s has %s channels' % (src, probe or '?'))
    x = _decode(src, 12)
    irs = room.speaker_irs(hrtf, preset, {c[0]: c[1:] for c in CH}, absolute=True)
    n = len(x) + max(len(v[0]) for v in irs.values()); N = 1 << (n - 1).bit_length()
    y = np.zeros((2, N))
    for i, (name, *_ ) in enumerate(CH):
        X = np.fft.rfft(x[:, i], N)
        for ear in range(2): y[ear] += np.fft.irfft(X*np.fft.rfft(irs[name][ear], N), N)
    y = y[:, :len(x) + FS//2]; y *= 10**(-1/20)/np.abs(y).max()
    st = _decode(src, 2).T; st *= np.sqrt((y**2).mean()/(st**2).mean())
    if np.abs(st).max() > 0.95: st *= 0.95/np.abs(st).max()
    os.makedirs(outdir, exist_ok=True)
    _flac(os.path.join(outdir, 'demo.flac'), y); _flac(os.path.join(outdir, 'demo_stereo.flac'), st)
    # visualization data
    hop = FS//FPS; nfr = len(x)//hop; e = (x[:nfr*hop]**2).reshape(nfr, hop, 12).mean(1)
    sm = np.zeros_like(e)
    for k in range(nfr):
        prev = sm[k-1] if k else e[0]; sm[k] = np.where(e[k] > prev, prev + 0.6*(e[k] - prev), prev + 0.12*(e[k] - prev))
    db = 10*np.log10(sm + 1e-12); ref = np.percentile(np.delete(db, 3, axis=1).max(1), 99)
    lv = np.clip((db - ref + 45)/45, 0, 1); frames = []
    for k in range(nfr):
        v = sum(sm[k, i]*direction_vec(az, el) for i, (nm, az, el, _) in enumerate(CH) if nm != 'LFE')
        tot = np.delete(sm[k], 3).sum(); v = v/tot if tot > 0 else np.zeros(3)
        top = sm[k, 8:12].sum(); ear = sm[k, [0, 1, 2, 4, 5, 6, 7]].sum()
        frames.append([[round(float(a), 2) for a in lv[k]], [round(float(a), 3) for a in v], round(float(top/(top + ear)) if top + ear > 0 else 0.0, 3)])
    json.dump(dict(fps=FPS, duration=len(x)/FS, channels=[dict(name=nm, az=az, el=el) for nm, az, el, _ in CH], frames=frames),
              open(os.path.join(outdir, 'demo.json'), 'w'), separators=(',', ':'))
    return len(x)/FS
