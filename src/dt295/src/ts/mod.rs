//! Pure MPEG-TS / PSI logic used by the stream worker -- no device I/O, so all of it
//! is unit-tested.
//!
//! * [`crc`]    CRC-32/MPEG-2 (via the `crc` crate).
//! * [`es`]     whether the video / audio come out descrambled (PES headers).
//! * [`packet`] 188-byte packet header helpers and packet-grid sync.
//! * [`pat`]    PAT checks on single packets, section search and PAT parsing.
//! * [`pmt`]    PMT parsing (the ES of a program and their ECMs).
//! * [`section`] section reassembly per PID, descriptor loops.
//! * [`link`]   the device's link-layer AES decryption and the link-key check.
//! * [`stats`]  per-stream header statistics (continuity errors, TEI, PIDs, ...).
//! * [`psi`]    the satellite PSI keeper (cache, re-send, CS service reduction).
//! * [`route`]  the PID routes of an all-services stream (PAT / PMT -> route slots).

pub mod crc;
pub mod es;
pub mod link;
pub mod packet;
pub mod pat;
pub(crate) mod pmt;
pub mod psi;
pub mod route;
pub mod section;
pub mod stats;

#[cfg(test)]
pub(crate) mod testutil;
