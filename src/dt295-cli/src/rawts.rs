//! The blob-free raw-TS stream as `dt295 capture`, `dt295 logo` and `dt295 serve` run it:
//! bring the chip up cold (once per plug) or only retune, wait for the first TS, and say
//! why none came. No vendor firmware, no secure session, no state file: the TS stays
//! scrambled and the host descrambles it with a real card.

use std::thread;
use std::time::{Duration, Instant};

use dt295::chan::NO_TSID;
use dt295::{Device, RAW_FIRST_TS, RAW_STREAM_CAP, TsSink};

use crate::serve::proto::Tuning;
use crate::tuner::stopping;

/// Start a raw-TS stream of `tuning` into `sink`: the whole bring-up on a chip fresh from
/// a plug (`Ok(true)`), else a retune + rearm (`Ok(false)`).
pub fn start(dev: &mut Device, tuning: Tuning, sink: TsSink) -> Result<bool, String> {
    // A previous worker that would not stop still holds the device I/O; cold_monitor_installed
    // (and the stream start) below would block on its lock forever. Refuse instead of
    // deadlocking; a re-plug is the reset. Covers the initial tune and every restart path.
    if dev.raw_present() {
        return Err("the previous raw-TS worker has not stopped; re-plug the unit".to_owned());
    }
    let (terrestrial, hz, tsid) = match tuning {
        Tuning::Terrestrial { hz } => (true, hz, None),
        Tuning::Satellite { hz, tsid } => (false, hz, (tsid != NO_TSID).then_some(tsid)),
    };
    // Decide the bring-up from the chip, not a flag: our monitor BODY persists across a
    // retune and a reopen (another process too), and clears only when a re-plug wipes RAM.
    // The first tune after a cold open does the whole bring-up; a later one only retunes +
    // rearms. A "TS flowed" flag would mark an off-air-but-brought-up chip cold and re-run
    // the once-per-plug bootstrap, which faults. (Same reasoning as the BonDriver.)
    let full = !dev.cold_monitor_installed();
    dev.stream_start_cold(terrestrial, hz, tsid, full, RAW_STREAM_CAP, sink)
        .map(|()| full)
        .map_err(|e| format!("raw stream start: {e}"))
}

/// Wait (up to [`RAW_FIRST_TS`]) for the worker's first TS (`bytes`: what it delivered so
/// far, monotonic), or until it ends (a fault or an unplug before any TS) or the process
/// is told to stop ([`stopping`]: the caller tells that case apart).
pub fn wait_first_ts(dev: &Device, bytes: impl Fn() -> u64) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < RAW_FIRST_TS {
        if bytes() > 0 {
            return true;
        }
        if !dev.raw_running() || stopping() {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    bytes() > 0
}

/// After [`wait_first_ts`] came back without TS: stop the worker and say why (`full`: what
/// [`start`] gave, whether it was the bring-up or a retune). It ends on its own, in about
/// a tune's time: with nothing (Ok) when the demod does not lock, with the error when the
/// tune fails. Only a worker still running at the deadline waited the whole time. "no
/// lock" is also what a channel scan skips quietly.
pub fn why_no_ts(dev: &mut Device, full: bool) -> String {
    let running = dev.raw_running();
    match (running, dev.stream_stop_raw()) {
        (false, Ok(_)) => "no lock".to_owned(),
        (false, Err(e)) if full => format!("the cold bring-up failed: {e}"),
        (false, Err(e)) => format!("the retune failed: {e}"),
        (true, Ok(_)) => format!("no TS within {RAW_FIRST_TS:?}"),
        (true, Err(e)) => {
            format!("no TS within {RAW_FIRST_TS:?}, and the stream worker failed: {e}")
        }
    }
}
