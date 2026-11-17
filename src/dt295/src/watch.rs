//! When a host should start the stream over: for the B-CAS card ([`CardWatch`]), or for
//! a firmware that stopped acking ([`StallWatch`]).
//!
//! A card pulled or inserted while streaming leaves the stream running without its
//! video and audio: the firmware does not take the card up again by itself, and a
//! channel change does not bring it back. Starting over on a re-initialized device
//! ([`Device::start`], the tune, the stream) does (hardware 2026-10-28). The first
//! start-over after a card event first tries the quicker way, the card session
//! reopened in place with the stream stopped ([`Device::resume_in_place`]; see
//! [`StartOver::in_place`]). A card just inserted may take tens of seconds before it
//! works, so a start-over that leaves the stream without video / audio is tried again
//! a few times, further apart each time, with the device restarted. Nothing is started
//! over while no card is in (the firmware's B-CAS state 0), nor for missing video /
//! audio while the card works (state 3): the card has no contract for that service
//! (hardware 2026-10-30, a card without contracts on a pay channel).
//!
//! [`Device::start`]: crate::Device::start
//! [`Device::resume_in_place`]: crate::Device::resume_in_place

use std::time::{Duration, Instant};

use crate::Live;
use crate::s1::{CARD_WORKS, NO_CARD};

/// Quiet time after the last card event before starting over (pulling a card and
/// putting one in makes two events).
const SETTLE: Duration = Duration::from_secs(5);
/// Start-overs per cycle, and the wait after each before the next one (when the stream
/// is still without video / audio then).
const MAX_TRIES: usize = 4;
const WAITS: [Duration; MAX_TRIES - 1] = [
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(40),
];
/// Card events this soon after a start-over count as its own, not the user's.
const OWN_EVENTS: Duration = Duration::from_secs(5);
/// A stream without video / audio this long and no card event (a card changed while
/// nothing streamed, or a start that did not take the card up) starts a cycle too,
/// once per tune the host asked for.
const SCRAMBLED_FOR: Duration = Duration::from_secs(15);

/// What [`CardWatch::poll`] says for a card event.
const CARD_IN: &str = "a B-CAS card was put in";

/// A start-over [`CardWatch::poll`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartOver {
    /// For the log.
    pub why: &'static str,
    /// The first after a card event: try [`Device::resume_in_place`] before restarting
    /// the device. A later one (the video / audio still missing) restarts the device.
    ///
    /// [`Device::resume_in_place`]: crate::Device::resume_in_place
    pub in_place: bool,
}

/// Watches a running stream for B-CAS card events, missing video / audio and a firmware
/// that stopped acking ([`StallWatch`]), and says when to start the stream over. Lives
/// as long as the opened device.
#[derive(Debug)]
pub struct CardWatch {
    /// Card events already handled.
    seen: u32,
    /// The last card event not acted on yet.
    event_at: Option<Instant>,
    /// Start-overs made in this cycle, and when the next one is due if still missing.
    tries: usize,
    next: Option<Instant>,
    last_start_over: Option<Instant>,
    missing_since: Option<Instant>,
    /// A stream without video / audio may still start a cycle for the current tune.
    may_self_start: bool,
    stall: StallWatch,
}

impl CardWatch {
    pub fn new(live: &Live) -> Self {
        Self {
            seen: live.card_events(),
            event_at: None,
            tries: 0,
            next: None,
            last_start_over: None,
            missing_since: None,
            may_self_start: true,
            stall: StallWatch::new(),
        }
    }

    /// The host tuned a channel it was asked for.
    pub fn tuned(&mut self) {
        self.may_self_start = true;
        self.missing_since = None;
        self.stall.tuned();
    }

    /// Call while a stream runs, about once a second or more often; `stalled`: the
    /// firmware stopped acking ([`Device::needs_recovery`]). `Some`: stop the stream,
    /// [`Device::start`] the device again (or first [`StartOver::in_place`]), tune the
    /// channel again and start the stream, then call [`CardWatch::started_over`].
    ///
    /// [`Device::needs_recovery`]: crate::Device::needs_recovery
    /// [`Device::start`]: crate::Device::start
    pub fn poll(&mut self, live: &Live, stalled: bool, now: Instant) -> Option<StartOver> {
        let why = self.step(live.card_events(), live.card_state(), live.es_clear(), now);
        self.start_over(why, stalled, now)
    }

    fn start_over(
        &mut self,
        why: Option<&'static str>,
        stalled: bool,
        now: Instant,
    ) -> Option<StartOver> {
        if let Some(why) = why {
            let in_place = why == CARD_IN;
            return Some(StartOver { why, in_place });
        }
        (stalled && self.stall.start_over(now)).then_some(StartOver {
            why: "the firmware stopped acking",
            in_place: false,
        })
    }

    fn step(
        &mut self,
        events: u32,
        state: Option<u8>,
        clear: Option<bool>,
        now: Instant,
    ) -> Option<&'static str> {
        if events != self.seen {
            self.seen = events;
            let own = self
                .last_start_over
                .is_some_and(|t| now.duration_since(t) < OWN_EVENTS);
            if !own {
                self.event_at = Some(now);
                self.tries = 0;
                self.next = None;
            }
        }
        // no card in (pulled, or stage 2 found none): nothing to start over for
        let card_in = state != Some(NO_CARD);
        // a working card without video / audio: no contract, not a lost card session
        let card_lost = card_in && state != Some(CARD_WORKS);
        if let Some(t) = self.event_at {
            if now.duration_since(t) < SETTLE {
                return None;
            }
            self.event_at = None;
            return card_in.then_some(CARD_IN);
        }
        if let Some(next) = self.next {
            if now < next {
                return None;
            }
            self.next = None;
            return (card_lost && clear == Some(false))
                .then_some("the video / audio are still missing");
        }
        if clear == Some(false) && card_lost {
            let since = *self.missing_since.get_or_insert(now);
            if self.may_self_start && now.duration_since(since) >= SCRAMBLED_FOR {
                self.may_self_start = false;
                self.missing_since = None;
                self.tries = 0;
                return Some("the video / audio are missing");
            }
        } else {
            self.missing_since = None;
        }
        None
    }

    /// The host started the stream over (whether that worked or not).
    pub fn started_over(&mut self, now: Instant) {
        self.last_start_over = Some(now);
        self.missing_since = None;
        self.tries += 1;
        self.next = WAITS.get(self.tries - 1).map(|&w| now + w);
    }
}

/// Start-overs in a row for a firmware that stopped acking, each within [`IN_A_ROW`]
/// of the one before; the next one waits until then.
const STALL_TRIES: u32 = 3;
const IN_A_ROW: Duration = Duration::from_mins(1);

/// Says when a host should start a stream over because the firmware stopped acking
/// ([`Device::needs_recovery`]). It does within seconds of a stream's start more often
/// the more ECMs the stream descrambles (CS110's ND06), and a device started again
/// streams on (hardware 2026-10-31). Without a start-over the stream stays silent.
///
/// [`Device::needs_recovery`]: crate::Device::needs_recovery
#[derive(Debug, Default)]
pub struct StallWatch {
    tries: u32,
    last: Option<Instant>,
}

impl StallWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The host tuned a channel it was asked for.
    pub fn tuned(&mut self) {
        *self = Self::default();
    }

    /// The firmware stopped acking (call while it has not been started again): `true`
    /// = stop the stream, [`Device::start`] the device again, tune the channel again
    /// and start the stream. `false`: it did so `STALL_TRIES` times in a row; the
    /// next one waits a minute.
    ///
    /// [`Device::start`]: crate::Device::start
    pub fn start_over(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|t| now.duration_since(t) >= IN_A_ROW) {
            self.tries = 0;
        }
        if self.tries >= STALL_TRIES {
            return false;
        }
        self.tries += 1;
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch() -> CardWatch {
        CardWatch {
            seen: 0,
            event_at: None,
            tries: 0,
            next: None,
            last_start_over: None,
            missing_since: None,
            may_self_start: true,
            stall: StallWatch::new(),
        }
    }

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    const IN: Option<u8> = Some(1);
    const OUT: Option<u8> = Some(NO_CARD);

    #[test]
    fn card_event_settles_then_retries_while_missing() {
        let t0 = Instant::now();
        let mut w = watch();
        assert_eq!(w.step(0, None, Some(true), t0), None);
        // pulled at 1 s, put back at 4 s: one start-over, 5 s after the last event
        assert_eq!(w.step(1, OUT, Some(false), at(t0, 1)), None);
        assert_eq!(w.step(2, IN, Some(false), at(t0, 4)), None);
        assert_eq!(w.step(2, IN, Some(false), at(t0, 8)), None);
        assert!(w.step(2, IN, Some(false), at(t0, 9)).is_some());
        w.started_over(at(t0, 9));
        // its own card event (within 5 s) is not a new one
        assert_eq!(w.step(3, IN, None, at(t0, 11)), None);
        // still without video / audio 10 s later: again, then 20 s and 40 s later
        assert_eq!(w.step(3, IN, Some(false), at(t0, 18)), None);
        assert!(w.step(3, IN, Some(false), at(t0, 19)).is_some());
        w.started_over(at(t0, 19));
        assert!(w.step(3, IN, Some(false), at(t0, 39)).is_some());
        w.started_over(at(t0, 39));
        assert!(w.step(3, IN, Some(false), at(t0, 79)).is_some());
        w.started_over(at(t0, 79));
        // four tries: the cycle ends; one cycle for missing video / audio is still allowed later
        assert_eq!(w.step(3, IN, Some(false), at(t0, 200)), None);
        assert!(w.step(3, IN, Some(false), at(t0, 215)).is_some());
    }

    #[test]
    fn a_pulled_card_alone_starts_nothing() {
        let t0 = Instant::now();
        let mut w = watch();
        assert_eq!(w.step(1, OUT, Some(false), t0), None);
        assert_eq!(w.step(1, OUT, Some(false), at(t0, 6)), None);
        // nor the stream staying without video / audio meanwhile
        assert_eq!(w.step(1, OUT, Some(false), at(t0, 7)), None);
        assert_eq!(w.step(1, OUT, Some(false), at(t0, 7 + 30)), None);
        // put back later: that one does
        assert_eq!(w.step(2, IN, Some(false), at(t0, 40)), None);
        assert!(w.step(2, IN, Some(false), at(t0, 45)).is_some());
    }

    #[test]
    fn a_clear_stream_ends_the_cycle() {
        let t0 = Instant::now();
        let mut w = watch();
        assert_eq!(w.step(1, IN, Some(false), t0), None);
        assert!(w.step(1, IN, Some(false), at(t0, 5)).is_some());
        w.started_over(at(t0, 5));
        assert_eq!(w.step(1, IN, Some(true), at(t0, 15)), None);
        assert_eq!(w.step(1, IN, Some(true), at(t0, 100)), None);
    }

    #[test]
    fn a_working_card_without_av_has_no_contract() {
        let t0 = Instant::now();
        let mut w = watch();
        const WORKS: Option<u8> = Some(CARD_WORKS);
        // a pay service the card has no contract for: nothing to start over
        assert_eq!(w.step(0, WORKS, Some(false), t0), None);
        assert_eq!(w.step(0, WORKS, Some(false), at(t0, 30)), None);
        // a card put in: one start-over, no retries once it works
        assert_eq!(w.step(1, IN, Some(false), at(t0, 31)), None);
        assert!(w.step(1, IN, Some(false), at(t0, 36)).is_some());
        w.started_over(at(t0, 36));
        assert_eq!(w.step(1, WORKS, Some(false), at(t0, 46)), None);
        assert_eq!(w.step(1, WORKS, Some(false), at(t0, 200)), None);
    }

    #[test]
    fn missing_av_without_events_once_per_tune() {
        let t0 = Instant::now();
        let mut w = watch();
        assert_eq!(w.step(0, None, Some(false), t0), None);
        assert_eq!(w.step(0, None, None, at(t0, 10)), None); // unknown resets the clock
        assert_eq!(w.step(0, None, Some(false), at(t0, 11)), None);
        assert!(w.step(0, None, Some(false), at(t0, 26)).is_some());
        w.started_over(at(t0, 26));
        for s in [36, 56, 96] {
            assert!(w.step(0, None, Some(false), at(t0, s)).is_some());
            w.started_over(at(t0, s));
        }
        assert_eq!(w.step(0, None, Some(false), at(t0, 500)), None); // no more for this tune
        w.tuned();
        assert_eq!(w.step(0, None, Some(false), at(t0, 501)), None);
        assert!(w.step(0, None, Some(false), at(t0, 516)).is_some());
    }

    #[test]
    fn a_stall_starts_over_with_the_device_a_card_event_in_place() {
        let t0 = Instant::now();
        let mut w = watch();
        assert_eq!(w.start_over(None, false, t0), None);
        let stall = w.start_over(None, true, t0).unwrap();
        assert!(!stall.in_place);
        // the card first; for the video / audio, the device
        assert!(w.start_over(Some(CARD_IN), true, t0).unwrap().in_place);
        assert!(!w.start_over(Some("missing"), false, t0).unwrap().in_place);
        // a tune counts the stalls from 0
        assert!(w.start_over(None, true, t0).is_some());
        assert!(w.start_over(None, true, t0).is_some());
        assert_eq!(w.start_over(None, true, t0), None);
        w.tuned();
        assert!(w.start_over(None, true, t0).is_some());
    }

    #[test]
    fn stalls_start_over_three_times_in_a_row() {
        let t0 = Instant::now();
        let mut s = StallWatch::new();
        assert!(s.start_over(at(t0, 0)));
        assert!(s.start_over(at(t0, 5)));
        assert!(s.start_over(at(t0, 10)));
        assert!(!s.start_over(at(t0, 15)));
        assert!(!s.start_over(at(t0, 69)));
        // a minute after the last one
        assert!(s.start_over(at(t0, 70)));
        // a stream that ran a minute counts again from 0; so does a tune
        assert!(s.start_over(at(t0, 200)));
        assert!(s.start_over(at(t0, 201)));
        assert!(s.start_over(at(t0, 202)));
        assert!(!s.start_over(at(t0, 203)));
        s.tuned();
        assert!(s.start_over(at(t0, 204)));
    }

    #[test]
    fn a_new_event_restarts_the_cycle() {
        let t0 = Instant::now();
        let mut w = watch();
        assert!(w.step(1, IN, Some(false), at(t0, 0)).is_none());
        assert!(w.step(1, IN, Some(false), at(t0, 5)).is_some());
        w.started_over(at(t0, 5));
        // pulled and put back later: settles, then starts over at once (tries begin again)
        assert_eq!(w.step(2, OUT, Some(false), at(t0, 12)), None);
        assert_eq!(w.step(3, IN, Some(false), at(t0, 13)), None);
        assert!(w.step(3, IN, Some(false), at(t0, 18)).is_some());
    }
}
