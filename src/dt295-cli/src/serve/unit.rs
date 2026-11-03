//! One served unit: a thread that holds the PIX-DT295 open for good, the way the
//! BonDriver does inside TVTest. It boots the firmware once, keeps the secure
//! session alive while idle, and for each request tunes and streams to the client's
//! connection until the client goes away (a recorder kills `dt295 tune`: the socket
//! closes and the stream is stopped properly), a new request takes the unit over, the
//! unit is unplugged, or the daemon stops. Unplugged, it waits for the unit to come
//! back and opens it again. A B-CAS card pulled or put in during a stream leaves it
//! running scrambled: the stream is started over on a booted device ([`CardWatch`]), as
//! is a stream whose firmware stopped acking.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dt295::state::State;
use dt295::{CardWatch, Device, System, TsSink};
use tracing::{info, warn};

use super::conf::Conf;
use super::proto::{Reply, Request, Tuning};
use crate::tuner::{key_journal, prior_keys, stopping, take_link_keys};

/// A request and the connection its TS goes to.
pub struct Job {
    pub req: Request,
    pub conn: UnixStream,
}

/// Idle cadence: the keepalive check (it only sends when one is due).
const TICK: Duration = Duration::from_secs(1);
/// How often to check that the unit is still plugged in.
const PRESENCE_EVERY: Duration = Duration::from_secs(5);
/// How long to wait before trying to open (or boot) the unit again.
const OPEN_RETRY: Duration = Duration::from_secs(5);
/// The streaming loop's cadence (client gone, new request, link key to save).
const POLL: Duration = Duration::from_millis(200);
/// TS buffered for a slow client, in the worker's reads (up to 64 KiB each): seconds
/// of TS. Beyond it the TS is dropped (and counted), so a client that stops reading
/// never stalls the USB reads.
const QUEUE_READS: usize = 256;
/// A stream that ran this long without a byte marks the device for a fresh boot
/// before the next tune (the remux can stop emitting TS after many bring-ups).
const SILENT: Duration = Duration::from_secs(10);

pub fn spawn(
    serial: String,
    conf: Arc<Conf>,
) -> std::io::Result<(mpsc::Sender<Job>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name(format!("unit {serial}"))
        .spawn(move || run(&serial, &conf, &rx))?;
    Ok((tx, thread))
}

fn run(serial: &str, conf: &Conf, jobs: &Receiver<Job>) {
    let span = tracing::info_span!("unit", serial);
    let _entered = span.enter();
    let mut unit = Unit::new(serial, conf);
    let mut next = None;
    while !stopping() {
        unit.open_if_due();
        let job = match next.take() {
            Some(job) => job,
            None => match jobs.recv_timeout(TICK) {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) => {
                    unit.idle();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
        };
        let job = yield_to_queued(job, jobs, serial);
        next = unit.serve(job, jobs);
    }
    unit.close();
}

/// Of a background request (`if-free`: a logo run, a channel scan) and one queued
/// right behind it, the one that takes units over goes first and the background one
/// is refused: started, it would only be taken over seconds later. Two background
/// requests: the first goes, the second is refused.
fn yield_to_queued(job: Job, jobs: &Receiver<Job>, serial: &str) -> Job {
    if !job.req.if_free {
        return job;
    }
    let Ok(other) = jobs.try_recv() else {
        return job;
    };
    let (go, mut refused) = if other.req.if_free {
        (job, other)
    } else {
        (other, job)
    };
    info!(
        "{}: refused, another request came with it",
        refused.req.to_line()
    );
    let why = format!("unit {serial} is taken by another request");
    let _ = reply(&mut refused.conn, &Reply::Busy(why));
    go
}

struct Unit<'a> {
    serial: &'a str,
    conf: &'a Conf,
    dev: Option<Device>,
    state_path: PathBuf,
    /// What the state file holds (the journal also writes keys to it directly).
    state: State,
    /// The journal wrote a key to the state file.
    pending_written: Arc<AtomicBool>,
    /// The last stream delivered nothing: boot again before the next tune.
    reboot: bool,
    /// When to start a stream over for the B-CAS card (per opened device).
    watch: Option<CardWatch>,
    next_open: Instant,
    absent_logged: bool,
    last_presence: Instant,
}

impl<'a> Unit<'a> {
    fn new(serial: &'a str, conf: &'a Conf) -> Self {
        Self {
            serial,
            conf,
            dev: None,
            state_path: conf.state_file(serial),
            state: State::default(),
            pending_written: Arc::new(AtomicBool::new(false)),
            reboot: false,
            watch: None,
            next_open: Instant::now(),
            absent_logged: false,
            last_presence: Instant::now(),
        }
    }

    fn open_if_due(&mut self) {
        if self.dev.is_none() && Instant::now() >= self.next_open {
            self.open();
        }
    }

    /// Open the unit, hand it the state file's link keys and PSI, and boot it.
    fn open(&mut self) {
        self.next_open = Instant::now() + OPEN_RETRY;
        let mut dev = match Device::open_serial(self.serial) {
            Ok(dev) => dev,
            Err(dt295::Error::NoDevice) => {
                if !self.absent_logged {
                    info!("not connected; waiting for it");
                    self.absent_logged = true;
                }
                return;
            }
            Err(e) => {
                warn!("cannot open: {e}");
                return;
            }
        };
        self.absent_logged = false;
        // the System1 path; before start: the front-end power-on depends on it
        dev.set_trc_firmware(Some(self.conf.trc_firmware()));
        self.state = match State::load(&self.state_path) {
            Ok(st) => st.unwrap_or_default(),
            Err(e) => {
                warn!("cannot read {}: {e}", self.state_path.display());
                State::default()
            }
        };
        let keys = prior_keys(&self.state);
        if !keys.is_empty() {
            dev.set_prior_link_keys(&keys);
        }
        dev.set_link_key_journal(key_journal(
            self.state_path.clone(),
            Arc::clone(&self.pending_written),
        ));
        for rec in &self.state.psi {
            dev.psi_import(rec);
        }
        let t0 = Instant::now();
        match boot(&mut dev, &self.conf.idle_firmware()) {
            Ok(()) => {
                info!("opened and booted ({} ms)", t0.elapsed().as_millis());
                self.watch = Some(CardWatch::new(&dev.live()));
                self.dev = Some(dev);
                self.reboot = false;
                self.last_presence = Instant::now();
            }
            Err(e) => warn!("cannot boot: {e}"),
        }
    }

    /// Keep the session alive, notice an unplug, save what changed.
    fn idle(&mut self) {
        let Some(mut dev) = self.dev.take() else {
            return;
        };
        dev.idle_tick();
        if self.last_presence.elapsed() >= PRESENCE_EVERY {
            self.last_presence = Instant::now();
            if !self.present() {
                info!("disconnected");
                self.save(&mut dev);
                self.next_open = Instant::now();
                return; // dropping `dev` closes it
            }
        }
        self.save(&mut dev);
        self.dev = Some(dev);
    }

    fn present(&self) -> bool {
        dt295::present_serials().map_or(true, |serials| {
            serials.iter().any(|s| s.eq_ignore_ascii_case(self.serial))
        })
    }

    /// Write the state file if a stream proved a link key or (not streaming) the PSI
    /// cache changed.
    fn save(&mut self, dev: &mut Device) {
        if self.pending_written.swap(false, Ordering::Relaxed) {
            // the journal wrote an unproved key to the file: keep it in our copy
            if let Ok(Some(file)) = State::load(&self.state_path) {
                self.state.pending_keys = file.pending_keys;
            }
        }
        let mut changed = take_link_keys(&mut self.state, dev);
        if !dev.live().is_streaming() && dev.psi_changed() {
            self.state.psi = dev.psi_export();
            changed = true;
        }
        if changed && let Err(e) = self.state.save(&self.state_path) {
            warn!("cannot save {}: {e}", self.state_path.display());
        }
    }

    /// Serve one request; returns a request that took the unit over meanwhile.
    fn serve(&mut self, job: Job, jobs: &Receiver<Job>) -> Option<Job> {
        let Job { req, mut conn } = job;
        if self.dev.is_none() {
            self.open();
        }
        let Some(mut dev) = self.dev.take() else {
            let why = format!("unit {} is not connected", self.serial);
            let _ = reply(&mut conn, &Reply::Error(why));
            return None;
        };
        let fw = self.conf.idle_firmware();
        if self.reboot || dev.needs_recovery() {
            info!(
                "{}; booting the device again",
                if self.reboot {
                    "the last stream delivered nothing"
                } else {
                    "the firmware faulted"
                }
            );
            if let Err(e) = boot(&mut dev, &fw) {
                warn!("cannot boot: {e}");
                let _ = reply(
                    &mut conn,
                    &Reply::Error(format!("cannot boot the unit: {e}")),
                );
                self.dev = Some(dev);
                return None;
            }
            self.reboot = false;
        }

        let t0 = Instant::now();
        if let Some(w) = self.watch.as_mut() {
            w.tuned();
        }
        let (pipe, queue) = Pipe::new();
        let mut started = start(&mut dev, &req, pipe.sink());
        if let Err(why) = &started {
            // 45-60 s after a previous session the firmware can fault on the way
            if dev.needs_recovery() && !stopping() {
                warn!("{why}; the firmware faulted, booting the device once more");
                started = boot(&mut dev, &fw)
                    .map_err(|e| format!("cannot boot the unit: {e}"))
                    .and_then(|()| start(&mut dev, &req, pipe.sink()));
            }
        }
        if let Err(why) = started {
            warn!("{}: {why}", req.to_line());
            let _ = reply(&mut conn, &Reply::Error(why));
            self.dev = Some(dev);
            return None;
        }
        info!(
            "{}: streaming ({} ms)",
            req.to_line(),
            t0.elapsed().as_millis()
        );
        if reply(&mut conn, &Reply::Ok).is_err() {
            pipe.closed.store(true, Ordering::Release); // the client is gone already
        }
        // to end a write the client never takes (a takeover must not wait for it)
        let shutter = conn.try_clone().ok();
        let writer = {
            let pipe = Arc::clone(&pipe);
            thread::Builder::new()
                .name(format!("unit {} writer", self.serial))
                .spawn(move || write_ts(&pipe, &queue, conn))
        };
        if writer.is_err() {
            pipe.closed.store(true, Ordering::Release);
        }

        let t_stream = Instant::now();
        let mut takeover = None;
        let mut unplugged = false;
        let end = loop {
            if stopping() {
                break "the daemon stops";
            }
            // a write fails at once, but while nothing is written (the stream holds its
            // TS back until the service's PAT) only the socket tells
            if pipe.closed.load(Ordering::Acquire) || shutter.as_ref().is_some_and(peer_closed) {
                break "the client went away";
            }
            match jobs.try_recv() {
                // a background job (logo collection) never takes a stream over
                Ok(mut job) if job.req.if_free => {
                    info!("{}: refused, the unit is streaming", job.req.to_line());
                    let why = format!("unit {} is streaming", self.serial);
                    let _ = reply(&mut job.conn, &Reply::Busy(why));
                }
                Ok(job) => {
                    takeover = Some(job);
                    break "a new request takes the unit over";
                }
                Err(TryRecvError::Disconnected) => break "the daemon stops",
                Err(TryRecvError::Empty) => {}
            }
            if dev.live().device_lost() {
                unplugged = true;
                break "the unit was unplugged";
            }
            if self.last_presence.elapsed() >= PRESENCE_EVERY {
                self.last_presence = Instant::now();
                if !self.present() {
                    unplugged = true;
                    break "the unit was unplugged";
                }
            }
            let stalled = dev.needs_recovery();
            let over = self
                .watch
                .as_mut()
                .and_then(|w| w.poll(&dev.live(), stalled, Instant::now()));
            if let Some(over) = over {
                info!("{}: starting the stream over", over.why);
                if let Err(e) = dev.stream_stop() {
                    warn!("stream stop failed: {e}");
                }
                let again = if over.in_place && dev.resume_in_place(pipe.sink()) {
                    Ok(())
                } else {
                    boot(&mut dev, &fw)
                        .map_err(|e| format!("cannot boot the unit: {e}"))
                        .and_then(|()| start(&mut dev, &req, pipe.sink()))
                };
                if let Some(w) = self.watch.as_mut() {
                    w.started_over(Instant::now());
                }
                if let Err(why) = again {
                    warn!("{}: {why}", req.to_line());
                    break "the stream did not start again";
                }
            }
            self.save(&mut dev); // a link key the stream proved, at once
            thread::sleep(POLL);
        };
        let device_bytes = dev.live().delivered();
        if let Err(e) = dev.stream_stop() {
            warn!("stream stop failed: {e}");
        }
        pipe.finish();
        if let Some(conn) = &shutter {
            let _ = conn.shutdown(std::net::Shutdown::Both);
        }
        if let Ok(writer) = writer {
            let _ = writer.join();
        }
        let bytes = pipe.bytes.load(Ordering::Relaxed);
        let dropped = pipe.dropped.load(Ordering::Relaxed);
        info!(
            "stream ended after {:.1} s ({end}): {bytes} bytes{}",
            t_stream.elapsed().as_secs_f64(),
            if dropped > 0 {
                format!(", {dropped} dropped (the client did not read in time)")
            } else {
                String::new()
            }
        );
        if device_bytes == 0 && t_stream.elapsed() >= SILENT {
            warn!("the device delivered nothing; the next tune boots it first");
            self.reboot = true;
        } else if bytes == 0 && device_bytes > 0 {
            info!(
                "no PAT listed the service ({device_bytes} bytes held back): off the air, or \
                 not on this TS"
            );
        }
        self.save(&mut dev);
        if unplugged {
            info!("disconnected");
            self.next_open = Instant::now();
        } else {
            self.dev = Some(dev);
        }
        takeover
    }

    fn close(&mut self) {
        if let Some(mut dev) = self.dev.take() {
            self.save(&mut dev);
        }
    }
}

/// Boot the firmware; once more if it stopped acking on the way (within a minute of
/// an earlier session it can).
fn boot(dev: &mut Device, fw: &std::path::Path) -> dt295::Result<()> {
    dev.start(fw)?;
    if dev.needs_recovery() {
        warn!("the firmware stopped acking during the start; starting once more");
        dev.start(fw)?;
    }
    Ok(())
}

/// Tune, wait for lock (up to ~3 s more), and start streaming into `sink`.
fn start(dev: &mut Device, req: &Request, sink: TsSink) -> Result<(), String> {
    let sys = match req.tuning {
        Tuning::Terrestrial { hz } => {
            dev.tune_t(hz).map_err(|e| format!("tune: {e}"))?;
            System::Terrestrial
        }
        Tuning::Satellite { hz, tsid } => {
            dev.tune_s(hz, tsid).map_err(|e| format!("tune: {e}"))?;
            System::Satellite
        }
    };
    let mut lock = Err(String::new());
    for _ in 0..30 {
        lock = match dev.read_status(sys) {
            Ok(st) if st.locked => Ok(()),
            Ok(st) => Err(format!(
                "no lock (C/N {:.1} dB)",
                f64::from(st.cn_mdb) / 1000.0
            )),
            Err(e) => Err(format!("status: {e}")),
        };
        if lock.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    lock?;
    dev.set_service_id(req.sid);
    dev.stream_start(sink)
        .map_err(|e| format!("stream start: {e}"))
}

fn reply(conn: &mut UnixStream, r: &Reply) -> std::io::Result<()> {
    conn.write_all(format!("{}\n", r.to_line()).as_bytes())
}

/// The TS between the stream worker and the client: the worker never blocks on it.
struct Pipe {
    tx: Mutex<Option<SyncSender<Vec<u8>>>>,
    /// Bytes the device delivered (queued or dropped).
    bytes: AtomicU64,
    /// Bytes dropped because the client did not keep up.
    dropped: AtomicU64,
    /// The client is gone.
    closed: AtomicBool,
}

impl Pipe {
    fn new() -> (Arc<Self>, Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::sync_channel(QUEUE_READS);
        let pipe = Self {
            tx: Mutex::new(Some(tx)),
            bytes: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        };
        (Arc::new(pipe), rx)
    }

    fn sink(self: &Arc<Self>) -> TsSink {
        let pipe = Arc::clone(self);
        Box::new(move |ts: &[u8]| pipe.push(ts))
    }

    fn push(&self, ts: &[u8]) {
        self.bytes.fetch_add(ts.len() as u64, Ordering::Relaxed);
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tx) = tx.as_ref() else {
            return;
        };
        match tx.try_send(ts.to_vec()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(ts.len() as u64, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => self.closed.store(true, Ordering::Release),
        }
    }

    /// No more TS: the writer drains the queue and ends.
    fn finish(&self) {
        self.tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

/// Whether the client closed its end (without reading or writing anything).
fn peer_closed(conn: &UnixStream) -> bool {
    let mut b = 0u8;
    // SAFETY: a valid socket and a 1-byte buffer; MSG_DONTWAIT makes only this call
    // non-blocking (the writer thread shares the socket in blocking mode).
    let n = unsafe {
        libc::recv(
            conn.as_raw_fd(),
            (&raw mut b).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match n {
        0 => true, // end of stream
        n if n > 0 => false,
        _ => !matches!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ),
    }
}

/// The writer thread: queue to client until the queue ends or the client is gone.
fn write_ts(pipe: &Pipe, queue: &Receiver<Vec<u8>>, mut conn: UnixStream) {
    for chunk in queue {
        if conn.write_all(&chunk).is_err() {
            pipe.closed.store(true, Ordering::Release);
            return;
        }
    }
}
