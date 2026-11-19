//! Channel definitions, without the device: the text of a setting file ([`text`]), an
//! ini file as the Win32 profile API reads it ([`profile`]), the number syntax of C
//! ([`num`]) and the BS / CS110 / ISDB-T frequency plan ([`plan`]). The BonDriver and the
//! `dt295` command read their ini with these, through `dt295-core`.
//!
//! And the channel files of other BonDrivers ([`load`]), read as those drivers read them,
//! with their tuning spaces and channels numbered as those drivers number them (so a
//! TVTest `.ch2` or an EDCB channel list made with the other driver still fits): a
//! BonDriver_BDA ini ([`bda`]), a ChSet.txt of the PT drivers ([`chset`]) and an ini of
//! the BonDriver_PX series ([`px`]).

use std::fmt;
use std::path::Path;

pub mod bda;
pub mod chset;
pub mod num;
pub mod plan;
pub mod profile;
pub mod px;
pub mod text;

/// A channel file of another BonDriver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFile {
    pub format: Format,
    /// The tuning spaces, numbered from 0 as the driver numbers them.
    pub spaces: Vec<Space>,
}

/// The driver a channel file is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `BonDriver_BDA.ini` (`[TuningSpace00]` ...).
    Bda,
    /// `<name>.ChSet.txt` of BonDriver_PT-ST / PT3-ST and their forks.
    ChSet,
    /// `BonDriver_PX_*.ini` (`[BS]` / `[CS110]` with `CHANNEL_NUM`).
    Px,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::Bda => "BonDriver_BDA ini",
            Format::ChSet => "ChSet.txt",
            Format::Px => "BonDriver_PX ini",
        })
    }
}

/// A tuning space: its channels are numbered from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    pub name: String,
    pub channels: Vec<Channel>,
}

/// One channel of a tuning space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub tuning: Tuning,
}

/// What a channel tunes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tuning {
    /// ISDB-T at this centre frequency.
    Terrestrial { hz: u32 },
    /// ISDB-S at this IF (the frequency out of the LNB), this TS of the transponder.
    Satellite { if_hz: u32, ts: Ts },
    /// A number the driver lists but that is no channel to tune here, and why (another
    /// modulation, a number without a definition, an IF out of the tuner's range ...).
    Untunable(String),
}

/// Which TS of a satellite transponder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ts {
    /// None given: the one the demod streams first.
    First,
    /// The TS whose TSID has these low 3 bits (0-7), as BonDriver_PTx-ST_mod reads a TSID
    /// of 7 or less: the relative TS number.
    Number(u8),
    /// The TS of this TSID.
    Id(u16),
}

/// A satellite TSID of this or less is a TS number ([`Ts::Number`]): the TS whose TSID
/// has these low 3 bits.
pub const MAX_TS_NUMBER: u8 = 0b111;

impl Ts {
    /// A TSID field: 0-7 is a TS number, else a TSID (16 bits).
    pub fn from_field(v: u32) -> Self {
        match u8::try_from(v) {
            Ok(n) if n <= MAX_TS_NUMBER => Ts::Number(n),
            _ => Ts::Id(v as u16),
        }
    }
}

/// Why a channel file could not be read.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The text is no ini file (`rust-ini` refused it after the profile API's cleanup).
    Ini(profile::ParseError),
    /// None of the formats.
    Unknown,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Ini(e) => write!(f, "not an ini file ({e})"),
            Error::Unknown => f.write_str(
                "not a channel file of BonDriver_BDA, the PT drivers (ChSet.txt) or the \
                 BonDriver_PX series",
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Read a channel file ([`parse`]); its name tells a ChSet.txt for ISDB-T from one for
/// ISDB-S ([`chset::is_satellite`]). A ChSet.txt that is not UTF-8 is CP932 (the PT
/// drivers convert it with code page 932); the ini files are read as the profile API
/// reads them ([`text::decode`]).
pub fn load(path: &Path) -> Result<ChannelFile, Error> {
    let bytes = std::fs::read(path)?;
    let mut text = text::decode(&bytes);
    if chset::is_chset(&text) {
        text = text::decode_cp932(&bytes);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    parse(&text, &name)
}

/// The channel file in `text` (`file_name` is its name): a ChSet.txt when it has a
/// tuning space line (`$name<TAB>n`), else an ini of BonDriver_BDA (a `[TuningSpaceNN]`
/// or `[Channel]` with channels) or of the BonDriver_PX series (`[BS]` / `[CS110]` with
/// `CHANNEL_NUM` or `CHnnn` lines).
pub fn parse(text: &str, file_name: &str) -> Result<ChannelFile, Error> {
    if chset::is_chset(text) {
        let spaces = chset::parse(text, chset::is_satellite(file_name));
        return Ok(ChannelFile {
            format: Format::ChSet,
            spaces,
        });
    }
    let ini = profile::parse(text).map_err(Error::Ini)?;
    if bda::is_bda(&ini) {
        return Ok(ChannelFile {
            format: Format::Bda,
            spaces: bda::parse(&ini),
        });
    }
    if px::is_px(&ini) {
        return Ok(ChannelFile {
            format: Format::Px,
            spaces: px::parse(&ini),
        });
    }
    Err(Error::Unknown)
}

/// A satellite channel at `if_khz` (the IF in kHz), or why it is none: an IF outside
/// the tuner's range.
fn satellite(if_khz: i64, ts: Ts) -> Tuning {
    match u32::try_from(if_khz) {
        Ok(khz) if plan::SAT_IF_KHZ.contains(&khz) => Tuning::Satellite {
            if_hz: khz * 1000,
            ts,
        },
        _ => Tuning::Untunable(format!(
            "IF {:.3} MHz is outside the tuner's 950-2150 MHz",
            if_khz as f64 / 1000.0
        )),
    }
}

/// A placeholder for a number without a definition.
fn undefined(name: &str) -> Channel {
    Channel {
        name: name.to_owned(),
        tuning: Tuning::Untunable("no definition".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_fields() {
        assert_eq!(Ts::from_field(0), Ts::Number(0));
        assert_eq!(Ts::from_field(7), Ts::Number(7));
        assert_eq!(Ts::from_field(8), Ts::Id(8));
        assert_eq!(Ts::from_field(0x4010), Ts::Id(0x4010));
        assert_eq!(Ts::from_field(0x1_4010), Ts::Id(0x4010), "16 bits");
    }

    #[test]
    fn formats() {
        let chset = "$BS\t0\nBS01/TS0\t0\t0\t0\t16400\n";
        assert_eq!(parse(chset, "a.ChSet.txt").unwrap().format, Format::ChSet);
        let bda = "[TuningSpace00]\nCH000=1,11727.480,R,0,BS01/TS0,0x0,0x4010\n";
        assert_eq!(parse(bda, "a.ini").unwrap().format, Format::Bda);
        let px = "[BS]\nCHANNEL_NUM = 1\nCH001 = BS1/TS0,0,11727480,0x4010\n";
        assert_eq!(parse(px, "a.ini").unwrap().format, Format::Px);
        // BonDriver_DT295's own ini is none of them
        let own = "[Tuner]\nRawTS=1\n[BS]\nBS01/TS0=1049480,0x4010\n";
        assert!(matches!(parse(own, "a.ini"), Err(Error::Unknown)));
        assert!(matches!(parse("", "a.txt"), Err(Error::Unknown)));
    }

    #[test]
    fn satellite_range() {
        let t = satellite(1_049_480, Ts::First);
        assert_eq!(
            t,
            Tuning::Satellite {
                if_hz: 1_049_480_000,
                ts: Ts::First
            }
        );
        assert!(matches!(
            satellite(2_222_480, Ts::First),
            Tuning::Untunable(_)
        ));
        assert!(matches!(satellite(-5, Ts::First), Tuning::Untunable(_)));
    }
}
