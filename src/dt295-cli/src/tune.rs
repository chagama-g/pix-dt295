//! `dt295 tune`: ask `dt295 serve` for a stream and write its TS to stdout -- a
//! recorder's tuner command (Mirakurun / mirakc). It owns no device: when the recorder
//! kills it, the socket closes and the daemon stops the stream properly.

use std::fs::File;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Args};

use dt295::chan::NO_TSID;

use crate::serve::conf::client_socket;
use crate::serve::proto::{Reply, Request, Tuning};
use crate::{parse_hex_u16, parse_u16, parse_u32};

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("band").required(true).args(["ter", "sat"])))]
pub struct TuneArgs {
    /// The unit, by USB serial number (a [unit <serial>] of the daemon's configuration;
    /// `--unit` is the same).
    #[arg(long, alias = "unit", value_name = "SERIAL")]
    pub serial: String,

    /// ISDB-T: the channel centre frequency in Hz (e.g. 557142857 for UHF 27ch).
    #[arg(long, value_name = "HZ", value_parser = parse_u32)]
    pub ter: Option<u32>,

    /// ISDB-S: the IF frequency in Hz (satellite RF - 10678 MHz, e.g. 1318000000).
    #[arg(long, value_name = "HZ", value_parser = parse_u32)]
    pub sat: Option<u32>,

    /// ISDB-S: the broadcast transport_stream_id in hex (e.g. 40f1 or 0x40F1); 7 or
    /// less is a TS number, the TS whose TSID has those low 3 bits; default: the
    /// frequency's first TS.
    #[arg(long, value_name = "HEX", value_parser = parse_hex_u16, requires = "sat")]
    pub tsid: Option<u16>,

    /// The service to stream (decimal or 0x hex); default: every service of the TS
    /// (mirakc filters the services itself).
    #[arg(long, value_name = "ID", value_parser = parse_u16)]
    pub sid: Option<u16>,

    /// Only if the unit is free: fail instead of taking it over from a running stream
    /// (for background jobs such as `dt295 logo`; a recorder's request still takes the
    /// unit over from this one).
    #[arg(long)]
    pub if_free: bool,

    /// The configuration of `dt295 serve`, for its socket (default /etc/dt295.conf if
    /// it exists).
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// The socket of `dt295 serve` (default: the configuration's, else
    /// /run/dt295/dt295.sock).
    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,
}

pub fn run(a: &TuneArgs) -> Result<()> {
    if io::stdout().is_terminal() {
        bail!("refusing to write a TS to the terminal: redirect stdout");
    }
    let tuning = match (a.ter, a.sat, a.tsid) {
        (Some(hz), None, None) => Tuning::Terrestrial { hz },
        (None, Some(hz), tsid) => Tuning::Satellite {
            hz,
            tsid: tsid.unwrap_or(NO_TSID),
        },
        _ => bail!("give --ter or --sat"),
    };
    let socket = client_socket(a.config.as_deref(), a.socket.as_deref())?;
    let req = Request {
        unit: a.serial.clone(),
        tuning,
        sid: a.sid,
        if_free: a.if_free,
    };
    let mut conn = UnixStream::connect(&socket).with_context(|| {
        format!(
            "cannot connect to dt295 serve at {} (is it running?)",
            socket.display()
        )
    })?;
    conn.write_all(format!("{}\n", req.to_line()).as_bytes())
        .context("cannot send the request")?;
    let mut ts = BufReader::with_capacity(1 << 16, conn);
    let mut line = String::new();
    ts.read_line(&mut line)
        .context("no reply from dt295 serve")?;
    if line.is_empty() {
        bail!("dt295 serve closed the connection");
    }
    if let Reply::Busy(why) | Reply::Error(why) = Reply::parse(&line) {
        bail!("{why}");
    }
    match pump(ts) {
        // the reader is gone (a recorder stopping a recording): the normal end
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e).context("passing the TS on"),
        _ => Ok(()),
    }
}

/// Copy the TS to stdout until either end goes away. A recorder that gives up on a
/// silent stream only closes its end of the pipe (mirakc after its 30 s limit), and
/// the daemon sends nothing until the service's PAT, so waiting for a write to fail
/// could take for ever (45 min for a station off the air): poll sees the closed pipe
/// at once. Written unbuffered: stdout's own buffer flushes at every 0x0A.
fn pump(ts: BufReader<UnixStream>) -> io::Result<()> {
    let mut out = File::from(io::stdout().as_fd().try_clone_to_owned()?);
    out.write_all(ts.buffer())?;
    let sock = ts.into_inner();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: sock.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            // no events asked for: POLLERR / POLLHUP come anyway when the reader goes
            libc::pollfd {
                fd: out.as_raw_fd(),
                events: 0,
                revents: 0,
            },
        ];
        // SAFETY: two valid pollfd entries for open descriptors.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if fds[1].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Ok(());
        }
        if fds[0].revents != 0 {
            let n = match (&sock).read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if n == 0 {
                return Ok(()); // the daemon ended the stream
            }
            out.write_all(&buf[..n])?;
        }
    }
}
