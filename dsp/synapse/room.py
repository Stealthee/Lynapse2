"""Room model of Synapse's Spatial Audio, re-implemented from its behaviour, driven by the room presets from
YOUR OWN Synapse install (the preset pack `*_props.dat` is a ZIP of JSON files; nothing is shipped here).

Per virtual speaker the engine renders:
  direct      HRTF(speaker direction) * direct_gain * ref/dist                       (ref = 0.5 m)
  early refl. shoebox image-source model, each reflection rendered through the HRTF from its own direction
  late reverb Freeverb on the mono sum of ALL input channels, added to both ears

Early reflections:
  room   = box centred on the listener horizontally: x +-width/2, z +-length/2, height vertical with the
           floor 1.0668 m (3.5 ft) below the ears; walls reflect fully (no absorption)
  images = mirror the source across each wall except the one it just came from; keep an image if the
           listener->image path crosses no other wall; depth 2 if max_reflections > 6, else 1
  length += sign * order * early_stagger when stagger is on (sign = parity of floor() of the image coords)
  gain   = ref/length; emitters face the listener with a cardioid pattern: gain *= (1 + d.f)/2
           (d = emitter -> first wall hit, f = emitter facing); keep the `max_reflections` strongest
  each   : delay (length - ref)/343.2 s (whole samples), level early_reflections_gain; sum -> ER EQ
Late reverb (Freeverb with its classic tunings, but this engine's parameter mapping):
  combs 1116 1188 1277 1356 1422 1491 1557 1617, allpasses 556 441 341 225 (@44.1 kHz), first N used
  length L = int(t*delay_mult*fs/44100), R = int((t+spread)*delay_mult*fs/44100)
  comb feedback = room_size, damping = damp, allpass feedback = all_pass_feedback, input gain 1
  wet1 = wet*(width/2+0.5), wet2 = wet*(1-width)/2; pre-EQ -> reverb -> post-EQ -> * late_gain
EQ bands: RBJ Audio-EQ-Cookbook biquads (alpha = sin(w0)/(2Q), A = 10^(dB/40)).
"""
import io, json, os, zipfile
import numpy as np
from hrtf_set import FS, direction_vec

C = 343.2; REF = 0.5; FLOOR = -1.0668
COMB_T = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617]
ALLP_T = [556, 441, 341, 225]
P = 'spatial_advanced_'

# ------------------------------------------------------------------ presets
class Presets:
    """Room/profile presets from a preset pack (*_props.dat ZIP) or an unpacked preset directory."""
    def __init__(self, path):
        self.files = {}
        if zipfile.is_zipfile(path):
            z = zipfile.ZipFile(path)
            for n in z.namelist():
                if n.endswith('.json'): self.files[n.replace('\\', '/')] = z.read(n)
        else:
            for dp, _, fs in os.walk(path):
                for f in fs:
                    if f.endswith('.json'):
                        full = os.path.join(dp, f); self.files[os.path.relpath(full, path)] = open(full, 'rb').read()
    def rooms(self):
        return sorted(os.path.basename(n)[:-5] for n in self.files if '/room_presets/' in '/' + n)
    def room(self, rid):
        n = next((n for n in self.files if '/room_presets/' in '/' + n and os.path.basename(n) == rid + '.json'), None)
        if n is None: raise KeyError('room preset %r not found' % rid)
        return {p['id']: p['value'] for p in json.loads(self.files[n])['properties']}

def active_room(selection_json):
    """room preset id from an endpoint's preset_selection.json (None if unreadable)."""
    try: return next(s['current_preset'] for s in json.load(open(selection_json)) if s['id'] == 'room_presets')
    except Exception: return None

# ------------------------------------------------------------------ EQ
def rbj(kind, f, gain_db, q, fs=FS):
    w = 2*np.pi*f/fs; c = np.cos(w); s = np.sin(w); al = s/(2*q + 2.220446049250313e-16); A = 10**(gain_db/40)
    sa = 2*np.sqrt(A)*al
    b, a = {'PEAK': ([1+al*A, -2*c, 1-al*A], [1+al/A, -2*c, 1-al/A]),
            'LOW_SHELF': ([A*((A+1)-(A-1)*c+sa), 2*A*((A-1)-(A+1)*c), A*((A+1)-(A-1)*c-sa)], [(A+1)+(A-1)*c+sa, -2*((A-1)+(A+1)*c), (A+1)+(A-1)*c-sa]),
            'HIGH_SHELF': ([A*((A+1)+(A-1)*c+sa), -2*A*((A-1)+(A+1)*c), A*((A+1)+(A-1)*c-sa)], [(A+1)-(A-1)*c+sa, 2*((A-1)-(A+1)*c), (A+1)-(A-1)*c-sa]),
            'HIGH_PASS': ([(1+c)/2, -(1+c), (1+c)/2], [1+al, -2*c, 1-al]),
            'LOW_PASS': ([(1-c)/2, 1-c, (1-c)/2], [1+al, -2*c, 1-al]),
            'NOTCH': ([1, -2*c, 1], [1+al, -2*c, 1-al]),
            'BAND_PASS': ([al, 0, -al], [1+al, -2*c, 1-al]),
            'ALL_PASS': ([1-al, -2*c, 1+al], [1+al, -2*c, 1-al])}[kind]
    return np.array(b)/a[0], np.array(a)/a[0]

def biquad(x, b, a):
    y = np.empty_like(x); z1 = z2 = 0.0
    for n, v in enumerate(x):
        o = b[0]*v + z1; z1 = b[1]*v - a[1]*o + z2; z2 = b[2]*v - a[2]*o; y[n] = o
    return y

def eq_bands(p, stage):
    k = lambda n, f: '%s%s_eq_band_%d_%s' % (P, stage, n, f)
    return [(p[k(n, 'type')], float(p[k(n, 'freq')]), float(p[k(n, 'gain_db')]), float(p[k(n, 'q')]))
            for n in range(10) if p.get(k(n, 'enabled'))]

def apply_eq(x, bands, fs=FS):
    for kind, f, g, q in bands: x = biquad(x, *rbj(kind, f, g, q, fs))
    return x

# ------------------------------------------------------------------ late reverb
def freeverb(x, p, fs=FS):
    k = fs/44100.0; lr = P + 'late_reverb_'
    mult = float(p[lr + 'comb_filter_delay_mult']); spread = int(p[lr + 'stereo_spread'])
    nc = int(p[lr + 'num_comb_filters']); na = int(p[lr + 'num_all_pass_filters'])
    fb = np.float32(p[lr + 'room_size']); d1 = np.float32(p[lr + 'damp']); d2 = np.float32(1) - d1
    afb = np.float32(p[lr + 'all_pass_feedback']); wet = float(p[lr + 'wet']); width = float(p[lr + 'width'])
    wet1 = np.float32((width*0.5 + 0.5)*wet); wet2 = np.float32((1.0 - width)*0.5*wet)
    acc = []
    for ch in range(2):
        ln = lambda t: int(mult*k*t) if ch == 0 else int((t + spread)*mult*k)
        cl = [ln(t) for t in COMB_T[:nc]]; al = [ln(t) for t in ALLP_T[:na]]
        cb = [np.zeros(n, np.float32) for n in cl]; ci = [0]*nc; st = [np.float32(0)]*nc
        ab = [np.zeros(n, np.float32) for n in al]; ai = [0]*na
        y = np.empty(len(x), np.float32)
        for n, v in enumerate(x.astype(np.float32)):
            s = np.float32(0)
            for j in range(nc):
                o = cb[j][ci[j]]; st[j] = st[j]*d1 + o*d2; cb[j][ci[j]] = st[j]*fb + v; ci[j] = (ci[j] + 1) % cl[j]; s += o
            for j in range(na):
                o = ab[j][ai[j]]; ab[j][ai[j]] = o*afb + s; ai[j] = (ai[j] + 1) % al[j]; s = o - s
            y[n] = s
        acc.append(y)
    return acc[0]*wet1 + acc[1]*wet2, acc[1]*wet1 + acc[0]*wet2

def late_reverb(p, fs=FS, seconds=4.0):
    if not p.get(P + 'late_reverb_enabled', True): return np.zeros(1), np.zeros(1)
    x = np.zeros(int(seconds*fs)); x[0] = 1.0
    L, R = freeverb(apply_eq(x, eq_bands(p, 'late_reverb_pre'), fs), p, fs)
    post = eq_bands(p, 'late_reverb_post'); g = 10**(float(p[P + 'late_reverb_gain'])/20)
    L = apply_eq(L.astype(np.float64), post, fs)*g; R = apply_eq(R.astype(np.float64), post, fs)*g
    e = L**2 + R**2; tail = np.cumsum(e[::-1])[::-1]; n = np.nonzero(tail > tail[0]*1e-12)[0][-1] + 1
    return L[:n], R[:n]

# ------------------------------------------------------------------ early reflections
def _walls(W, H, L):
    hx, hy, hz = W/2, H/2, L/2; cy = FLOOR + hy if -hy < FLOOR else 0.0; c = np.array([0.0, cy, 0.0])
    ws = [(c + [0, 0, -hz], np.array([0, 0, 1.0])), (c + [0, 0, hz], np.array([0, 0, -1.0])),
          (c + [-hx, 0, 0], np.array([1.0, 0, 0])), (c + [hx, 0, 0], np.array([-1.0, 0, 0])),
          (c + [0, -hy, 0], np.array([0, 1.0, 0])), (c + [0, hy, 0], np.array([0, -1.0, 0]))]
    return c, np.array([hx, hy, hz]), ws

def _crosses(a, b, skip, c, h, ws):
    for k, (q, n) in enumerate(ws):
        den = (b - a) @ n
        if k == skip or abs(den) < 1e-12: continue
        t = ((q - a) @ n)/den
        if 1e-9 < t < 1 - 1e-9:
            d = np.abs(a + t*(b - a) - c); ax = [2, 2, 0, 0, 1, 1][k]
            if all(d[i] <= h[i] + 1e-6 for i in range(3) if i != ax): return True
    return False

def reflections(src, p):
    W, H, L = (float(p[P + 'room_' + k]) for k in ('width', 'height', 'length'))
    c, h, ws = _walls(W, H, L); lis = np.zeros(3)
    nmax = int(p[P + 'max_reflections']); order = 2 if nmax > 6 else 1
    stag = float(p[P + 'early_stagger']) if p.get(P + 'early_stagger_enabled') else 0.0
    fwd = -src/np.linalg.norm(src); out = []
    def rec(pt, depth, last, chain):
        for k, (q, n) in enumerate(ws):
            if k == last: continue
            img = pt - 2*((pt - q) @ n)*n
            if _crosses(lis, img, k, c, h, ws): continue
            ch = chain + [(k, img)]; out.append(ch)
            if depth < order: rec(img, depth + 1, k, ch)
    rec(src, 1, -1, [])
    res = []
    for chain in out:
        img = chain[-1][1]; ln = np.linalg.norm(img)
        if stag: ln += np.prod([1 if int(np.floor(v)) % 2 == 0 else -1 for v in img])*len(chain)*stag
        g = REF/ln if ln >= REF else 1.0
        cur = lis.copy(); ok = True
        for k, im in reversed(chain):                     # trace back to the first wall hit
            q, n = ws[k]; d = cur - im; d /= np.linalg.norm(d); den = d @ n
            t = ((q - im) @ n)/den if abs(den) > 1e-5 else -1
            if den < 0 or t < 0: ok = False; break
            cur = im + d*t
        if ok:
            d = cur - src; d /= np.linalg.norm(d); g *= (d @ fwd + 1)*0.5
        res.append(dict(walls=[k for k, _ in chain], image=img, length=float(ln), gain=float(g)))
    res.sort(key=lambda r: -r['gain'])
    return res[:nmax]

# ------------------------------------------------------------------ per-speaker room response
def speaker_irs(hrtf, p, speakers, fs=FS, absolute=False):
    """{name: (az, el, dist)} -> {name: (left_ir, right_ir)}, direct + early reflections + late reverb.
       absolute=False: each speaker scaled so its direct part equals the plain HRIR (drop-in HRIR file).
       absolute=True : the engine's own scale (keeps level differences between speakers, for whole mixes)."""
    gd = 10**(float(p[P + 'direct_gain'])/20)
    ge = 10**(float(p[P + 'early_reflections_gain'])/20) if p.get(P + 'early_reflections_enabled') else 0.0
    revL, revR = late_reverb(p, fs); er = eq_bands(p, 'early_reflection')
    refl = {s: (reflections(sph2cart(*v), p) if ge else []) for s, v in speakers.items()}
    maxdel = max([(r['length'] - speakers[s][2])/C*fs for s in speakers for r in refl[s]] + [0])
    n = int(max(len(revL), maxdel + 600)) + 64
    out = {}
    for s, (az, el, dist) in speakers.items():
        gdir = gd*(REF/dist if dist >= REF else 1.0); norm = 1.0 if absolute else gdir
        dL, dR = hrtf.ir(az, el, n, 0.0, fs)
        eL = np.zeros(n); eR = np.zeros(n)
        for r in refl[s]:
            a, e = cart2azel(r['image']); amp = r['gain']*ge/norm
            L, R = hrtf.ir(a, e, n, float(int((r['length'] - dist)/C*fs)), fs); eL += amp*L; eR += amp*R
        eL = apply_eq(eL, er, fs); eR = apply_eq(eR, er, fs)
        rl = np.zeros(n); rr = np.zeros(n); rl[:len(revL)] = revL/norm; rr[:len(revR)] = revR/norm
        out[s] = ((gdir/norm)*dL + eL + rl, (gdir/norm)*dR + eR + rr)
    return out

def sph2cart(az, el, r): return r*direction_vec(az, el)
def cart2azel(v): return np.degrees(np.arctan2(-v[0], -v[2])), np.degrees(np.arcsin(v[1]/np.linalg.norm(v)))

# Virtual speakers at the engine's default emitter angles (azimuth + = left, elevation up, distance m)
SPEAKERS_71 = {'FL': (30, 0, 1.5), 'FR': (-30, 0, 1.5), 'FC': (0, 0, 1.5), 'SL': (90, 0, 1.5), 'SR': (-90, 0, 1.5),
               'RL': (142.5, 0, 1.5), 'RR': (-142.5, 0, 1.5)}
HESUVI = [('FL', 0), ('FL', 1), ('SL', 0), ('SL', 1), ('RL', 0), ('RL', 1), ('FC', 0),
          ('FR', 1), ('FR', 0), ('SR', 1), ('SR', 0), ('RR', 1), ('RR', 0), ('FC', 1)]
