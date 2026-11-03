//! `dt295 capture`: open, select the firmware path, boot, tune, wait for lock, stream
//! every service of the TS (or the one of `--sid`) into a file or stdout, for a while or
//! until stopped. With
//! `--bondriver` and `--channel` it takes its unit, firmware and channel from a
//! BonDriver_DT295 ini, and with `-t 0` it runs until a signal or until the reader
//! closes the output: what a recorder (Mirakurun / mirakc) expects of a tuner command.

use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use dt295::chan::NO_TSID;
use dt295::{StallWatch, System};

use crate::CaptureArgs;
use crate::bonini::{self, BonDriver, Tune};
use crate::serve::conf::Conf;
use crate::tuner::{self, DeviceArgs, Unit, stopping};

/// The stream's destination, written on the core's stream worker thread.
struct Sink {
    out: Box<dyn Write + Send>,
    bytes: u64,
    /// Whether the first window of >= 2 packets was 0x47-aligned.
    sync: Option<bool>,
    /// The first write error (later data is dropped).
    error: Option<io::Error>,
}

impl Sink {
    fn new(out: Box<dyn Write + Send>) -> Self {
        Self {
            out,
            bytes: 0,
            sync: None,
            error: None,
        }
    }

    fn write(&mut self, data: &[u8]) {
        // Quick 0x47 sync check on the first packet-sized window we see.
        if self.sync.is_none() && data.len() >= 188 * 2 {
            self.sync = Some(data[0] == 0x47 && data[188] == 0x47);
        }
        self.bytes += data.len() as u64;
        if self.error.is_none()
            && let Err(e) = self.out.write_all(data)
        {
            self.error = Some(e);
        }
    }

    fn sync_text(&self) -> &'static str {
        match self.sync {
            Some(true) => "OK (0x47 aligned)",
            Some(false) => "NOT aligned (check scramble/packet mode)",
            None => "unknown",
        }
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

/// What to tune.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Terrestrial { hz: u32 },
    Satellite { hz: u32, tsid: u16 },
}

/// The tune and service of the options: `--channel` (looked up in the ini), else
/// `--ter` / `--sat` / `--tsid`; `--sid` wins over a line's service.
fn target(a: &CaptureArgs, ini: Option<&BonDriver>) -> Result<(Target, Option<u16>)> {
    if let Some(name) = &a.channel {
        let satellite = ini.map_or(&[][..], |b| &b.satellite[..]);
        let Some(tune) = bonini::tune(name, satellite) else {
            bail!(
                "no channel {name:?}: neither a [BS] / [CS110] line of {} nor a built-in \
                 13ch..62ch / C13ch..C63ch",
                ini.map_or("--bondriver".into(), |b| b.ini.display().to_string())
            );
        };
        return Ok(match tune {
            Tune::Terrestrial(hz) => (Target::Terrestrial { hz }, a.sid),
            Tune::Satellite(l) => (
                Target::Satellite {
                    hz: l.freq_hz,
                    tsid: l.tsid,
                },
                a.sid.or(l.sid),
            ),
        });
    }
    match (a.ter, a.sat) {
        (Some(hz), _) => Ok((Target::Terrestrial { hz }, a.sid)),
        (None, Some(hz)) => Ok((
            Target::Satellite {
                hz,
                tsid: a.tsid.unwrap_or(NO_TSID),
            },
            a.sid,
        )),
        (None, None) => bail!("--channel, --ter or --sat is required"),
    }
}

/// The unit of `dt295 serve`'s configuration (`--serial`, or its only unit) with the
/// firmware and the state file the daemon uses; options given win.
fn conf_device_args(conf: &Conf, given: &DeviceArgs) -> Result<DeviceArgs> {
    let serial = match (&given.serial, conf.units.as_slice()) {
        (Some(s), _) => s.clone(),
        (None, [only]) => only.clone(),
        (None, units) => bail!(
            "which unit? the configuration has {}: give --serial",
            units.len()
        ),
    };
    Ok(DeviceArgs {
        fw: given.fw.clone().or_else(|| Some(conf.idle_firmware())),
        trc: given.trc.clone().or_else(|| Some(conf.trc_firmware())),
        index: 0,
        state: given
            .state
            .clone()
            .or_else(|| Some(conf.state_file(&serial))),
        serial: Some(serial),
    })
}

/// Tune, wait for lock and start streaming `sid` into the sink.
fn start_stream(
    unit: &mut Unit,
    target: Target,
    sid: Option<u16>,
    sink: &Arc<Mutex<Sink>>,
) -> Result<()> {
    let sys = match target {
        Target::Terrestrial { hz } => {
            unit.dev.tune_t(hz).context("tune (ISDB-T)")?;
            System::Terrestrial
        }
        Target::Satellite { hz, tsid } => {
            unit.dev.tune_s(hz, tsid).context("tune (ISDB-S)")?;
            System::Satellite
        }
    };
    unit.wait_lock(sys)?;

    unit.dev.set_service_id(sid);
    let worker_sink = Arc::clone(sink);
    unit.dev
        .stream_start(Box::new(move |ts: &[u8]| {
            worker_sink
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .write(ts);
        }))
        .context("stream start")
}

pub fn run(a: &CaptureArgs) -> Result<()> {
    let ini = a.bondriver.as_deref().map(BonDriver::load).transpose()?;
    let dev = match (&ini, &a.config) {
        (Some(b), _) => b.device_args(&a.dev),
        (None, Some(c)) => conf_device_args(&Conf::load(c)?, &a.dev)?,
        (None, None) => a.dev.clone(),
    };
    let (target, sid) = target(a, ini.as_ref())?;
    let sid = sid.filter(|_| !a.all_services);

    // The output first: no point booting the device if the TS cannot be written.
    let out: Box<dyn Write + Send> = match &a.output {
        Some(p) => Box::new(BufWriter::new(
            File::create(p).with_context(|| format!("cannot create {}", p.display()))?,
        )),
        None if io::stdout().is_terminal() => {
            bail!("refusing to write a TS to the terminal: use -o FILE or redirect stdout")
        }
        None => Box::new(BufWriter::new(io::stdout())),
    };
    tuner::install_ctrl_c();
    let mut unit = Unit::open(&dev)?;

    let sink = Arc::new(Mutex::new(Sink::new(out)));
    if let Err(e) = start_stream(&mut unit, target, sid, &sink) {
        // Linux VM 2026-10-24: a stream start 46 s after a recording ended faulted to
        // 0xFFFF (that process's secure session timing out under this one).
        if stopping() || !unit.dev.needs_recovery() {
            return Err(e);
        }
        tracing::warn!("{e:#}; the firmware faulted, starting the device once more");
        unit.restart()?;
        start_stream(&mut unit, target, sid, &sink)?;
    }

    // -t 0: until Ctrl-C / SIGTERM, or until the reader closes the output
    let t0 = Instant::now();
    let limit = (a.seconds > 0).then(|| Duration::from_secs(a.seconds));
    let tty = io::stderr().is_terminal();
    let mut stall = StallWatch::new();
    while !stopping() && limit.is_none_or(|l| t0.elapsed() < l) {
        thread::sleep(Duration::from_millis(200));
        {
            let s = sink.lock().unwrap_or_else(PoisonError::into_inner);
            if s.error.is_some() {
                break;
            }
            if tty {
                eprint!("captured {:.2} MiB\r", mib(s.bytes));
            }
        }
        // a firmware that stopped acking leaves the stream silent
        if unit.dev.needs_recovery() && stall.start_over(Instant::now()) {
            tracing::warn!("the firmware stopped acking: starting the stream over");
            if let Err(e) = unit.dev.stream_stop() {
                tracing::warn!("stream stop failed: {e}");
            }
            unit.restart()?;
            start_stream(&mut unit, target, sid, &sink)?;
        }
        // as soon as the stream proves its link key: a recorder ends this process with
        // SIGKILL, so the end of the run may never come
        unit.save_link_key();
    }
    if tty {
        eprintln!();
    }

    let stopped = unit.dev.stream_stop(); // the worker no longer writes after this
    unit.save_link_key();
    let mut s = sink.lock().unwrap_or_else(PoisonError::into_inner);
    let flushed = s.out.flush();
    eprintln!("total {} bytes; TS sync {}", s.bytes, s.sync_text());
    match s.error.take() {
        // the reader is gone (a recorder stopping a recording): the normal end
        Some(e) if e.kind() == io::ErrorKind::BrokenPipe => {
            eprintln!("the reader closed the output");
        }
        Some(e) => return Err(e).context("writing the TS"),
        None => match flushed {
            Err(e) if e.kind() != io::ErrorKind::BrokenPipe => {
                return Err(e).context("writing the TS");
            }
            _ => {}
        },
    }
    stopped.context("stream stop")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer that fails after `ok` bytes.
    struct Failing {
        ok: usize,
    }

    impl Write for Failing {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.ok == 0 {
                return Err(io::Error::other("disk full"));
            }
            let n = buf.len().min(self.ok);
            self.ok -= n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn sink_counts_and_checks_sync() {
        let mut s = Sink::new(Box::new(io::sink()));
        s.write(&[0x47; 100]);
        assert_eq!(s.sync, None, "too short to judge");
        let mut ts = vec![0u8; 188 * 3];
        ts[0] = 0x47;
        ts[188] = 0x47;
        s.write(&ts);
        assert_eq!(s.sync, Some(true));
        s.write(&[0u8; 188 * 2]);
        assert_eq!(s.sync, Some(true), "judged once");
        assert_eq!(s.bytes, 100 + 188 * 5);

        let mut s = Sink::new(Box::new(io::sink()));
        s.write(&[0u8; 188 * 2]);
        assert_eq!(s.sync_text(), "NOT aligned (check scramble/packet mode)");
    }

    #[test]
    fn sink_keeps_the_first_error() {
        let mut s = Sink::new(Box::new(Failing { ok: 10 }));
        s.write(&[0; 8]);
        assert!(s.error.is_none());
        s.write(&[0; 8]);
        assert_eq!(
            s.error.as_ref().map(ToString::to_string).as_deref(),
            Some("disk full")
        );
        s.write(&[0; 8]);
        assert_eq!(s.bytes, 24, "still counted");
    }

    fn args(cli: &[&str]) -> CaptureArgs {
        use clap::Parser;
        let all = ["dt295", "capture"].iter().chain(cli);
        match crate::Cli::try_parse_from(all).unwrap().command {
            crate::Command::Capture(a) => a,
            _ => panic!("not capture"),
        }
    }

    #[test]
    fn targets() {
        let ini = BonDriver::from_ini(
            std::path::Path::new("d.ini"),
            "[BS]\nBS15/101=1318000,0x40F1,101\n",
        );
        let sat = Target::Satellite {
            hz: 1_318_000_000,
            tsid: 0x40F1,
        };
        let a = args(&["--bondriver", "d.ini", "--channel", "bs15/101", "-t", "0"]);
        assert_eq!(a.seconds, 0);
        assert_eq!(target(&a, Some(&ini)).unwrap(), (sat, Some(101)));
        let a = args(&[
            "--bondriver",
            "d.ini",
            "--channel",
            "BS15/101",
            "--sid",
            "102",
        ]);
        assert_eq!(
            target(&a, Some(&ini)).unwrap(),
            (sat, Some(102)),
            "--sid wins"
        );
        let a = args(&["--fw", "f", "--channel", "27ch"]);
        assert_eq!(
            target(&a, None).unwrap(),
            (Target::Terrestrial { hz: 557_142_857 }, None)
        );
        let a = args(&["--bondriver", "d.ini", "--channel", "BS99/1"]);
        assert!(target(&a, Some(&ini)).is_err());
        let a = args(&["--fw", "f", "--sat", "1318000000"]);
        assert_eq!(
            target(&a, None).unwrap().0,
            Target::Satellite {
                hz: 1_318_000_000,
                tsid: NO_TSID
            }
        );
    }

    #[test]
    fn the_daemon_configuration() {
        use std::path::Path;
        let a = args(&["-c", "/etc/dt295.conf", "--channel", "27ch"]);
        assert_eq!(a.config.as_deref(), Some(Path::new("/etc/dt295.conf")));
        let one = Conf::parse("[dt295]\nfirmware-dir = /fw\n[unit AB12345678]\n").unwrap();
        let dev = conf_device_args(&one, &a.dev).unwrap();
        assert_eq!(dev.serial.as_deref(), Some("AB12345678"), "its only unit");
        assert_eq!(
            dev.fw.as_deref(),
            Some(Path::new("/fw/mb86m01_assp_osec_idle.bin"))
        );
        assert_eq!(
            dev.state.as_deref(),
            Some(Path::new("/var/lib/dt295/AB12345678.state"))
        );
        let two = Conf::parse("[unit A]\n[unit B]\n").unwrap();
        assert!(conf_device_args(&two, &a.dev).is_err(), "which one?");
        let a = args(&["-c", "x.conf", "--serial", "B", "--ter", "557142857"]);
        assert_eq!(
            conf_device_args(&two, &a.dev).unwrap().serial.as_deref(),
            Some("B")
        );
        let parse = |cli: &[&str]| {
            use clap::Parser;
            crate::Cli::try_parse_from(["dt295", "capture"].iter().chain(cli))
        };
        assert!(parse(&["-c", "x", "--bondriver", "b.ini", "--ter", "1"]).is_err());
    }
}
