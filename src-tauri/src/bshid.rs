// BlackShark V3 Pro over /dev/hidraw: a userspace stand-in for the razerkraken kernel driver.
//
// Used only when no razerkraken driver is bound to the headset (e.g. the CachyOS EAC kernel,
// which refuses unsigned DKMS modules). The kernel's own hid-generic driver then owns the dongle's
// HID interface and exposes it as /dev/hidrawN; this module speaks the same Razer vendor protocol
// as the modded driver (openrazer-blackshark, razerkraken_driver.c) through it.
//
// The rest of razerctl talks to the headset through sysfs-style attribute files. To keep all of
// that code unchanged, this module presents the device under a virtual directory
// (/razerctl-hidraw/hidrawN) with the same attribute names and the same text formats as the
// driver, and `read` / `write` / `exists` below route paths under it here and everything else to
// the real filesystem.
//
// Protocol (everything is a 64-byte report 0x02 sent as a HID SET_REPORT; the dongle's interface
// has no interrupt-OUT endpoint, so a hidraw write() becomes exactly that control transfer):
//   [0]=0x02 report id, [2]=0x60 transaction id, [6]=payload length from [9],
//   [9]=direction (0x80 dongle, 0x00 cable), [10]=class, [11]=sub, [12]=count/cmd, [13..]=args,
//   [62]=XOR of bytes 0..61.
// Replies and the headset's own pushes come back as input report 0x02 on ep 0x84, which hidraw
// read() delivers while the node is open. Report 0x05 [05 00] every 3.5 s keeps the RF telemetry
// channel awake.

use parking_lot::{Condvar, Mutex};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, LazyLock,
    },
    thread,
    time::{Duration, Instant},
};

pub const VROOT: &str = "/razerctl-hidraw";
const VID: &str = "1532";
const PID_PRO: &str = "0577"; // 2.4 GHz dongle
const PID_PRO_WIRED: &str = "0576"; // USB-C cable
const LEN: usize = 64;
const EQ_SLOTS: i64 = 9;

// ---------------------------------------------------------------- frame builders

fn crc(b: &mut [u8; LEN]) {
    b[62] = b[..62].iter().fold(0, |a, x| a ^ x);
}

/// razer_blackshark_v3pro_build: always direction 0x80 (the driver does the same for both PIDs).
fn pro(class: u8, cmd: u8, args: &[u8]) -> [u8; LEN] {
    let mut b = [0u8; LEN];
    b[0] = 0x02;
    b[2] = 0x60;
    b[6] = 4 + args.len() as u8;
    b[9] = 0x80;
    b[10] = class;
    b[12] = cmd;
    b[13..13 + args.len()].copy_from_slice(args);
    crc(&mut b);
    b
}

/// razer_blackshark_build_get
fn get(param: u8, dir: u8) -> [u8; LEN] {
    let mut b = [0u8; LEN];
    b[0] = 0x02;
    b[2] = 0x60;
    b[6] = 0x04;
    b[9] = dir;
    b[10] = param;
    crc(&mut b);
    b
}

/// razer_blackshark_build_set
fn set(param: u8, val: u8, dir: u8) -> [u8; LEN] {
    let mut b = [0u8; LEN];
    b[0] = 0x02;
    b[2] = 0x60;
    b[6] = 0x05;
    b[9] = dir;
    b[10] = param;
    b[12] = 0x01;
    b[13] = val;
    crc(&mut b);
    b
}

/// sign-magnitude band byte, -6..=+6 dB
fn band(db: i64) -> u8 {
    let db = db.clamp(-6, 6);
    if db < 0 { 0x80 | (-db) as u8 } else { db as u8 }
}
fn unband(b: u8) -> i64 {
    if b & 0x80 != 0 { -((b & 0x7f) as i64) } else { b as i64 }
}

/// razer_blackshark_build_mic_eq_cmd (0x97)
fn mic_eq(bands: &[i64; 10], dir: u8) -> [u8; LEN] {
    let mut b = [0u8; LEN];
    b[0] = 0x02;
    b[2] = 0x60;
    b[6] = 0x0e;
    b[9] = dir;
    b[10] = 0x97;
    b[12] = 0x0a;
    for (i, v) in bands.iter().enumerate() {
        b[13 + i] = band(*v);
    }
    crc(&mut b);
    b
}

// EQ preset tables, identical to bs_v3pro_eq_{bands,meta,commit} in the driver.
const EQ_BANDS: [[u8; 12]; 5] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x01, 0x02, 0x02, 0x05, 0x05, 0x01, 0x81, 0x02, 0x03, 0x03, 0x03, 0x00],
    [0x02, 0x03, 0x03, 0x03, 0x81, 0x84, 0x84, 0x02, 0x03, 0x03, 0x03, 0x00],
    [0x03, 0x02, 0x02, 0x00, 0x00, 0x01, 0x81, 0x81, 0x03, 0x03, 0x03, 0x00],
    [0x04, 0x01, 0x01, 0x81, 0x00, 0x02, 0x00, 0x04, 0x04, 0x04, 0x83, 0x00],
];
const EQ_META: [[u8; 7]; 5] = [
    [0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00],
    [0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00],
    [0x02, 0x03, 0x01, 0x01, 0x03, 0x00, 0x00],
    [0x03, 0x02, 0x01, 0x01, 0x02, 0x00, 0x00],
    [0x04, 0x04, 0x01, 0x01, 0x0b, 0x00, 0x00],
];
const EQ_COMMIT: [[u8; 12]; 5] = [
    [0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x01, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x02, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x03, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x04, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
];

/// The 5-frame preset switch (razer_blackshark_v3pro_apply_eq). `custom` replaces the slot's curve.
fn eq_frames(slot: u8, custom: Option<&[i64; 10]>) -> [[u8; LEN]; 5] {
    let (mut bands, mut meta, mut commit);
    if (slot as usize) < EQ_BANDS.len() {
        bands = EQ_BANDS[slot as usize];
        meta = EQ_META[slot as usize];
        commit = EQ_COMMIT[slot as usize];
    } else {
        // slots 5..8: flat data with the slot index applied
        bands = EQ_BANDS[0];
        meta = EQ_META[0];
        commit = EQ_COMMIT[0];
        bands[0] = slot;
        meta[0] = slot;
        commit[0] = slot;
    }
    if let Some(c) = custom {
        for i in 0..10 {
            bands[1 + i] = band(c[i]);
        }
    }
    [
        pro(0xe1, 0x01, &[0x01, 0x00]), // gate: get state
        pro(0x95, 0x0b, &bands),        // band data
        pro(0xe0, 0x06, &meta),         // meta (byte 3 = make active)
        pro(0xe1, 0x01, &[0x02, 0x00]), // gate: apply
        pro(0xeb, 0x0b, &commit),       // commit
    ]
}

// ---------------------------------------------------------------- telemetry cache

/// Mirrors the driver's cached_* / pushed_* fields. -1 = unknown.
#[derive(Clone, Debug)]
struct Cache {
    battery_pct: i64,
    charging: i64,
    fw: Option<[u8; 3]>,
    serial: String,
    sidetone: i64,
    game_chat: i64,
    in_call_mix: i64,
    ull: i64,
    prompts: i64,
    fn_button: i64,
    thx: i64,
    mic_eq_preset: i64,
    mic_muted: i64,
    anc_mode: i64,
    anc_level: i64,
    power_save: i64,
    eq_profile: i64,
    indicator_led: i64,
    eq_query_slot: i64,
    eq_query_bands: [i64; 10],
    /// bumped on every vendor frame received (the driver's "vendor_response" completion)
    frames: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            battery_pct: -1,
            charging: -1,
            fw: None,
            serial: String::new(),
            sidetone: -1,
            game_chat: -1,
            in_call_mix: -1,
            ull: -1,
            prompts: -1,
            fn_button: -1,
            thx: -1,
            mic_eq_preset: -1,
            mic_muted: -1,
            anc_mode: -1,
            anc_level: -1,
            power_save: -1,
            eq_profile: -1,
            indicator_led: -1,
            eq_query_slot: -1,
            eq_query_bands: [0; 10],
            frames: 0,
        }
    }
}

enum Event {
    /// RF link (re)established: battery re-query + re-prime, like the driver's 0x20 handler
    LinkUp,
}

/// razer_blackshark_v3_cache. Returns an event to act on outside the lock.
fn absorb(c: &mut Cache, d: &[u8]) -> Option<Event> {
    c.frames += 1;
    let mut ev = None;
    if (d[11] == 0x01 || d[11] == 0x02) && d[12] >= 1 {
        let v = d[13];
        match d[10] {
            0x21 if v <= 100 => c.battery_pct = v as i64,
            0x2a if v <= 1 => c.charging = v as i64,
            0x20 if v == 0x01 => {
                c.charging = -1;
                c.battery_pct = -1;
                ev = Some(Event::LinkUp);
            }
            0x02 if d[11] == 0x01 && (3..=4).contains(&d[12]) && v != 0xff && (d[13] | d[14] | d[15]) != 0 => {
                c.fw = Some([d[13], d[14], d[15]]);
            }
            0x00 if d[11] == 0x01 && v != 0xff && (d[12] as usize) < 32 => {
                let s: String = d[13..13 + d[12] as usize].iter().take_while(|b| **b != 0).map(|b| *b as char).collect();
                if !s.is_empty() {
                    c.serial = s;
                }
            }
            0x19 if v <= 15 => c.sidetone = v as i64,
            0x5c | 0x65 if v != 0xff => c.game_chat = v as i64,
            0x5d if v != 0xff => c.in_call_mix = v as i64,
            0x5f if v <= 1 => c.ull = v as i64,
            0x66 if v <= 1 => c.prompts = v as i64,
            0x6a if v <= 3 => c.fn_button = v as i64,
            0x9e if v <= 1 => c.thx = v as i64,
            0x16 if (0x20..=0x23).contains(&v) => c.mic_eq_preset = (v - 0x20) as i64,
            0x55 if v <= 1 => c.mic_muted = v as i64,
            0x12 => {
                match v {
                    0x00 => c.anc_mode = 0,
                    0x01 => c.anc_mode = 1,
                    0x50 => c.anc_mode = 2,
                    _ => {}
                }
                if (1..=4).contains(&d[14]) {
                    c.anc_level = d[14] as i64;
                }
            }
            0x2c if v <= 60 => c.power_save = v as i64,
            0x60 if d[12] >= 4 && d[16] == 0x01 && (v as i64) < EQ_SLOTS => c.eq_profile = v as i64,
            0x15 if d[12] >= 11 => {
                for i in 0..10 {
                    c.eq_query_bands[i] = unband(d[14 + i]);
                }
                c.eq_query_slot = v as i64; // last: readers gate on it
            }
            _ => {}
        }
    }
    ev
}

// ---------------------------------------------------------------- device

pub struct Dev {
    node: String, // "hidraw10"
    pid: String,
    file: Mutex<fs::File>, // writes; also serialises commands like the driver's device->lock
    cache: Mutex<Cache>,
    cv: Condvar,
    alive: AtomicBool,
    started: Instant,
    /// generation of the last LinkUp handled, so overlapping re-queries don't pile up
    link_gen: AtomicU64,
}

impl Dev {
    fn dir(&self) -> u8 {
        if self.pid == PID_PRO_WIRED { 0x00 } else { 0x80 }
    }
    fn dongle(&self) -> bool {
        self.pid == PID_PRO
    }

    fn raw_write(&self, f: &mut fs::File, buf: &[u8]) -> io::Result<()> {
        match f.write(buf) {
            Ok(_) => Ok(()),
            Err(e) => {
                if matches!(e.raw_os_error(), Some(19) | Some(5)) {
                    // ENODEV / EIO: unplugged or rebound
                    self.alive.store(false, Ordering::SeqCst);
                }
                Err(e)
            }
        }
    }

    /// razer_blackshark_send_cmd: send one frame, then wait up to 1 s for any reply frame.
    /// Ok(false) = sent but nothing came back (several frames are write-only; the driver
    /// treats that as success too).
    fn send(&self, frame: &[u8; LEN]) -> io::Result<bool> {
        let mut f = self.file.lock();
        self.send_locked(&mut f, frame, Duration::from_millis(1000))
    }

    fn send_locked(&self, f: &mut fs::File, frame: &[u8; LEN], wait: Duration) -> io::Result<bool> {
        let before = self.cache.lock().frames;
        self.raw_write(f, frame)?;
        let deadline = Instant::now() + wait;
        let mut c = self.cache.lock();
        while c.frames == before {
            if self.cv.wait_until(&mut c, deadline).timed_out() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// razer_blackshark_v3_handshake: three priming frames, back to back, no reply wait.
    fn handshake(&self) {
        let mut f = self.file.lock();
        for (cls, dir) in [(0x02u8, 0x00u8), (0x2a, 0x00), (0x2a, 0x80)] {
            let mut b = pro(cls, 0x00, &[]);
            b[9] = dir;
            crc(&mut b);
            let _ = self.raw_write(&mut f, &b);
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn battery_query(&self) {
        let _ = self.send(&pro(0x21, 0x00, &[0x00]));
    }
    fn charging_query(&self) {
        let _ = self.send(&pro(0x2a, 0x00, &[]));
    }

    /// razer_blackshark_v3_prime_caches: one GET per class the headset never pushes unprompted.
    fn prime(&self) {
        const GETTERS: [u8; 13] = [0x02, 0x19, 0x2c, 0x5f, 0x9e, 0x12, 0x16, 0x55, 0x5c, 0x65, 0x5d, 0x66, 0x6a];
        for g in GETTERS {
            if !self.alive.load(Ordering::SeqCst) {
                return;
            }
            let _ = self.send(&pro(g, 0x00, &[]));
            thread::sleep(Duration::from_millis(120));
        }
    }

    /// The driver's battery_query_work: re-query whichever of battery/charging is unknown,
    /// up to 4 tries 700 ms apart. Never a steady poll (sustained 0x21 polling drops the RF link).
    fn battery_requery(self: &Arc<Self>, gen: u64) {
        for _ in 0..4 {
            if self.link_gen.load(Ordering::SeqCst) != gen || !self.alive.load(Ordering::SeqCst) {
                return;
            }
            let (b, c) = {
                let c = self.cache.lock();
                (c.battery_pct, c.charging)
            };
            if b >= 0 && c >= 0 {
                return;
            }
            if b < 0 {
                self.battery_query();
            }
            if c < 0 {
                self.charging_query();
            }
            thread::sleep(Duration::from_millis(700));
        }
    }

    fn on_link_up(self: &Arc<Self>) {
        let gen = self.link_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let d = self.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(600));
            d.battery_requery(gen);
            // staggered past the battery re-query, like the driver (3.5 s after the push)
            thread::sleep(Duration::from_millis(500));
            if d.link_gen.load(Ordering::SeqCst) == gen {
                d.prime();
            }
        });
    }

    // ------------------------------------------------------------ attributes

    fn attrs(&self) -> Vec<&'static str> {
        let mut a = vec![
            "audio_function_button",
            "audio_prompts",
            "charge_level",
            "charge_status",
            "device_serial",
            "device_type",
            "eq_slot",
            "firmware_version",
            "game_chat_balance",
            "in_call_audio_mix",
            "mic_eq",
            "mic_eq_preset",
            "mic_mute",
            "v3pro_anc",
            "v3pro_headphone_eq",
            "v3pro_power_save",
            "v3pro_sidetone",
            "v3pro_thx_spatial_audio",
            "v3pro_ultra_low_latency",
            "version",
        ];
        if self.dongle() {
            a.push("indicator_led");
        }
        a.sort();
        a
    }

    fn read_attr(&self, attr: &str) -> io::Result<String> {
        if !self.attrs().contains(&attr) || attr == "mic_eq" {
            // mic_eq is write-only in the driver (0220)
            return Err(io::Error::from(if attr == "mic_eq" { io::ErrorKind::PermissionDenied } else { io::ErrorKind::NotFound }));
        }
        if attr == "device_serial" {
            return Ok(format!("{}\n", self.serial()));
        }
        let c = self.cache.lock().clone();
        let s = match attr {
            "version" => "hidraw".to_string(),
            "device_type" => format!("Razer BlackShark V3 Pro ({})", if self.dongle() { "Wireless" } else { "Wired" }),
            "firmware_version" => match c.fw {
                Some(v) => format!("v{}.{}.{}", v[0], v[1], v[2]),
                None => "v1.0".into(),
            },
            "charge_level" => (if c.battery_pct >= 0 { c.battery_pct * 255 / 100 } else { -1 }).to_string(),
            "charge_status" => c.charging.to_string(),
            "v3pro_sidetone" => c.sidetone.to_string(),
            "v3pro_thx_spatial_audio" => c.thx.to_string(),
            "v3pro_anc" => format!("{} {}", c.anc_mode, c.anc_level),
            "v3pro_ultra_low_latency" => c.ull.to_string(),
            "v3pro_power_save" => c.power_save.to_string(),
            "v3pro_headphone_eq" => c.eq_profile.to_string(),
            "mic_eq_preset" => c.mic_eq_preset.to_string(),
            "mic_mute" => c.mic_muted.to_string(),
            "eq_slot" => {
                let b: Vec<String> = c.eq_query_bands.iter().map(|v| v.to_string()).collect();
                format!("{} {}", c.eq_query_slot, b.join(" "))
            }
            "audio_function_button" => c.fn_button.to_string(),
            "game_chat_balance" => c.game_chat.to_string(),
            "in_call_audio_mix" => c.in_call_mix.to_string(),
            "audio_prompts" => c.prompts.to_string(),
            "indicator_led" => c.indicator_led.to_string(),
            _ => return Err(io::ErrorKind::NotFound.into()),
        };
        Ok(format!("{s}\n"))
    }

    /// Serial: queried on first read; a failed query is not cached (it answers 0xff until the RF
    /// link is up), so a later read retries.
    fn serial(&self) -> String {
        if self.cache.lock().serial.is_empty() {
            let _ = self.send(&get(0x00, self.dir()));
        }
        let s = self.cache.lock().serial.clone();
        if s.is_empty() { "BS_V3PRO_000000".into() } else { s }
    }

    fn write_attr(&self, attr: &str, raw: &[u8]) -> io::Result<()> {
        let text = String::from_utf8_lossy(raw);
        let text = text.trim();
        let inval = || io::Error::from(io::ErrorKind::InvalidInput);
        let int = || text.parse::<u64>().map_err(|_| inval());
        let ints = || -> io::Result<Vec<i64>> { text.split_whitespace().map(|t| t.parse::<i64>().map_err(|_| inval())).collect() };
        let dir = self.dir();
        match attr {
            "v3pro_sidetone" => {
                let v = int()?.min(15) as u8;
                self.send(&pro(0x99, 0x01, &[v, 0]))?;
                self.cache.lock().sidetone = v as i64;
            }
            "v3pro_thx_spatial_audio" => {
                let v = (int()? != 0) as u8;
                self.send(&pro(0x9e, 0x01, &[v, 0]))?;
                self.cache.lock().thx = v as i64;
            }
            "v3pro_anc" => {
                let v = ints()?;
                if v.len() != 2 || v[0] < 0 || v[1] < 0 {
                    return Err(inval());
                }
                let (mode, level) = (v[0], v[1].clamp(1, 4));
                let m = match mode {
                    1 => 0x01,
                    2 => 0x50,
                    _ => 0x00,
                };
                self.send(&pro(0x92, 0x02, &[m, level as u8]))?;
                let mut c = self.cache.lock();
                c.anc_mode = if mode > 2 { 0 } else { mode };
                c.anc_level = level;
            }
            "v3pro_ultra_low_latency" => {
                let v = (int()? != 0) as u8;
                self.send(&pro(0xdf, 0x01, &[v, 0]))?;
                self.cache.lock().ull = v as i64;
            }
            "v3pro_power_save" => {
                let v = int()?.min(60) as u8;
                self.send(&pro(0xac, 0x01, &[v, 0]))?;
                self.cache.lock().power_save = v as i64;
            }
            "v3pro_headphone_eq" => {
                let v = ints()?;
                if (v.len() != 1 && v.len() != 11) || !(0..EQ_SLOTS).contains(&v[0]) {
                    return Err(inval());
                }
                let custom: Option<[i64; 10]> = (v.len() == 11).then(|| std::array::from_fn(|i| v[1 + i].clamp(-6, 6)));
                let mut f = self.file.lock();
                for fr in eq_frames(v[0] as u8, custom.as_ref()) {
                    // write-only frames time out on the reply; only a real bus error aborts
                    self.send_locked(&mut f, &fr, Duration::from_millis(1000))?;
                }
                drop(f);
                self.cache.lock().eq_profile = v[0];
            }
            "eq_slot" => {
                let slot = int()? as i64;
                if slot >= EQ_SLOTS {
                    return Err(inval());
                }
                self.cache.lock().eq_query_slot = -1;
                self.handshake();
                let _ = self.send(&pro(0x15, 0x01, &[slot as u8]));
                // the 0x15 reply may arrive after an unrelated frame; wait for the slot itself
                let deadline = Instant::now() + Duration::from_millis(300);
                let mut c = self.cache.lock();
                while c.eq_query_slot != slot {
                    if self.cv.wait_until(&mut c, deadline).timed_out() {
                        break;
                    }
                }
            }
            "mic_eq" => {
                let v = ints()?;
                if v.len() != 10 {
                    return Err(inval());
                }
                let b: [i64; 10] = std::array::from_fn(|i| v[i].clamp(-6, 6));
                self.send(&mic_eq(&b, dir))?;
            }
            "mic_eq_preset" => {
                let v = int()?.min(3) as u8;
                self.send(&set(0x96, 0x20 + v, dir))?;
                self.cache.lock().mic_eq_preset = v as i64;
            }
            "indicator_led" if self.dongle() => {
                let v = int()?.min(2) as u8;
                self.send(&set(0xe6, v, dir))?;
                self.cache.lock().indicator_led = v as i64;
            }
            "audio_function_button" => {
                let v = int()?;
                if v > 0xff {
                    return Err(inval());
                }
                self.send(&set(0xea, v as u8, dir))?;
                self.cache.lock().fn_button = v as i64;
            }
            "game_chat_balance" => {
                let v = int()?.min(20) as u8;
                self.send(&pro(0xdc, 0x01, &[v, 0]))?;
                self.cache.lock().game_chat = v as i64;
            }
            "in_call_audio_mix" => {
                let v = int()?.min(2) as u8;
                self.send(&pro(0xdd, 0x01, &[v, 0]))?;
                self.cache.lock().in_call_mix = v as i64;
            }
            "audio_prompts" => {
                let v = (int()? != 0) as u8;
                self.send(&pro(0xe5, 0x02, &[0, v, 0]))?;
                self.cache.lock().prompts = v as i64;
            }
            _ => {
                return Err(io::Error::from(if self.attrs().contains(&attr) {
                    io::ErrorKind::PermissionDenied // read-only attribute
                } else {
                    io::ErrorKind::NotFound
                }))
            }
        }
        Ok(())
    }
}

/// Open /dev/<node> and bring it up the way razer_kraken_probe does.
fn open(node: &str, pid: &str) -> io::Result<Arc<Dev>> {
    let f = fs::OpenOptions::new().read(true).write(true).open(format!("/dev/{node}"))?;
    let mut rd = f.try_clone()?;
    let dev = Arc::new(Dev {
        node: node.to_string(),
        pid: pid.to_string(),
        file: Mutex::new(f),
        cache: Mutex::new(Cache::default()),
        cv: Condvar::new(),
        alive: AtomicBool::new(true),
        started: Instant::now(),
        link_gen: AtomicU64::new(0),
    });

    // reader: the ep 0x84 URB callback
    let d = dev.clone();
    thread::Builder::new().name(format!("bshid-rd-{node}")).spawn(move || {
        let mut buf = [0u8; LEN];
        loop {
            match rd.read(&mut buf) {
                Ok(n) if n == LEN && buf[0] == 0x02 => {
                    let ev = absorb(&mut d.cache.lock(), &buf);
                    d.cv.notify_all();
                    if let Some(Event::LinkUp) = ev {
                        d.on_link_up();
                    }
                }
                Ok(_) => {} // dial / consumer-control reports: hid-generic already handles those
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    d.alive.store(false, Ordering::SeqCst);
                    d.cv.notify_all();
                    return;
                }
            }
        }
    })?;

    // RF_WAKE keep-alive: output report 5 [05 00] every 3.5 s, first after 750 ms
    let d = dev.clone();
    thread::Builder::new().name(format!("bshid-ka-{node}")).spawn(move || {
        thread::sleep(Duration::from_millis(750));
        while d.alive.load(Ordering::SeqCst) {
            {
                let mut f = d.file.lock();
                let _ = d.raw_write(&mut f, &[0x05, 0x00]);
            }
            thread::sleep(Duration::from_millis(3500));
        }
    })?;

    // probe-time handshake + battery, then the deferred cache prime at 1.5 s
    let d = dev.clone();
    thread::Builder::new().name(format!("bshid-init-{node}")).spawn(move || {
        d.handshake();
        d.battery_query();
        let wait = Duration::from_millis(1500).saturating_sub(d.started.elapsed());
        thread::sleep(wait);
        d.prime();
    })?;

    Ok(dev)
}

// ---------------------------------------------------------------- discovery / registry

static DEVS: LazyLock<Mutex<Vec<Arc<Dev>>>> = LazyLock::new(|| Mutex::new(vec![]));

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// hidraw nodes of a BlackShark V3 Pro that no razerkraken driver owns:
/// (node, pid, usb interface number).
fn candidates() -> Vec<(String, String, String)> {
    let mut out = vec![];
    let Ok(rd) = fs::read_dir("/sys/class/hidraw") else { return out };
    for e in rd.flatten() {
        let node = e.file_name().to_string_lossy().to_string();
        let hid = e.path().join("device");
        let ue = read_trim(&hid.join("uevent")).unwrap_or_default();
        let mut id = "";
        let mut drv = "";
        for l in ue.lines() {
            if let Some(v) = l.strip_prefix("HID_ID=") { id = v }
            if let Some(v) = l.strip_prefix("DRIVER=") { drv = v }
        }
        // HID_ID=0003:00001532:00000577
        let p: Vec<String> = id.split(':').map(|s| s.trim_start_matches('0').to_lowercase()).collect();
        if p.len() != 3 || p[1] != VID {
            continue;
        }
        let pid = format!("{:0>4}", p[2]);
        if pid != PID_PRO && pid != PID_PRO_WIRED || drv == "razerkraken" {
            continue;
        }
        let ifn = fs::canonicalize(&hid)
            .ok()
            .and_then(|h| h.parent().map(|u| u.join("bInterfaceNumber")))
            .and_then(|f| read_trim(&f))
            .unwrap_or_default();
        out.push((node, pid, ifn));
    }
    // the Razer vendor interface is #5; prefer it if a model ever exposes several HID interfaces
    out.sort_by_key(|(n, _, i)| (i != "05", n.clone()));
    let mut seen = vec![];
    out.retain(|(_, p, _)| {
        let first = !seen.contains(p);
        seen.push(p.clone());
        first
    });
    out
}

/// Attribute names of a virtual device dir (None if `p` is not one).
pub fn list(p: impl AsRef<Path>) -> Option<Vec<String>> {
    let p = p.as_ref();
    if !p.starts_with(VROOT) {
        return None;
    }
    Some(route(p).map(|(d, _)| d.attr_list()).unwrap_or_default())
}

/// Keep the registry in sync with what is plugged in. Called from razerctl's device scan.
/// RAZERCTL_NO_HIDRAW=1 disables the fallback entirely.
pub fn refresh() -> Vec<Arc<Dev>> {
    if std::env::var_os("RAZERCTL_NO_HIDRAW").is_some() {
        return vec![];
    }
    let want = candidates();
    let mut devs = DEVS.lock();
    devs.retain(|d| d.alive.load(Ordering::SeqCst) && want.iter().any(|(n, p, _)| *n == d.node && *p == d.pid));
    for (node, pid, _) in want {
        if devs.iter().any(|d| d.node == node) {
            continue;
        }
        match open(&node, &pid) {
            Ok(d) => {
                eprintln!("razerctl: BlackShark V3 Pro on /dev/{node} without razerkraken, using the hidraw backend");
                devs.push(d);
            }
            Err(e) => eprintln!("razerctl: /dev/{node}: {e} (hidraw backend not available)"),
        }
    }
    devs.clone()
}

impl Dev {
    pub fn path(&self) -> String {
        format!("{VROOT}/{}", self.node)
    }
    pub fn pid(&self) -> &str {
        &self.pid
    }
    pub fn conn(&self) -> &'static str {
        if self.dongle() { "wireless" } else { "wired" }
    }
    pub fn attr_list(&self) -> Vec<String> {
        self.attrs().into_iter().map(String::from).collect()
    }
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

/// (device, attribute) for a path under the virtual root.
fn route(p: &Path) -> Option<(Arc<Dev>, String)> {
    let rest = p.strip_prefix(VROOT).ok()?;
    let mut it = rest.components();
    let node = it.next()?.as_os_str().to_string_lossy().to_string();
    let attr = it.next().map(|c| c.as_os_str().to_string_lossy().to_string()).unwrap_or_default();
    let dev = DEVS.lock().iter().find(|d| d.node == node && d.is_alive()).cloned()?;
    Some((dev, attr))
}

fn gone() -> io::Error {
    io::Error::from_raw_os_error(19) // ENODEV
}

// ---------------------------------------------------------------- fs-like API used by razerctl

/// fs::read_to_string, but paths under the virtual root read the hidraw backend.
pub fn read(p: impl AsRef<Path>) -> io::Result<String> {
    let p = p.as_ref();
    if !p.starts_with(VROOT) {
        return fs::read_to_string(p);
    }
    let (d, attr) = route(p).ok_or_else(gone)?;
    d.read_attr(&attr)
}

pub fn read_bytes(p: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let p = p.as_ref();
    if !p.starts_with(VROOT) {
        return fs::read(p);
    }
    read(p).map(String::into_bytes)
}

/// fs::write (to an existing attribute), routed the same way.
pub fn write(p: impl AsRef<Path>, v: impl AsRef<[u8]>) -> io::Result<()> {
    let p = p.as_ref();
    if !p.starts_with(VROOT) {
        let mut f = fs::OpenOptions::new().write(true).open(p)?;
        return f.write_all(v.as_ref());
    }
    let (d, attr) = route(p).ok_or_else(gone)?;
    d.write_attr(&attr, v.as_ref())
}

pub fn exists(p: impl AsRef<Path>) -> bool {
    let p = p.as_ref();
    if !p.starts_with(VROOT) {
        return p.exists();
    }
    match route(p) {
        Some((d, attr)) => attr.is_empty() || d.attrs().contains(&attr.as_str()),
        None => false,
    }
}

// ---------------------------------------------------------------- standalone CLI

/// `razerctl --hid [attr [value]]`: drive the headset over hidraw without the GUI.
/// No arguments prints every readable attribute.
pub fn cli(args: &[String]) -> i32 {
    let devs = refresh();
    let Some(d) = devs.first().cloned() else {
        eprintln!("No BlackShark V3 Pro on hidraw without razerkraken (is the driver still bound? check /dev/hidraw* permissions)");
        return 1;
    };
    // let the probe handshake + cache prime finish (~1.5 s + 13 x 120 ms)
    let settle = Duration::from_millis(3600);
    thread::sleep(settle.saturating_sub(d.started.elapsed()));
    let path = PathBuf::from(d.path());
    match args {
        [] => {
            println!("/dev/{} (1532:{}, {})", d.node, d.pid, d.conn());
            for a in d.attrs() {
                if a == "mic_eq" {
                    continue;
                }
                match read(path.join(a)) {
                    Ok(v) => println!("{a:26} {}", v.trim()),
                    Err(e) => println!("{a:26} <{e}>"),
                }
            }
            0
        }
        [attr] => match read(path.join(attr)) {
            Ok(v) => {
                println!("{}", v.trim());
                0
            }
            Err(e) => {
                eprintln!("{attr}: {e}");
                1
            }
        },
        [attr, value @ ..] => match write(path.join(attr), value.join(" ")) {
            Ok(()) => {
                if let Ok(v) = read(path.join(attr)) {
                    println!("{attr} = {}", v.trim());
                }
                0
            }
            Err(e) => {
                eprintln!("{attr}: {e}");
                1
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xor_ok(b: &[u8; LEN]) -> bool {
        b[..62].iter().fold(0, |a, x| a ^ x) == b[62]
    }

    #[test]
    fn pro_frame_layout() {
        let b = pro(0x92, 0x02, &[0x50, 4]);
        assert_eq!(&b[..16], &[0x02, 0, 0x60, 0, 0, 0, 6, 0, 0, 0x80, 0x92, 0, 0x02, 0x50, 4, 0]);
        assert!(xor_ok(&b));
    }

    #[test]
    fn set_and_get_frames() {
        let s = set(0x96, 0x22, 0x80);
        assert_eq!(&s[..14], &[0x02, 0, 0x60, 0, 0, 0, 5, 0, 0, 0x80, 0x96, 0, 1, 0x22]);
        assert!(xor_ok(&s));
        let g = get(0x00, 0x00);
        assert_eq!(g[6], 4);
        assert_eq!(g[9], 0);
        assert!(xor_ok(&g));
    }

    #[test]
    fn bands_sign_magnitude() {
        assert_eq!(band(-6), 0x86);
        assert_eq!(band(9), 6);
        assert_eq!(unband(0x83), -3);
        let m = mic_eq(&[-5, -4, -4, -3, -2, 1, 2, 3, 3, 3], 0x80);
        assert_eq!(&m[10..23], &[0x97, 0, 0x0a, 0x85, 0x84, 0x84, 0x83, 0x82, 1, 2, 3, 3, 3]);
        assert!(xor_ok(&m));
    }

    #[test]
    fn eq_custom_keeps_slot_identity() {
        let f = eq_frames(4, Some(&[1, 2, 3, 4, 5, 6, -1, -2, -3, -4]));
        assert_eq!(f[1][10], 0x95);
        assert_eq!(f[1][13], 4); // slot
        assert_eq!(&f[1][14..24], &[1, 2, 3, 4, 5, 6, 0x81, 0x82, 0x83, 0x84]);
        assert_eq!(&f[4][13..20], &EQ_COMMIT[4][..7]);
        let f7 = eq_frames(7, None);
        assert_eq!(f7[2][13], 7);
        assert!(f.iter().chain(f7.iter()).all(xor_ok));
    }

    #[test]
    fn cache_absorbs_pushes() {
        let mut c = Cache::default();
        let mut d = [0u8; LEN];
        d[0] = 2;
        d[11] = 0x02;
        d[12] = 1;
        d[10] = 0x5f;
        d[13] = 1;
        absorb(&mut c, &d);
        assert_eq!(c.ull, 1);
        d[10] = 0x12;
        d[12] = 2;
        d[13] = 0x50;
        d[14] = 3;
        absorb(&mut c, &d);
        assert_eq!((c.anc_mode, c.anc_level), (2, 3));
        // EQ meta reply for a non-active slot must not move the active preset
        d[10] = 0x60;
        d[12] = 6;
        d[13] = 2;
        d[16] = 0;
        absorb(&mut c, &d);
        assert_eq!(c.eq_profile, -1);
        d[16] = 1;
        absorb(&mut c, &d);
        assert_eq!(c.eq_profile, 2);
        // 0xff capability replies are ignored
        d[10] = 0x21;
        d[12] = 1;
        d[13] = 0xff;
        absorb(&mut c, &d);
        assert_eq!(c.battery_pct, -1);
        d[10] = 0x20;
        d[13] = 1;
        assert!(matches!(absorb(&mut c, &d), Some(Event::LinkUp)));
    }
}
