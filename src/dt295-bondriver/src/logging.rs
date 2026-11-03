//! Logging: every `tracing` event of the driver and of the dt295 core (INFO and
//! worse, or `[Tuner] LogLevel`) becomes one line
//!
//! ```text
//! HH:MM:SS.mmm [BonDriver_DT295] <message>
//! ```
//!
//! (local time) sent to OutputDebugString (DebugView) and, with `[Tuner] LogFile`,
//! appended to that file by tracing-appender's non-blocking writer: the file write
//! happens on the appender's thread, never on the device or stream threads (a
//! blocking disk write on the stream worker used to make it miss the PAT read).
//!
//! The global subscriber is installed once per process (it cannot be replaced);
//! each driver instance plugs its log file into it ([`open_log_file`]) and unplugs
//! it when the returned [`LogSession`] drops (in `Release`).

use std::fmt::{self, Write as _};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock, PoisonError, mpsc};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Subscriber, warn};
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::{Registry, reload};

use crate::sys;

/// The tag after the time stamp of every line.
const TAG: &str = "[BonDriver_DT295]";

/// Line end in the log file: the platform's text line end.
const FILE_EOL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// The current driver's log file, if it has one.
static FILE_SINK: Mutex<Option<NonBlocking>> = Mutex::new(None);

static INIT: Once = Once::new();

/// The level of the global subscriber, set from the ini once it is read.
static LEVEL: OnceLock<reload::Handle<LevelFilter, Registry>> = OnceLock::new();

/// Install the global subscriber (at INFO) and the logging panic hook (once per
/// process).
pub fn init() {
    INIT.call_once(|| {
        let (filter, handle) = reload::Layer::new(LevelFilter::INFO);
        let _ = LEVEL.set(handle);
        let subscriber = Registry::default().with(DriverLayer.with_filter(filter));
        let _ = tracing::subscriber::set_global_default(subscriber);

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!("panic: {info}");
            previous(info);
        }));
    });
}

/// Log events of `level` and worse from now on (`[Tuner] LogLevel`).
pub fn set_level(level: LevelFilter) {
    if let Some(handle) = LEVEL.get() {
        let _ = handle.modify(|l| *l = level);
    }
}

/// `HH:MM:SS.mmm [BonDriver_DT295] <msg>`, without a line end.
pub fn format_line(t: sys::TimeOfDay, msg: &str) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03} {TAG} {msg}",
        t.hour, t.minute, t.second, t.millis
    )
}

/// The layer that formats and writes every event.
struct DriverLayer;

impl<S: Subscriber> Layer<S> for DriverLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut text = EventText::default();
        event.record(&mut text);
        let line = format_line(sys::local_time(), &text.finish());
        sys::output_debug_string(&format!("{line}\n"));
        let mut sink = FILE_SINK.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(w) = sink.as_mut() {
            let _ = w.write_all(format!("{line}{FILE_EOL}").as_bytes());
        }
    }
}

/// The message of an event, then its other fields as ` name=value`.
#[derive(Default)]
struct EventText {
    message: String,
    fields: String,
}

impl EventText {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else {
            self.message + &self.fields
        }
    }
}

impl Visit for EventText {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// A driver instance's log file. Dropping it unplugs the file from the subscriber,
/// lets the appender write what is queued, and waits (bounded) until its thread has
/// closed the file.
#[derive(Debug)]
pub struct LogSession {
    guard: Option<WorkerGuard>,
    closed: mpsc::Receiver<()>,
}

impl Drop for LogSession {
    fn drop(&mut self) {
        drop(
            FILE_SINK
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        drop(self.guard.take()); // flushes the queue and stops the appender thread
        let _ = self.closed.recv_timeout(Duration::from_secs(2));
    }
}

/// The file the appender thread owns; tells [`LogSession`] when it is closed.
struct LogFile {
    out: Option<BufWriter<File>>,
    closed: mpsc::SyncSender<()>,
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.out.as_mut() {
            Some(f) => f.write(buf),
            None => Ok(buf.len()),
        }
    }

    // Called by the appender after each batch it drained, so lines reach the file
    // promptly without a flush per line.
    fn flush(&mut self) -> io::Result<()> {
        self.out.as_mut().map_or(Ok(()), Write::flush)
    }
}

impl Drop for LogFile {
    fn drop(&mut self) {
        if let Some(mut f) = self.out.take() {
            let _ = f.flush();
        } // the file is closed here
        let _ = self.closed.try_send(());
    }
}

/// `<log>.1`.
fn previous_log(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

/// Start writing the log to `path` for this driver instance.
///
/// The previous run's log is kept as `<log>.1`, but only a non-empty one: a host
/// that creates the driver without opening the tuner leaves an empty log, which
/// would otherwise rotate the useful one away. `None` if the file cannot be created
/// (logging then goes to OutputDebugString only).
pub fn open_log_file(path: &Path) -> Option<LogSession> {
    if fs::metadata(path).is_ok_and(|m| m.len() > 0) {
        let _ = fs::rename(path, previous_log(path));
    }
    let file = match File::create(path) {
        Ok(f) => f,
        Err(e) => {
            warn!("log file: cannot write {} ({e})", path.display());
            return None;
        }
    };
    let (closed_tx, closed) = mpsc::sync_channel(1);
    let writer = LogFile {
        out: Some(BufWriter::new(file)),
        closed: closed_tx,
    };
    let (nb, guard) = NonBlockingBuilder::default()
        .thread_name("BonDriver_DT295 log")
        .finish(writer);
    *FILE_SINK.lock().unwrap_or_else(PoisonError::into_inner) = Some(nb);
    Some(LogSession {
        guard: Some(guard),
        closed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format() {
        let t = sys::TimeOfDay {
            hour: 7,
            minute: 5,
            second: 9,
            millis: 42,
        };
        assert_eq!(
            format_line(
                t,
                "state: saved (1 service id(s), link key yes, 0 PSI section(s))"
            ),
            "07:05:09.042 [BonDriver_DT295] state: saved (1 service id(s), link key yes, 0 PSI section(s))"
        );
    }

    #[test]
    fn file_session_rotation_and_lines() {
        let dir = std::env::temp_dir().join(format!("dt295-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("dt295core.log");
        init();

        // (Other tests running in parallel may log into the file too: check lines
        // with `contains`.)
        // An empty previous log is not rotated (keeps an older useful .1).
        fs::write(previous_log(&log), "older\n").unwrap();
        fs::write(&log, "").unwrap();
        let s = open_log_file(&log).unwrap();
        tracing::info!("first {}", 1);
        tracing::debug!("filtered out");
        drop(s);
        assert_eq!(fs::read_to_string(previous_log(&log)).unwrap(), "older\n");
        let first = fs::read_to_string(&log).unwrap();
        assert!(
            first.contains(&format!(" {TAG} first 1{FILE_EOL}")),
            "{first:?}"
        );
        assert!(!first.contains("filtered out"));
        assert_eq!(first.as_bytes()[2], b':');
        assert_eq!(first.as_bytes()[8], b'.');

        // A non-empty one becomes <log>.1.
        let s = open_log_file(&log).unwrap();
        tracing::warn!(code = 7, "second");
        drop(s);
        assert_eq!(fs::read_to_string(previous_log(&log)).unwrap(), first);
        let second = fs::read_to_string(&log).unwrap();
        assert!(
            second.contains(&format!(" {TAG} second code=7{FILE_EOL}")),
            "{second:?}"
        );

        // Unplugged: later lines go to OutputDebugString only.
        tracing::info!("after release");
        assert_eq!(fs::read_to_string(&log).unwrap(), second);
        let _ = fs::remove_dir_all(&dir);
    }
}
