//! What `dt295 tune` and `dt295 serve` say over the socket. The client sends one
//! line, the daemon answers one line, and then (after `ok`) the TS follows until
//! either side closes:
//!
//! ```text
//! > tune unit=AB12345678 ter=557142857 sid=1024
//! > tune unit=AB12345678 sat=1318000000 tsid=40f1 sid=101
//! > tune unit=AB12345678 sat=1318000000 tsid=40f1 sid=929 if-free
//! > tune unit=AB12345678 ter=557142857
//! < ok
//! < error unit AB12345678 is not connected
//! < busy unit AB12345678 is streaming
//! ```
//!
//! Frequencies are in Hz, `tsid` in hex, `sid` in decimal or `0x` hex; `tsid` is
//! optional (default: the frequency's first TS, as with `dt295 capture`), and so is
//! `sid` (default: every service of the TS). `if-free` asks for the unit
//! only if no stream runs on it (a request without it takes the unit over); `busy`
//! is the answer when one does (an older client takes it for `error`).

use dt295::chan::NO_TSID;

use crate::{parse_hex_u16, parse_u16, parse_u32};

/// The longest request line a daemon reads.
pub const MAX_LINE: u64 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tuning {
    /// ISDB-T: the channel centre frequency.
    Terrestrial { hz: u32 },
    /// ISDB-S: the IF frequency and the broadcast transport_stream_id.
    Satellite { hz: u32, tsid: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The USB serial number of the unit.
    pub unit: String,
    pub tuning: Tuning,
    pub sid: Option<u16>,
    /// Only if the unit is free: never take it over from a running stream.
    pub if_free: bool,
}

impl Request {
    /// The request line, without its newline.
    pub fn to_line(&self) -> String {
        let mut s = format!("tune unit={}", self.unit);
        match self.tuning {
            Tuning::Terrestrial { hz } => s.push_str(&format!(" ter={hz}")),
            Tuning::Satellite { hz, tsid: NO_TSID } => s.push_str(&format!(" sat={hz}")),
            Tuning::Satellite { hz, tsid } => s.push_str(&format!(" sat={hz} tsid={tsid:04x}")),
        }
        if let Some(sid) = self.sid {
            s.push_str(&format!(" sid={sid}"));
        }
        if self.if_free {
            s.push_str(" if-free");
        }
        s
    }

    pub fn parse(line: &str) -> Result<Self, String> {
        let mut words = line.split_whitespace();
        if words.next() != Some("tune") {
            return Err("expected a tune request".into());
        }
        let (mut unit, mut ter, mut sat, mut tsid, mut sid) = (None, None, None, None, None);
        let mut if_free = false;
        for word in words {
            if word == "if-free" {
                if_free = true;
                continue;
            }
            let (key, value) = word
                .split_once('=')
                .ok_or_else(|| format!("{word:?}: expected key=value"))?;
            match key {
                "unit" if !value.is_empty() => unit = Some(value.to_owned()),
                "ter" => ter = Some(parse_u32(value)?),
                "sat" => sat = Some(parse_u32(value)?),
                "tsid" => tsid = Some(parse_hex_u16(value)?),
                "sid" => sid = Some(parse_u16(value)?),
                _ => return Err(format!("{word:?}: unknown")),
            }
        }
        let unit = unit.ok_or("no unit")?;
        let tuning = match (ter, sat, tsid) {
            (Some(hz), None, None) => Tuning::Terrestrial { hz },
            (None, Some(hz), tsid) => Tuning::Satellite {
                hz,
                tsid: tsid.unwrap_or(NO_TSID),
            },
            _ => return Err("expected ter=, or sat= (with tsid=)".into()),
        };
        Ok(Self {
            unit,
            tuning,
            sid,
            if_free,
        })
    }
}

/// The daemon's answer to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The stream runs; the TS follows.
    Ok,
    /// An `if-free` request for a unit that streams.
    Busy(String),
    Error(String),
}

impl Reply {
    /// The reply line, without its newline.
    pub fn to_line(&self) -> String {
        match self {
            Self::Ok => "ok".into(),
            // one line, whatever the message holds
            Self::Busy(why) => format!("busy {}", why.replace(['\r', '\n'], " ")),
            Self::Error(why) => format!("error {}", why.replace(['\r', '\n'], " ")),
        }
    }

    pub fn parse(line: &str) -> Self {
        let line = line.trim_end();
        if line == "ok" {
            return Self::Ok;
        }
        match line.strip_prefix("busy ") {
            Some(why) => Self::Busy(why.to_owned()),
            None => Self::Error(line.trim_prefix("error ").to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        for r in [
            Request {
                unit: "AB12345678".into(),
                tuning: Tuning::Terrestrial { hz: 557_142_857 },
                sid: Some(1024),
                if_free: false,
            },
            Request {
                unit: "CD23456789".into(),
                tuning: Tuning::Satellite {
                    hz: 1_318_000_000,
                    tsid: 0x40F1,
                },
                sid: None,
                if_free: true,
            },
            Request {
                unit: "A".into(),
                tuning: Tuning::Satellite {
                    hz: 1_318_000_000,
                    tsid: NO_TSID,
                },
                sid: Some(101),
                if_free: false,
            },
        ] {
            assert_eq!(Request::parse(&r.to_line()), Ok(r));
        }
        assert_eq!(
            Request::parse("tune unit=A sat=1318000000").unwrap().tuning,
            Tuning::Satellite {
                hz: 1_318_000_000,
                tsid: NO_TSID
            },
            "the first TS of the frequency"
        );
        assert_eq!(
            Request::parse("tune unit=A sat=1318000000 tsid=0x40F1 sid=0x65").unwrap(),
            Request {
                unit: "A".into(),
                tuning: Tuning::Satellite {
                    hz: 1_318_000_000,
                    tsid: 0x40F1
                },
                sid: Some(101),
                if_free: false,
            }
        );
    }

    #[test]
    fn bad_requests() {
        for line in [
            "",
            "play unit=A ter=1",
            "tune ter=557142857",
            "tune unit= ter=1",
            "tune unit=A",
            "tune unit=A ter=1 tsid=1",
            "tune unit=A ter=1 sat=2 tsid=1",
            "tune unit=A ter=x",
            "tune unit=A ter=1 lnb=on",
            "tune unit=A ter",
        ] {
            assert!(Request::parse(line).is_err(), "{line:?}");
        }
    }

    #[test]
    fn replies() {
        assert_eq!(Reply::parse("ok\n"), Reply::Ok);
        let e = Reply::Error("no lock\n(C/N 0.0 dB)".into());
        assert_eq!(e.to_line(), "error no lock (C/N 0.0 dB)");
        assert_eq!(
            Reply::parse(&e.to_line()),
            Reply::Error("no lock (C/N 0.0 dB)".into())
        );
        assert_eq!(Reply::parse("garbage"), Reply::Error("garbage".into()));
        let b = Reply::Busy("unit A is streaming".into());
        assert_eq!(b.to_line(), "busy unit A is streaming");
        assert_eq!(Reply::parse(&b.to_line()), b);
    }
}
