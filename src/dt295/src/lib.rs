//! PIX-DT295 user-mode driver core.
//!
//! * [`Device`] -- open, boot + authenticate ([`Device::start`]), tune
//!   ([`Device::tune_t`] / [`Device::tune_s`]), and stream one service or every
//!   service of the TS as descrambled, link-decrypted MPEG-TS
//!   ([`Device::stream_start`]) on the System1 path (trc firmware, EP 0x81).
//! * [`ts`] -- pure TS / PSI logic (CRC, PAT checks, link decryption, the
//!   satellite PSI keeper, stream statistics); no device I/O, unit-tested.
//! * [`state`] -- the host's persistent state file (link key, PSI).
//! * [`chan`] -- the built-in ISDB-T channel tables and the satellite ini line.
//! * [`CardWatch`] -- when a host should start a stream over for the B-CAS card;
//!   [`StallWatch`] -- for a firmware that stopped acking.
//!
//! Logging uses `tracing`; the host installs the subscriber.

pub mod chan;
pub mod error;
pub mod state;
pub mod ts;

mod auth;
mod cas;
mod device;
mod dtvdec;
mod fe1;
mod fw;
mod i2c;
mod proto;
mod s1;
mod signal;
mod watch;

pub use device::{Device, KeyJournal, Live};
pub use dt295_usb::Listed;
pub use error::{Error, Result};
pub use watch::{CardWatch, StallWatch, StartOver};

/// The present PIX-DT295 units with their USB serial numbers, in the order
/// [`Device::open`] counts them; each is opened and closed again (no transfers) to
/// tell whether it is free.
pub fn list() -> Result<Vec<Listed>> {
    Ok(dt295_usb::list(dt295_usb::VID, dt295_usb::PID)?)
}

/// The USB serial numbers of the present PIX-DT295 units, without opening any.
pub fn present_serials() -> Result<Vec<String>> {
    Ok(dt295_usb::present_serials(dt295_usb::VID, dt295_usb::PID)?)
}

/// Broadcast system of a tune.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    /// ISDB-T (terrestrial, CATV pass-through).
    Terrestrial,
    /// ISDB-S (BS / CS110).
    Satellite,
}

/// Demodulator status read at tune time ([`Device::read_status`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Status {
    pub locked: bool,
    /// C/N in 0.001 dB.
    pub cn_mdb: i32,
    /// The raw C/N register value (for diagnostics).
    pub raw: u32,
}

/// Receives the TS of a running stream, on the stream worker thread, in 188-byte
/// packet multiples. Must not block for long (the device buffers only ~100 ms).
pub type TsSink = Box<dyn FnMut(&[u8]) + Send + 'static>;

/// One cached satellite PSI section ([`Device::psi_export`] / [`Device::psi_import`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsiRecord {
    /// 1 PAT, 2 PMT, 3 NIT, 4 SDT.
    pub kind: u8,
    /// An SDT kept from SDT\[other\] (table 0x46).
    pub other: bool,
    /// transport_stream_id (PAT/PMT/SDT) or network_id (NIT).
    pub id: u16,
    /// program_number (PMT) or section_number (NIT/SDT).
    pub sub: u16,
    /// Day (Unix time / 86400) the device last delivered it.
    pub seen_day: u32,
    /// The full section, table_id .. CRC_32.
    pub section: Vec<u8>,
}

/// Sections not delivered for this many days are dropped on import.
pub const PSI_KEEP_DAYS: u32 = 90;

/// Today as a Unix day number (the unit of [`PsiRecord::seen_day`]).
pub fn today() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs / 86_400) as u32
}
