//! Scheduling priority for the raw-TS stream.
//!
//! The on-chip capture ring holds only tens of ms of TS (a full CS transponder fills its
//! 192 KiB in about 40 ms), and nothing drains it but the host's pump CALL. A stream thread
//! that the OS leaves waiting behind busier, higher-priority threads (a recorder parsing
//! the TS, an EPG scan) calls the pump late, the ring overflows, and the TS is lost. So the
//! stream asks for a priority above normal work: for its own thread, which is the worker's,
//! spawned for this one stream (nothing to restore), and, on Linux, for the USB backend's
//! event thread, which completes every transfer the stream waits on. That thread is one per
//! process, shared by every unit, and stays raised once a stream has raised it: it only
//! wakes for USB completions.
//!
//! An unprivileged process gets either only within its limits: `RLIMIT_RTPRIO` of at least
//! 1 for SCHED_FIFO, `RLIMIT_NICE` of at least 30 (nice -10) for the fallback (systemd:
//! `LimitRTPRIO=1`, `LimitNICE=-10`). Without, the stream runs at the normal priority. The
//! limits are for a weak host (an STB that lost TS at the normal priority), so the shipped
//! systemd unit sets neither; docs/usage.md says when to add them.

/// SCHED_FIFO priority on Unix: the lowest real-time one. That is enough to run before every
/// normal thread, and leaves the system's own real-time threads (audio, video) above us.
/// The stream loop sleeps in the EP reap between pumps, so it does not hog a core.
#[cfg(unix)]
const RT_PRIORITY: i32 = 1;

/// The nice value to fall back on (Linux) when real-time scheduling is refused.
#[cfg(target_os = "linux")]
const NICE: i32 = -10;

/// nusb's thread that reaps the completed usbfs transfers (`nusb::platform::linux_usbfs`).
/// The pump's control transfer and the EP reads complete only once it runs: left at normal
/// priority, it makes the real-time stream thread wait behind every busy thread anyway
/// (pumps 100+ ms late under load, measured).
#[cfg(target_os = "linux")]
const USB_EVENT_THREAD: &str = "nusb-events";

/// Raise the stream's threads: the calling thread, and the USB event thread where the
/// backend has one. Returns what they got, or why not (no permission, typically: the
/// stream still runs, at the normal priority).
#[cfg(target_os = "linux")]
pub(crate) fn raise_stream_threads() -> Result<String, String> {
    // SAFETY: gettid takes no arguments and cannot fail.
    let me = raise_thread(unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t);
    let events = usb_event_threads();
    let ev = match events.first() {
        None => Err(format!("no {USB_EVENT_THREAD} thread found")),
        Some(_) => events
            .iter()
            .map(|&tid| raise_thread(tid))
            .collect::<Result<Vec<_>, _>>()
            .map(|got| got.join(", ")),
    };
    match (me, ev) {
        (Ok(me), Ok(ev)) => Ok(format!("{me} (USB event thread: {ev})")),
        (me, ev) => Err(format!(
            "stream thread: {}; USB event thread: {}",
            me.unwrap_or_else(|e| e),
            ev.unwrap_or_else(|e| e)
        )),
    }
}

/// Raise one thread of this process to SCHED_FIFO, or failing that to a lower nice value.
#[cfg(target_os = "linux")]
fn raise_thread(tid: libc::pid_t) -> Result<String, String> {
    // SAFETY: an all-zero sched_param is valid (musl's has fields beyond sched_priority).
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    param.sched_priority = RT_PRIORITY;
    // The raw syscall, which on Linux sets one thread: musl's sched_setscheduler() is a stub
    // that always fails (ENOSYS).
    // SAFETY: a thread id of this process and a valid sched_param that outlives the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_sched_setscheduler,
            tid,
            libc::SCHED_FIFO,
            &raw const param,
        )
    };
    if rc == 0 {
        return Ok(format!("SCHED_FIFO {RT_PRIORITY}"));
    }
    let fifo = std::io::Error::last_os_error();
    // A Linux thread has its own nice value, set through its thread id.
    // SAFETY: plain syscall on a thread id of this process.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, NICE) } == 0 {
        Ok(format!("nice {NICE} (SCHED_FIFO: {fifo})"))
    } else {
        let nice = std::io::Error::last_os_error();
        Err(format!("SCHED_FIFO: {fifo}; nice {NICE}: {nice}"))
    }
}

/// The thread ids of this process's USB event threads (normally one).
#[cfg(target_os = "linux")]
fn usb_event_threads() -> Vec<libc::pid_t> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    tasks
        .flatten()
        .filter(|t| {
            std::fs::read_to_string(t.path().join("comm"))
                .is_ok_and(|comm| comm.trim_end() == USB_EVENT_THREAD)
        })
        .filter_map(|t| t.file_name().to_str()?.parse().ok())
        .collect()
}

/// Raise the calling thread (other Unix: SCHED_FIFO only, the backend's threads untouched).
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn raise_stream_threads() -> Result<String, String> {
    // SAFETY: an all-zero sched_param is valid.
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    param.sched_priority = RT_PRIORITY;
    // SAFETY: pthread_self() is the calling thread; `param` lives across the call.
    let rc = unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if rc == 0 {
        Ok(format!("SCHED_FIFO {RT_PRIORITY}"))
    } else {
        Err(format!("SCHED_FIFO: OS error {rc}"))
    }
}

/// Raise the calling thread (Windows: THREAD_PRIORITY_HIGHEST, within the host process's
/// priority class). The WinUSB backend waits for its transfers on the calling thread, so
/// there is no other thread to raise.
#[cfg(windows)]
pub(crate) fn raise_stream_threads() -> Result<String, String> {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    };
    // SAFETY: GetCurrentThread returns the calling thread's pseudo handle, valid here.
    if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) } != 0 {
        Ok("THREAD_PRIORITY_HIGHEST".to_owned())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn raise_stream_threads() -> Result<String, String> {
    Err("not supported on this platform".to_owned())
}
