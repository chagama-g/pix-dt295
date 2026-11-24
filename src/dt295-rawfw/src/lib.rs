//! PIX-DT295 blob-free raw-TS path.
//!
//! This crate carries the host side of the blob-free raw-TS capture: the blob-free COLD
//! stream worker ([`rawstream`]), our own FR monitor host side ([`frmon_host`]) and the
//! blob-free demod I2C ([`fjcore`]). It is built on the hardware primitives in `dt295-core`.
//!
//! Logging uses `tracing`; the host installs the subscriber.

pub mod fjcore;
pub mod frmon_host;
mod prio;
#[cfg(windows)]
mod timer_policy;
pub mod rawstream;

pub use rawstream::{RAW_FIRST_TS, RAW_STALL, RAW_STREAM_CAP, RawCaptureStats, raw_stall_due};
