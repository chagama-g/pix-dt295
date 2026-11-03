//! The BonDriver object, behind the C++ shim's `IBonDriver2` methods.
//!
//! Threads: TVTest calls `SetChannel` and `GetTsStream` on its streaming thread and
//! `OpenTuner` / `CloseTuner` / `Release` on another; the idle keeper and the
//! core's stream worker run their own. The [`Device`] and everything that goes
//! with it live in one `Mutex`; what the host polls
//! (`GetSignalLevel`, `GetCurSpace`, `IsTunerOpening`, the FIFO) is readable
//! without it, so a long `SetChannel` never stalls those calls. The idle keeper may
//! tune too: a stream started over for the B-CAS card, a unit opened again.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dt295::{CardWatch, Device, Live, System, TsSink};
use tracing::level_filters::LevelFilter;
use tracing::{info, warn};

use crate::channels::{Channel, Space};
use crate::config::{Config, DeviceSel, ModulePaths, decode_ini, parse_ini};
use crate::fifo::{TS_CHUNK, TS_FIFO_SIZE, TsFifo};
use crate::logging::{self, LogSession};
use crate::persist::Persist;
use crate::sys;

/// `GetCurSpace` / `GetCurChannel` before a tune.
pub const NO_SELECTION: u32 = 0xFFFF_FFFF;
/// `WaitTsStream` results (the Win32 wait codes).
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 0x102;
pub const WAIT_ABANDONED: u32 = 0x80;
/// A `WaitTsStream` timeout meaning "forever" (as is `0`).
pub const INFINITE: u32 = 0xFFFF_FFFF;

/// Attempts and pause of the device open: after a close (or a crash) the WinUSB
/// handle can linger for a moment, and a fresh open fails until the OS releases it.
const OPEN_ATTEMPTS: usize = 5;
const OPEN_RETRY_PAUSE: Duration = Duration::from_millis(400);
/// How often a unit that went away is looked for again.
const REOPEN_EVERY: Duration = Duration::from_secs(2);
/// How often an open unit that is not streaming is checked to be still plugged in (a
/// stream's worker notices an unplug itself).
const PRESENCE_EVERY: Duration = Duration::from_secs(5);

/// `GetTunerName`.
pub static TUNER_NAME: [u16; 10] = ascii_wide("PIX-DT295");

const fn ascii_wide<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i] as u16;
        i += 1;
    }
    assert!(b.len() < N, "room for the NUL");
    out
}

fn wide_z(s: &str) -> Box<[u16]> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic caught at the FFI boundary may have poisoned a lock; the driver keeps
    // working with the state as it was (each update is self-contained).
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// NUL-terminated UTF-16 names of a space and its channels: built once, so the
/// pointers `EnumTuningSpace` / `EnumChannelName` return stay valid (and stable)
/// for the driver's lifetime.
#[derive(Debug)]
struct SpaceNames {
    name: Box<[u16]>,
    channels: Vec<Box<[u16]>>,
}

/// What the host polls without the device lock (`GetSignalLevel`, `GetCurSpace`,
/// `IsTunerOpening`, the FIFO), shared with the idle keeper, which may tune again on its
/// own (a stream started over, a unit opened again).
#[derive(Debug)]
struct Shared {
    fifo: TsFifo,
    /// The open device's live values (`GetSignalLevel` without the device lock).
    live: Mutex<Option<Live>>,
    /// C/N in dB (`f32` bits): the live value while streaming, else the tune-time one.
    last_level: AtomicU32,
    cur_space: AtomicU32,
    cur_channel: AtomicU32,
    opening: AtomicBool,
}

impl Shared {
    fn set_last_level(&self, db: f32) {
        self.last_level.store(db.to_bits(), Ordering::Relaxed);
    }

    /// TS lost because the host did not read the FIFO in time (logged per stream;
    /// separates host-side drops from the device TS's own continuity errors).
    fn log_fifo_drops(&self) {
        let dropped = self.fifo.take_dropped();
        if dropped > 0 {
            warn!("TS FIFO overflow: {dropped} bytes dropped (the host did not read in time)");
        }
    }
}

/// The stream's way into the TS FIFO.
fn fifo_sink(sh: &Arc<Shared>) -> TsSink {
    let sh = Arc::clone(sh);
    Box::new(move |ts: &[u8]| sh.fifo.push(ts))
}

/// Everything the device lock guards.
#[derive(Debug)]
struct Core {
    sh: Arc<Shared>,
    firmware: PathBuf,
    trc_firmware: Option<PathBuf>,
    device: DeviceSel,
    dev: Option<Device>,
    streaming: bool,
    /// A re-init after a firmware fault failed: stop retrying until re-open.
    recovery_failed: bool,
    spaces: Vec<Space>,
    persist: Persist,
    /// The channel the host asked for last: what a start-over or a re-open tunes again.
    selected: Option<(u32, u32)>,
    /// When to start the stream over (per opened device).
    watch: Option<CardWatch>,
    /// The device went away (unplugged) while open: open it again once it is back.
    lost: bool,
    next_reopen: Option<Instant>,
    /// The last check that an open device not streaming is still plugged in.
    last_presence: Instant,
}

impl Core {
    fn save_state(&mut self) {
        self.persist.save(self.dev.as_ref());
    }

    fn snapshot_psi(&mut self) {
        if let Some(dev) = self.dev.as_mut() {
            self.persist.snapshot_psi(dev);
        }
    }

    fn idle_tick(&mut self) {
        if !self.streaming
            && let Some(dev) = self.dev.as_mut()
        {
            dev.idle_tick(); // the stream worker keeps the session alive while streaming
        }
    }

    /// Open the configured unit (with retries: after a close or a crash the WinUSB
    /// handle can linger for a moment), select the trc firmware, hand over the saved
    /// state, and boot it. `who` names the caller in the log.
    fn open_device(&mut self, who: &str) -> Option<Device> {
        let open = |sel: &DeviceSel| match sel {
            DeviceSel::Index(n) => Device::open(*n),
            DeviceSel::Serial(s) => Device::open_serial(s),
        };
        let mut opened = open(&self.device);
        for _ in 1..OPEN_ATTEMPTS {
            if opened.is_ok() {
                break;
            }
            thread::sleep(OPEN_RETRY_PAUSE);
            opened = open(&self.device);
        }
        let mut dev = match opened {
            Ok(dev) => dev,
            Err(e) => {
                warn!("{who}: cannot open device ({}): {e}", self.device);
                return None;
            }
        };
        info!(
            "{who}: device serial {} ({})",
            dev.serial().unwrap_or("unknown"),
            self.device
        );
        // The System1 (EP 0x81) viewing path when a trc firmware is configured. This
        // MUST precede start(): it picks the front-end power-on sequence from it.
        if let Some(trc) = &self.trc_firmware {
            dev.set_trc_firmware(Some(trc.clone()));
        }
        self.persist.apply(&mut dev); // link key + PSI; a cold boot drops the key
        match dev.start(&self.firmware) {
            Ok(()) => Some(dev),
            Err(e) => {
                warn!("{who}: start failed ({e})");
                None // dropping `dev` closes it
            }
        }
    }

    /// Make a booted device the open one.
    fn adopt(&mut self, dev: Device) {
        *lock(&self.sh.live) = Some(dev.live());
        self.watch = Some(CardWatch::new(&dev.live()));
        self.dev = Some(dev);
        self.sh.opening.store(true, Ordering::Release);
        self.recovery_failed = false;
        self.lost = false;
        self.last_presence = Instant::now();
    }

    /// Stop the running stream (keeping what it taught: PSI, the link key); the firmware
    /// goes back to idle.
    fn stop_stream(&mut self) {
        if !self.streaming {
            return;
        }
        if let Some(dev) = self.dev.as_mut()
            && let Err(e) = dev.stream_stop()
        {
            warn!("stream stop failed ({e})");
        }
        self.streaming = false;
        self.sh.log_fifo_drops();
        // Per stop, so a killed host still keeps what the scan so far has seen
        // (writes only when the PSI or the link key changed).
        self.snapshot_psi();
        self.save_state();
    }

    /// `SetChannel(space, channel)`: stop the previous stream, re-init a faulted
    /// device (once per open), tune, and start the stream of a locked channel.
    /// True only when a stream runs.
    fn set_channel(&mut self, space: u32, channel: u32) -> bool {
        let (si, ci) = (space as usize, channel as usize);
        let Some(&Channel {
            sys,
            freq_hz,
            tsid,
            sid,
            ..
        }) = self.spaces.get(si).and_then(|s| s.channels.get(ci))
        else {
            return false;
        };
        if self.dev.is_none() && self.lost {
            self.try_reopen(); // re-plugged, and a tune came before the keeper saw it back
        }
        if self.dev.is_none() {
            return false;
        }

        // A channel change restarts the stream: stop the previous one (returns the
        // firmware to idle) before re-tuning.
        let t_begin = Instant::now();
        self.stop_stream();
        let Some(dev) = self.dev.as_mut() else {
            return false;
        };
        // A faulted (0xFFFF) or non-acking firmware makes every tune fail fast and
        // the host give up. Try one full re-init so the session recovers without a
        // re-plug; only once per open (if it fails the device needs a re-plug).
        if !self.recovery_failed && dev.needs_recovery() {
            info!("device faulted / not acking -- attempting re-init");
            match dev.start(&self.firmware) {
                Ok(()) => info!("re-init succeeded; device recovered"),
                Err(e) => {
                    info!("re-init FAILED; a USB re-plug is needed ({e})");
                    self.recovery_failed = true;
                }
            }
        }
        let t_stop = Instant::now();
        let mut t_tune;
        let mut retried = false;
        let (tuned, locked, cn_db, cn_raw) = loop {
            let Some(dev) = self.dev.as_mut() else {
                return false;
            };
            let tuned = match sys {
                System::Terrestrial => dev.tune_t(freq_hz),
                System::Satellite => dev.tune_s(freq_hz, tsid),
            };
            t_tune = Instant::now();
            let mut locked = false;
            let mut cn_db = -1.0f32; // for the log line below
            let mut cn_raw = 0u32;
            match &tuned {
                Err(e) => warn!("tune failed ({e})"),
                Ok(()) => {
                    self.sh.cur_space.store(space, Ordering::Release);
                    self.sh.cur_channel.store(channel, Ordering::Release);
                    // The tune already waited for lock; read it once for the level.
                    if let Ok(st) = dev.read_status(sys) {
                        let db = st.cn_mdb as f32 / 1000.0;
                        self.sh.set_last_level(db);
                        locked = st.locked;
                        cn_db = db;
                        cn_raw = st.raw;
                    }
                    // Only stream a locked channel: on an empty one the bring-up and PAT
                    // probe would take tens of seconds for nothing and time a scan out.
                    if locked {
                        // every service of the TS, unless the ini names one
                        dev.set_service_id(sid);
                        self.sh.fifo.purge();
                        match dev.stream_start(fifo_sink(&self.sh)) {
                            Ok(()) => self.streaming = true,
                            Err(e) => warn!("stream start failed ({e})"),
                        }
                    }
                }
            }
            // The firmware faulted during this tune -- seen when a unit is opened about a
            // minute after the previous process closed it: that session's timeout comes in
            // the new one. Start it again and tune once more (about 4 s, within the host's
            // 10 s for a channel change) instead of failing this one.
            let faulted = !self.streaming && self.dev.as_mut().is_some_and(|d| d.needs_recovery());
            if faulted && !retried && !self.recovery_failed {
                retried = true;
                info!("the firmware faulted during the tune -- re-init and tune once more");
                if let Some(dev) = self.dev.as_mut() {
                    match dev.start(&self.firmware) {
                        Ok(()) => continue,
                        Err(e) => {
                            info!("re-init FAILED; a USB re-plug is needed ({e})");
                            self.recovery_failed = true;
                        }
                    }
                }
            }
            break (tuned, locked, cn_db, cn_raw);
        };
        let t_end = Instant::now();
        let timing = [
            t_stop - t_begin,
            t_tune - t_stop,
            t_end - t_tune,
            t_end - t_begin,
        ];
        info!(
            "{}",
            set_channel_line(freq_hz, locked, cn_db, cn_raw, timing)
        );
        // Success means a stream is running (a locked channel whose stream failed to
        // start must not look "tuned" with no TS).
        tuned.is_ok() && locked && self.streaming
    }

    /// The idle keeper's checks, once a second: a unit that went away is closed and
    /// opened again once it is back (and the channel tuned again); a stream is started
    /// over for the B-CAS card or a firmware that stopped acking ([`CardWatch`]).
    fn watch_tick(&mut self) {
        let now = Instant::now();
        let gone = match self.dev.as_ref() {
            Some(dev) if dev.live().device_lost() => true,
            // a stream's worker notices an unplug itself; an idle device is looked for
            Some(_)
                if !self.streaming && now.duration_since(self.last_presence) >= PRESENCE_EVERY =>
            {
                self.last_presence = now;
                !self.present()
            }
            _ => false,
        };
        if gone {
            warn!("the device is gone (unplugged?): closed it; it is opened again when it is back");
            self.drop_device();
        }
        if self.lost {
            if self.next_reopen.is_none_or(|t| now >= t) {
                self.next_reopen = Some(now + REOPEN_EVERY);
                if self.try_reopen()
                    && let Some((space, channel)) = self.selected
                {
                    self.set_channel(space, channel);
                }
            }
            return;
        }
        if !self.streaming {
            return;
        }
        let stalled = self.dev.as_mut().is_some_and(Device::needs_recovery);
        let over = match (self.dev.as_ref(), self.watch.as_mut()) {
            (Some(dev), Some(w)) => w.poll(&dev.live(), stalled, now),
            _ => None,
        };
        if let Some(over) = over {
            info!("{}: starting the stream over", over.why);
            self.start_over(over.in_place);
            if let Some(w) = self.watch.as_mut() {
                w.started_over(Instant::now());
            }
        }
    }

    /// Whether the configured unit is plugged in (true when that cannot be told).
    fn present(&self) -> bool {
        dt295::present_serials().map_or(true, |serials| match &self.device {
            DeviceSel::Serial(s) => serials.iter().any(|p| p.eq_ignore_ascii_case(s)),
            DeviceSel::Index(n) => serials.len() > *n,
        })
    }

    /// Close a device that went away; [`Core::watch_tick`] opens it again.
    fn drop_device(&mut self) {
        if self.streaming {
            if let Some(dev) = self.dev.as_mut() {
                let _ = dev.stream_stop(); // the worker ends; the device does not answer
            }
            self.streaming = false;
            self.sh.log_fifo_drops();
        }
        self.snapshot_psi();
        self.save_state();
        *lock(&self.sh.live) = None;
        self.dev = None; // closes it
        self.watch = None;
        self.lost = true;
        self.next_reopen = None;
    }

    /// Open a unit that went away, if it is back. True when it is open again.
    fn try_reopen(&mut self) -> bool {
        if !self.present() {
            return false;
        }
        match self.open_device("reopen") {
            Some(dev) => {
                info!("the device is back: opened it again");
                self.adopt(dev);
                true
            }
            None => false,
        }
    }

    /// Start the stream of the selected channel over on a re-initialized device: what
    /// brings a B-CAS card back (a channel change alone does not).
    fn start_over(&mut self, in_place: bool) {
        let Some((space, channel)) = self.selected else {
            return;
        };
        self.stop_stream();
        let Some(dev) = self.dev.as_mut() else {
            return;
        };
        if in_place && dev.resume_in_place(fifo_sink(&self.sh)) {
            self.streaming = true;
            return;
        }
        if let Err(e) = dev.start(&self.firmware) {
            warn!("re-init failed ({e})");
        }
        if !self.set_channel(space, channel) {
            warn!("the channel did not stream again");
        }
    }
}

/// The idle session keeper thread and its stop signal (dropping `stop` ends it).
#[derive(Debug)]
struct IdleKeeper {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

/// One BonDriver instance.
#[derive(Debug)]
pub struct Driver {
    core: Arc<Mutex<Core>>,
    sh: Arc<Shared>,
    names: Vec<SpaceNames>,
    /// The space `SetChannel(BYTE)` maps UHF channel numbers into.
    uhf_space: Option<usize>,
    /// The buffer behind `GetTsStream(BYTE **)`.
    out: Mutex<Box<[u8]>>,
    idle: Mutex<Option<IdleKeeper>>,
    log: Mutex<Option<LogSession>>,
}

// The FFI hands one `&Driver` to every host thread.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Driver>();
};

impl Driver {
    /// The driver as `CreateBonDriver` makes it: the ini next to the DLL, its log
    /// file, the state file.
    pub fn new() -> Self {
        logging::init();
        let paths = sys::module_path()
            .map(|p| ModulePaths::from_module_path(&p))
            .unwrap_or_default();
        let ini_path = paths.sibling(".ini");
        let text = std::fs::read(&ini_path)
            .map(|b| decode_ini(&b))
            .unwrap_or_default(); // no ini: all defaults
        let (ini, ini_error) = match parse_ini(&text) {
            Ok(ini) => (ini, None),
            Err(e) => (ini::Ini::new(), Some(e)),
        };
        let cfg = Config::from_ini(&ini, &paths);
        logging::set_level(*cfg.log_level.as_ref().unwrap_or(&LevelFilter::INFO));
        let log = cfg.log_file.as_deref().and_then(logging::open_log_file);
        info!(
            "version {} ({})",
            env!("DT295_VERSION"),
            std::env::consts::ARCH
        );
        if let Err(v) = &cfg.log_level {
            warn!("ini: LogLevel={v} is none of error, warn, info, debug, trace; using info");
        }
        if let Some(e) = ini_error {
            warn!(
                "ini: cannot parse {} ({e}); using the defaults",
                ini_path.display()
            );
        }
        Self::with_config(cfg, log)
    }

    /// A driver for `cfg` (the state is loaded now: the spaces are built).
    pub fn with_config(cfg: Config, log: Option<LogSession>) -> Self {
        let spaces = cfg.spaces;
        let mut persist = Persist::new(cfg.state_file);
        persist.load();
        let names = spaces
            .iter()
            .map(|s| SpaceNames {
                name: wide_z(&s.name),
                channels: s.channels.iter().map(|c| wide_z(&c.name)).collect(),
            })
            .collect();
        let uhf_space = spaces.iter().position(|s| s.name == "UHF");
        let sh = Arc::new(Shared {
            fifo: TsFifo::new(TS_FIFO_SIZE),
            live: Mutex::new(None),
            last_level: AtomicU32::new(0f32.to_bits()),
            cur_space: AtomicU32::new(NO_SELECTION),
            cur_channel: AtomicU32::new(NO_SELECTION),
            opening: AtomicBool::new(false),
        });
        Self {
            core: Arc::new(Mutex::new(Core {
                sh: Arc::clone(&sh),
                firmware: cfg.firmware,
                trc_firmware: cfg.trc_firmware,
                device: cfg.device,
                dev: None,
                streaming: false,
                recovery_failed: false,
                spaces,
                persist,
                selected: None,
                watch: None,
                lost: false,
                next_reopen: None,
                last_presence: Instant::now(),
            })),
            sh,
            names,
            uhf_space,
            out: Mutex::new(vec![0; TS_CHUNK].into_boxed_slice()),
            idle: Mutex::new(None),
            log: Mutex::new(log),
        }
    }

    // ---- tuner ----

    /// `OpenTuner`: open (with retries), select the trc firmware and hand over the
    /// saved state, then boot. Already open: just make sure the keeper runs.
    pub fn open_tuner(&self) -> bool {
        let ok = {
            let mut core = lock(&self.core);
            if core.dev.is_some() {
                true
            } else if let Some(dev) = core.open_device("OpenTuner") {
                core.adopt(dev);
                true
            } else {
                false
            }
        };
        if ok {
            self.start_idle_keeper();
        }
        ok
    }

    /// `CloseTuner`: stop the stream, save, close; the selection is forgotten.
    pub fn close_tuner(&self) {
        self.stop_idle_keeper(); // before taking the device lock: the keeper uses it too
        {
            let mut core = lock(&self.core);
            if core.dev.is_some() {
                core.stop_stream();
                core.snapshot_psi();
                core.save_state();
                *lock(&self.sh.live) = None;
                core.dev = None; // closes the device
            }
            self.sh.opening.store(false, Ordering::Release);
            core.recovery_failed = false;
            core.selected = None;
            core.watch = None;
            core.lost = false;
            self.sh.cur_space.store(NO_SELECTION, Ordering::Release);
            self.sh.cur_channel.store(NO_SELECTION, Ordering::Release);
            self.sh.set_last_level(0.0);
        }
        self.purge_ts_stream();
    }

    /// `IsTunerOpening`.
    pub fn is_tuner_opening(&self) -> bool {
        self.sh.opening.load(Ordering::Acquire)
    }

    // ---- idle session keeper ----
    //
    // The System1 secure session must see a keepalive within a minute or the firmware
    // faults to 0xFFFF, also while idle. While streaming the core's worker sends it,
    // during a tune the tune path does; when nothing at all happens (TVTest showing
    // a scan-result dialog, parked on an empty channel) this thread does, ticking
    // once a second (Device::idle_tick only touches the device when one is due).
    // The same tick starts a stream over for the B-CAS card (a card pulled or put in
    // leaves the stream running scrambled) and opens a unit again after a re-plug,
    // tuning the selected channel again (Core::watch_tick).
    // try_lock: it never stalls behind a long SetChannel (which keeps the session
    // alive itself) or CloseTuner.

    fn start_idle_keeper(&self) {
        let mut idle = lock(&self.idle);
        if idle.is_some() {
            return;
        }
        let (stop, stopped) = mpsc::channel();
        let core = Arc::clone(&self.core);
        match thread::Builder::new()
            .name("BonDriver_DT295 idle".into())
            .spawn(move || idle_keeper(&core, &stopped))
        {
            Ok(thread) => *idle = Some(IdleKeeper { stop, thread }),
            Err(e) => warn!("idle keeper: cannot start its thread ({e})"),
        }
    }

    fn stop_idle_keeper(&self) {
        let keeper = lock(&self.idle).take();
        if let Some(IdleKeeper { stop, thread }) = keeper {
            drop(stop);
            let _ = thread.join(); // at most one in-flight keepalive
        }
    }

    // ---- channels ----

    /// `GetTunerName`.
    pub fn tuner_name(&self) -> &'static [u16] {
        &TUNER_NAME
    }

    /// `EnumTuningSpace`: the NUL-terminated name, or `None` past the last space.
    pub fn enum_tuning_space(&self, space: u32) -> Option<&[u16]> {
        self.names.get(space as usize).map(|s| &*s.name)
    }

    /// `EnumChannelName`: the NUL-terminated name, or `None` past the last channel.
    pub fn enum_channel_name(&self, space: u32, channel: u32) -> Option<&[u16]> {
        self.names
            .get(space as usize)?
            .channels
            .get(channel as usize)
            .map(|c| &**c)
    }

    /// `GetCurSpace` ([`NO_SELECTION`] until a tune succeeded).
    pub fn cur_space(&self) -> u32 {
        self.sh.cur_space.load(Ordering::Acquire)
    }

    /// `GetCurChannel` ([`NO_SELECTION`] until a tune succeeded).
    pub fn cur_channel(&self) -> u32 {
        self.sh.cur_channel.load(Ordering::Acquire)
    }

    /// `SetChannel(BYTE)` (IBonDriver v1): the UHF physical channel 13-62.
    pub fn set_channel_byte(&self, ch: u8) -> bool {
        match self.uhf_space {
            Some(space) if (13..=62).contains(&ch) => {
                self.set_channel(space as u32, u32::from(ch - 13))
            }
            _ => false,
        }
    }

    /// `SetChannel(space, channel)`: stop the previous stream, re-init a faulted
    /// device (once per open), tune, and start the stream of a locked channel.
    /// True only when a stream runs.
    pub fn set_channel(&self, space: u32, channel: u32) -> bool {
        let mut core = lock(&self.core);
        let known = core
            .spaces
            .get(space as usize)
            .is_some_and(|s| (channel as usize) < s.channels.len());
        if known {
            core.selected = Some((space, channel));
            if let Some(w) = core.watch.as_mut() {
                w.tuned();
            }
        }
        core.set_channel(space, channel)
    }

    /// `GetSignalLevel`: C/N in dB. Never touches the device (the host polls it
    /// continuously): while streaming the worker's live sample, else the value read
    /// at `SetChannel`.
    pub fn signal_level(&self) -> f32 {
        if let Some(live) = lock(&self.sh.live).as_ref()
            && live.is_streaming()
            && let Some(cn) = live.cn_mdb()
        {
            self.sh.set_last_level(cn as f32 / 1000.0);
        }
        f32::from_bits(self.sh.last_level.load(Ordering::Relaxed))
    }

    // ---- TS FIFO ----

    /// `WaitTsStream`: wait for TS data, `timeout_ms` (0 or INFINITE = forever).
    pub fn wait_ts_stream(&self, timeout_ms: u32) -> u32 {
        let timeout = match timeout_ms {
            0 | INFINITE => None,
            ms => Some(Duration::from_millis(u64::from(ms))),
        };
        if self.sh.fifo.wait(timeout) {
            WAIT_OBJECT_0
        } else {
            WAIT_TIMEOUT
        }
    }

    /// `GetReadyCount`: buffered chunks.
    pub fn ready_count(&self) -> u32 {
        self.sh.fifo.ready_count()
    }

    /// `GetTsStream(BYTE *)`: up to one chunk, handed to `copy` in (at most) two
    /// parts. Returns the byte count.
    pub fn pop_ts_with(&self, copy: impl FnOnce(&[u8], &[u8])) -> usize {
        self.sh.fifo.pop_with(TS_CHUNK, copy)
    }

    /// `GetTsStream(BYTE **)`: up to one chunk in the driver's own buffer, which
    /// stays valid (and unchanged) until the next call or the driver's release.
    /// Null and 0 when there is no data.
    pub fn ts_stream_ptr(&self) -> (*mut u8, usize) {
        let mut out = lock(&self.out);
        let n = self.sh.fifo.pop(&mut out);
        let p = if n > 0 {
            out.as_mut_ptr()
        } else {
            std::ptr::null_mut()
        };
        (p, n)
    }

    /// `PurgeTsStream`.
    pub fn purge_ts_stream(&self) {
        self.sh.fifo.purge();
    }
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Driver {
    /// `Release`: everything is stopped and joined here (never in DllMain).
    fn drop(&mut self) {
        self.close_tuner();
        drop(
            self.log
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }
}

/// The per-tune diagnostic line, in a fixed format (log analysis greps it). `timing` = stop, tune, start, total.
fn set_channel_line(
    freq_hz: u32,
    locked: bool,
    cn_db: f32,
    cn_raw: u32,
    timing: [Duration; 4],
) -> String {
    let [stop, tune, start, total] = timing.map(|d| d.as_millis());
    format!(
        "SetChannel {:.3} MHz: {} cn={cn_db:.1}dB (raw {cn_raw}) | timing ms: \
         stop={stop} tune={tune} start={start} total={total}",
        f64::from(freq_hz) / 1e6,
        if locked { "LOCK" } else { "nolock" },
    )
}

/// The idle keeper's loop: once a second, unless the device is busy.
fn idle_keeper(core: &Mutex<Core>, stop: &mpsc::Receiver<()>) {
    let mut ticks = 0u32;
    while let Err(RecvTimeoutError::Timeout) = stop.recv_timeout(Duration::from_secs(1)) {
        let mut core = match core.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => continue,
        };
        core.idle_tick();
        core.watch_tick();
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(3) {
            // A new link key soon after the first stream, in case the host is killed
            // (only writes when something changed). Held under the device lock, so it
            // never lands inside a timing-sensitive bring-up.
            core.save_state();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ModulePaths, parse_ini};
    use std::ffi::OsStr;

    fn driver(ini: &str) -> Driver {
        let paths =
            ModulePaths::from_module_path(OsStr::new(r"C:\nonexistent-dt295\BonDriver_DT295.dll"));
        let mut cfg = Config::from_ini(&parse_ini(ini).unwrap(), &paths);
        cfg.state_file = None;
        Driver::with_config(cfg, None)
    }

    fn utf16z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    #[test]
    fn set_channel_log_line() {
        let ms = Duration::from_millis;
        assert_eq!(
            set_channel_line(
                557_142_857,
                true,
                28.46,
                1234,
                [ms(1), ms(2003), ms(1500), ms(3504)]
            ),
            "SetChannel 557.143 MHz: LOCK cn=28.5dB (raw 1234) | timing ms: \
             stop=1 tune=2003 start=1500 total=3504"
        );
        assert_eq!(
            set_channel_line(1_318_000_000, false, -1.0, 0, [ms(0); 4]),
            "SetChannel 1318.000 MHz: nolock cn=-1.0dB (raw 0) | timing ms: \
             stop=0 tune=0 start=0 total=0"
        );
    }

    #[test]
    fn tuner_name() {
        let d = driver("");
        assert_eq!(d.tuner_name(), &utf16z("PIX-DT295")[..]);
    }

    #[test]
    fn enumerates_spaces_and_channels() {
        let d = driver("[BS]\nBS01/TS0=1049480,0x4010,151\n");
        let names: Vec<_> = (0..)
            .map_while(|s| d.enum_tuning_space(s))
            .map(|w| String::from_utf16_lossy(&w[..w.len() - 1]))
            .collect();
        assert_eq!(names, ["UHF", "CATV", "BS"]);
        assert_eq!(d.enum_channel_name(0, 0), Some(&utf16z("13ch")[..]));
        assert_eq!(d.enum_channel_name(1, 50), Some(&utf16z("C63ch")[..]));
        assert_eq!(d.enum_channel_name(1, 51), None);
        assert_eq!(d.enum_channel_name(2, 0), Some(&utf16z("BS01/TS0")[..]));
        assert_eq!(d.enum_channel_name(3, 0), None);
        assert_eq!(d.enum_tuning_space(NO_SELECTION), None);
        // Stable pointers: the same storage every call.
        let a = d.enum_channel_name(2, 0).unwrap().as_ptr();
        let b = d.enum_channel_name(2, 0).unwrap().as_ptr();
        assert_eq!(a, b);
    }

    #[test]
    fn closed_tuner_state() {
        let d = driver("");
        assert!(!d.is_tuner_opening());
        assert_eq!(
            (d.cur_space(), d.cur_channel()),
            (NO_SELECTION, NO_SELECTION)
        );
        assert_eq!(d.signal_level(), 0.0);
        // Not open: no tune (and no device access).
        assert!(!d.set_channel(0, 14));
        assert!(!d.set_channel(9, 0));
        assert!(!d.set_channel_byte(27));
        d.close_tuner();
        assert_eq!(d.cur_space(), NO_SELECTION);
    }

    #[test]
    fn set_channel_byte_needs_uhf() {
        let d = driver("[Tuner]\nUHF=0\n");
        assert_eq!(d.uhf_space, None);
        assert!(!d.set_channel_byte(27));
        let d = driver("");
        assert_eq!(d.uhf_space, Some(0));
        assert!(!d.set_channel_byte(12));
        assert!(!d.set_channel_byte(63));
    }

    #[test]
    fn ts_reads() {
        let d = driver("");
        // No data: TRUE with size 0 and a null pointer at the FFI.
        assert_eq!(d.ts_stream_ptr(), (std::ptr::null_mut(), 0));
        assert_eq!(d.ready_count(), 0);
        assert_eq!(d.wait_ts_stream(10), WAIT_TIMEOUT);

        let ts: Vec<u8> = (0..TS_CHUNK + 188).map(|i| i as u8).collect();
        d.sh.fifo.push(&ts);
        assert_eq!(d.ready_count(), 2);
        assert_eq!(d.wait_ts_stream(0), WAIT_OBJECT_0);
        let (p, n) = d.ts_stream_ptr();
        assert_eq!(n, TS_CHUNK);
        // SAFETY: `p` points into the driver's TS_CHUNK-byte output buffer, which
        // nothing else touches until the next ts_stream_ptr call.
        let chunk = unsafe { std::slice::from_raw_parts(p, n) };
        assert_eq!(chunk, &ts[..TS_CHUNK]);
        assert_eq!(d.ready_count(), 1);

        let mut rest = Vec::new();
        let n = d.pop_ts_with(|a, b| {
            rest.extend_from_slice(a);
            rest.extend_from_slice(b);
        });
        assert_eq!(n, 188);
        assert_eq!(rest, &ts[TS_CHUNK..]);

        d.sh.fifo.push(&ts[..10]);
        d.purge_ts_stream();
        assert_eq!(d.ready_count(), 0);
        assert_eq!(d.wait_ts_stream(1), WAIT_TIMEOUT);
    }
}
