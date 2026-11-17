//! `dt295 serve`: hold the configured units open for good and stream to `dt295 tune`
//! clients over a Unix socket (Linux; a recorder runs `dt295 tune` as its tuner
//! command).
//!
//! Every `dt295 capture` booted the device, authenticated a new secure session and
//! was then killed by the recorder. That was the root of most trouble on Linux: a
//! previous session timing out under the new one, keepalives that never went out,
//! streams left running with their TS queued, link keys to carry between processes.
//! Here one thread per unit owns the device the way the BonDriver does in TVTest; a
//! client only asks for a stream and reads it.
//!
//! The configuration ([`conf`]) and the socket protocol ([`proto`]) build on every
//! platform, so their tests run on Windows too.

pub mod conf;
pub mod proto;
#[cfg(unix)]
mod unit;

#[cfg(unix)]
pub use daemon::{ServeArgs, run};

#[cfg(unix)]
mod daemon {
    use std::fs;
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::mpsc::Sender;
    use std::thread;
    use std::time::Duration;

    use anyhow::{Context, Result, bail};
    use clap::Args;
    use tracing::{info, warn};

    use super::conf::{self, Conf};
    use super::proto::{MAX_LINE, Reply, Request};
    use super::unit::{self, Job};
    use crate::tuner::{install_ctrl_c, stopping};

    /// How long a client may take to send its request line.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
    /// A client that reads nothing for this long counts as gone.
    const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Debug, Args)]
    pub struct ServeArgs {
        /// The configuration file.
        #[arg(short, long, value_name = "FILE", default_value = conf::DEFAULT_CONF)]
        pub config: PathBuf,
    }

    pub fn run(a: &ServeArgs) -> Result<()> {
        let conf = Arc::new(Conf::load(&a.config)?);
        install_ctrl_c();
        let listener = listen(&conf.socket)?;
        info!(
            "dt295 {}: listening on {}; units: {}",
            env!("DT295_VERSION"),
            conf.socket.display(),
            conf.units
                .iter()
                .map(|u| u.serial.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut units = Vec::new();
        for u in &conf.units {
            let (jobs, thread) = unit::spawn(u.serial.clone(), u.raw_ts, Arc::clone(&conf))
                .with_context(|| format!("cannot start the thread of unit {}", u.serial))?;
            units.push((u.serial.clone(), jobs, thread));
        }
        let routes: Vec<(String, Sender<Job>)> = units
            .iter()
            .map(|(serial, jobs, _)| (serial.clone(), jobs.clone()))
            .collect();
        while !stopping() {
            match listener.accept() {
                Ok((conn, _)) => dispatch(conn, &routes),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    warn!("accept: {e}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        info!("stopping");
        drop(routes);
        for (_, jobs, thread) in units {
            drop(jobs);
            let _ = thread.join();
        }
        let _ = fs::remove_file(&conf.socket);
        Ok(())
    }

    /// Bind the socket (replacing one an earlier run left), readable and writable by
    /// the owner and the group.
    fn listen(path: &Path) -> Result<UnixListener> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        match fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_socket() => fs::remove_file(path)
                .with_context(|| format!("cannot remove the old {}", path.display()))?,
            Ok(_) => bail!("{} exists and is not a socket", path.display()),
            Err(_) => {}
        }
        let listener = UnixListener::bind(path)
            .with_context(|| format!("cannot listen on {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
        listener.set_nonblocking(true)?;
        Ok(listener)
    }

    /// Read a client's request and hand it to its unit's thread.
    fn dispatch(mut conn: UnixStream, routes: &[(String, Sender<Job>)]) {
        let _ = conn.set_nonblocking(false);
        let _ = conn.set_read_timeout(Some(REQUEST_TIMEOUT));
        let _ = conn.set_write_timeout(Some(WRITE_TIMEOUT));
        let mut line = String::new();
        let read = BufReader::new((&conn).take(MAX_LINE)).read_line(&mut line);
        let routed = read
            .map_err(|e| format!("cannot read the request: {e}"))
            .and_then(|_| Request::parse(&line))
            .and_then(|req| {
                routes
                    .iter()
                    .find(|(serial, _)| serial.eq_ignore_ascii_case(&req.unit))
                    .map(|(_, jobs)| (req.clone(), jobs))
                    .ok_or_else(|| format!("no [unit {}] in the configuration", req.unit))
            });
        let why = match routed {
            Ok((req, jobs)) => {
                let _ = conn.set_read_timeout(None);
                match jobs.send(Job { req, conn }) {
                    Ok(()) => return,
                    Err(e) => {
                        conn = e.0.conn;
                        "the daemon is stopping".to_owned()
                    }
                }
            }
            Err(why) => why,
        };
        warn!("request {:?}: {why}", line.trim_end());
        let _ = conn.write_all(format!("{}\n", Reply::Error(why).to_line()).as_bytes());
    }
}
