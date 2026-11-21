"""Read the HRTF set that Razer Synapse installs for a headset's Spatial Audio driver (the 32 MB
`*_apo4_data.bin` in Windows' DriverStore), and turn it into impulse responses.

Nothing here is shipped data: you point it at the file from YOUR OWN Windows install.

File format (little endian, a serialized C++ object written field by field, no padding, no header):
  float32 up[3], origin[3]
  uint32  n_measurements, n_taps, n_triangles ; float32 measurement_radius
  n_measurements x { uint16 index ; float32 position[3], radius, azimuth_deg, elevation_deg,
                     delay_left_s, delay_right_s ; float32 taps_left[n_taps], taps_right[n_taps] }
  uint64  n_triangles  ;  n_triangles x { uint16 i0,i1,i2 ; float32 normal[3], edge_normals[3][3], plane, inv_area }
  uint64  n_measurements ; float32 positions[n][3]          (copy of the measurement positions)
  2 x optional KD-tree (direction lookup)                    -> ends exactly at end of file
Tap blocks are a packed real FFT (N = n_taps): [X0, X(N/2), re/im X1 .. X(N/2-1)]; irfft gives the HRIR,
a minimum-phase-like filter whose second half is zero. The per-ear delays (seconds) are NOT in the taps.
Coordinates: front = -Z, +X = right ear, +Y = up; azimuth = atan2(-x, -z) (positive = left), 48 kHz.
"""
import struct
import numpy as np

FS = 48000

class _Reader:
    def __init__(self, buf): self.b = buf; self.p = 0; self.n = len(buf)
    def u8(self): v = self.b[self.p]; self.p += 1; return v
    def u32(self): v = struct.unpack_from('<I', self.b, self.p)[0]; self.p += 4; return v
    def u64(self): v = struct.unpack_from('<Q', self.b, self.p)[0]; self.p += 8; return v
    def f32(self, c=1):
        v = np.frombuffer(self.b, '<f4', c, self.p); self.p += 4*c
        return float(v[0]) if c == 1 else v.copy()

def _kdtree(r):
    nodes = []; err = [None]
    def opt(depth):
        if err[0] or r.p >= r.n: err[0] = err[0] or 'eof'; return None
        if r.u8() == 0: return None
        L = opt(depth + 1); R = opt(depth + 1)
        if err[0] or r.p + 8 > r.n: err[0] = err[0] or 'eof'; return None
        c = r.u64()
        if c > 60000 or r.p + 2*c + 8 > r.n: err[0] = 'bad count'; return None
        idx = np.frombuffer(r.b, '<u2', int(c), r.p).astype(np.int64); r.p += 2*int(c)
        axis, val = struct.unpack_from('<if', r.b, r.p); r.p += 8
        nodes.append(dict(leaf=L is None and R is None, idx=idx, axis=axis, value=val)); return True
    opt(0); return nodes, err[0]

def load(path):
    """Parse the file; returns a dict. Raises ValueError if it does not look like the expected format."""
    buf = open(path, 'rb').read(); r = _Reader(buf)
    try:
        r.f32(6); n_meas, n_taps, n_tri = r.u32(), r.u32(), r.u32(); radius = r.f32()
        if not (100 < n_meas < 65536 and n_taps in (256, 512, 1024) and 0 < n_tri < 200000 and 0.1 < radius < 10):
            raise ValueError('header does not match (measurements %d, taps %d)' % (n_meas, n_taps))
        stride = 2 + 32 + 8*n_taps
        rec = np.frombuffer(buf, np.uint8, n_meas*stride, r.p).reshape(n_meas, stride)
        D = dict(radius=radius, n_taps=n_taps,
                 idx=rec[:, 0:2].copy().view('<u2')[:, 0],
                 pos=rec[:, 2:14].copy().view('<f4'), sph=rec[:, 14:26].copy().view('<f4'),
                 itd=rec[:, 26:34].copy().view('<f4'),
                 tapsL=rec[:, 34:34 + 4*n_taps].copy().view('<f4'), tapsR=rec[:, 34 + 4*n_taps:].copy().view('<f4'))
        r.p += n_meas*stride
        if r.u64() != n_tri: raise ValueError('triangle count mismatch')
        tri = np.frombuffer(buf, np.uint8, n_tri*62, r.p).reshape(n_tri, 62); r.p += n_tri*62
        D['tri_i'] = tri[:, 0:6].copy().view('<u2'); D['tri_n'] = tri[:, 6:18].copy().view('<f4')
        n2 = r.u64(); D['pos2'] = np.frombuffer(buf, '<f4', int(n2)*3, r.p).reshape(-1, 3).copy(); r.p += int(n2)*12
        trees = []
        for _ in range(2):
            if r.u8() != 1: raise ValueError('direction-lookup tree missing')
            trees.append(_kdtree(r))
        D['trees'] = trees; D['end'] = r.p; D['size'] = len(buf)
    except (struct.error, IndexError) as e:
        raise ValueError('file ended early or is not the expected format (%s)' % e)
    return D

def check(D):
    """Self-checks; returns a list of (name, ok). All must pass before the data is used."""
    pn = np.linalg.norm(D['pos'], axis=1)
    irL = decode(D['tapsL'])
    front = np.mean((irL[:, :32]**2).sum(1)/(irL**2).sum(1))
    return [
        ('records are numbered 0..n-1', (D['idx'] == np.arange(len(D['idx']))).all()),
        ('directions lie on one sphere', np.allclose(pn, D['radius'], atol=1e-3)),
        ('mesh references real directions', D['tri_i'].max() < len(D['pos'])),
        ('position copy matches', np.array_equal(D['pos2'], D['pos'])),
        ('lookup trees parse to the end of the file', all(e is None for _, e in D['trees']) and D['end'] == D['size']),
        ('filters decode to front-loaded impulse responses', front > 0.9),
        ('no NaN/Inf in filters', np.isfinite(D['tapsL']).all() and np.isfinite(D['tapsR']).all()),
    ]

def decode(taps):
    """packed real-FFT tap blocks (..., N) -> impulse responses (..., N)."""
    t = taps.astype(np.float64); N = t.shape[-1]; h = N//2
    X = np.empty(t.shape[:-1] + (h + 1,), np.complex128)
    X[..., 0] = t[..., 0]; X[..., h] = t[..., 1]; X[..., 1:h] = t[..., 2::2] + 1j*t[..., 3::2]
    return np.fft.irfft(X, n=N, axis=-1)

def frac_shift(x, samples):
    """Delay 1-D x by a (fractional) number of samples via FFT phase rotation (x must be long enough)."""
    N = len(x); k = np.arange(N//2 + 1)
    return np.fft.irfft(np.fft.rfft(x)*np.exp(-2j*np.pi*k*samples/N), n=N)

def direction_vec(az, el):
    a, e = np.radians(az), np.radians(el)
    return np.array([-np.cos(e)*np.sin(a), np.sin(e), -np.cos(e)*np.cos(a)])

class Hrtf:
    """Direction -> (left, right) HRIR with the stored inter-ear delays applied."""
    def __init__(self, D):
        self.D = D; self.L0 = decode(D['tapsL']); self.R0 = decode(D['tapsR'])
        self.dref = float(D['itd'].min())
        self.M = D['pos'][D['tri_i'].astype(int)].astype(np.float64).transpose(0, 2, 1)   # (tri, xyz, vertex)
    def interp(self, az, el):
        """Barycentric blend of the 3 measured directions around (az, el); returns (L, R, delays_s)."""
        d = direction_vec(az, el)
        w = np.linalg.solve(self.M, np.broadcast_to(d, (len(self.M), 3))[..., None])[..., 0]
        ok = np.where((w.min(1) >= -1e-9) & (w.sum(1) > 0))[0]
        t = ok[np.argmax(w[ok].min(1))]; wt = w[t]/w[t].sum(); vi = self.D['tri_i'][t].astype(int)
        return wt @ self.L0[vi], wt @ self.R0[vi], wt @ self.D['itd'][vi].astype(np.float64)
    def ir(self, az, el, n, extra_delay=0.0, fs=FS):
        L, R, dl = self.interp(az, el)
        out = []
        for h, d in ((L, dl[0]), (R, dl[1])):
            x = np.zeros(n); x[:len(h)] = h
            out.append(frac_shift(x, (d - self.dref)*fs + extra_delay))
        return out

def write_wav(path, chans, fs=FS):
    """float32 WAV; chans = (n_channels, n_frames). >2 channels uses WAVE_FORMAT_EXTENSIBLE."""
    d = np.ascontiguousarray(np.asarray(chans, dtype=np.float32).T); frames, ch = d.shape; raw = d.tobytes(); ba = ch*4
    if ch <= 2: fmt = struct.pack('<HHIIHHH', 3, ch, fs, fs*ba, ba, 32, 0)
    else:
        guid = struct.pack('<IHH', 3, 0, 0x10) + bytes.fromhex('800000aa00389b71')
        fmt = struct.pack('<HHIIHHHHI', 0xFFFE, ch, fs, fs*ba, ba, 32, 22, 32, 0) + guid
    body = b'WAVE' + b'fmt ' + struct.pack('<I', len(fmt)) + fmt + b'fact' + struct.pack('<II', 4, frames) + b'data' + struct.pack('<I', len(raw)) + raw
    with open(path, 'wb') as f: f.write(b'RIFF' + struct.pack('<I', len(body)) + body)

def read_wav(path):
    """Minimal WAV reader (PCM 16/24/32 or float32) -> (frames, channels) float64, sample rate."""
    b = open(path, 'rb').read(); i = b.find(b'fmt '); fmt, ch, fs = struct.unpack('<HHI', b[i+8:i+16]); bits = struct.unpack('<H', b[i+22:i+24])[0]
    if fmt == 0xFFFE: fmt = struct.unpack('<H', b[i+32:i+34])[0]
    j = b.find(b'data'); n = struct.unpack('<I', b[j+4:j+8])[0]; raw = b[j+8:j+8+n]
    if fmt == 3: x = np.frombuffer(raw, '<f4').astype(np.float64)
    elif bits == 24:
        a = np.frombuffer(raw, np.uint8).reshape(-1, 3).astype(np.int32)
        x = ((a[:, 0] | a[:, 1] << 8 | a[:, 2] << 16) << 8 >> 8)/2.0**23
    else: x = np.frombuffer(raw, '<i%d' % (bits//8)).astype(np.float64)/2.0**(bits - 1)
    return x.reshape(-1, ch), fs
