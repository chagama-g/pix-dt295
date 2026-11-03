//! TVTest BonDriver for the PIX-DT295.
//!
//! `BonDriver_DT295.dll` exports one function, `CreateBonDriver`, which returns an
//! `IBonDriver2` object: a small C++ class (`cpp/bonshim.cpp`, so the host's
//! `dynamic_cast` sees real MSVC RTTI) forwarding every method to [`driver::Driver`]
//! through a C function table ([`ffi`]).
//!
//! * [`config`]   -- `BonDriver_DT295.ini` (`[Tuner]`, `[BS]`, `[CS110]`)
//! * [`channels`] -- tuning spaces: built-in UHF / CATV tables, satellite ini lines
//! * [`persist`]  -- the state file rules (link key, PSI cache)
//! * [`fifo`]     -- the TS FIFO between the stream worker and `GetTsStream`
//! * [`logging`]  -- `HH:MM:SS.mmm [BonDriver_DT295] ...` to DebugView and `LogFile`
//! * [`driver`]   -- the BonDriver itself (OpenTuner, SetChannel, the idle keeper, ...)
#![allow(non_snake_case)] // the crate (and DLL) is named BonDriver_DT295

pub mod channels;
pub mod config;
pub mod driver;
pub mod fifo;
pub mod logging;
pub mod persist;
pub mod sys;

#[cfg(all(windows, target_env = "msvc"))]
pub mod ffi;

#[cfg(all(windows, target_env = "msvc"))]
pub use ffi::CreateBonDriver;
