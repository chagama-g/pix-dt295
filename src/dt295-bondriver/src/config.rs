//! `BonDriver_DT295.ini` (next to the DLL, same base name), read with `rust-ini` under
//! the Win32 profile API's rules (`GetPrivateProfileStringW` and friends).
//!
//! `[Tuner]`: `Firmware`, `TrcFirmware`, `DeviceIndex`, `LogFile`, `LogLevel`,
//! `StateFile`, `UHF`, `CATV`. `[BS]` / `[CS110]`: one `name=IF_kHz,TSID[,ServiceID]` line per channel.
//! Section and key names are case-insensitive; backslashes are literal (escapes off).

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use ini::{Ini, ParseOption};
use tracing::level_filters::LevelFilter;

use crate::channels::{Space, catv_space, satellite_channel, uhf_space};

const DEFAULT_FIRMWARE: &str = "mb86m01_assp_osec_idle.bin";
const DEFAULT_TRC_FIRMWARE: &str = "mb86m01_assp_osec_trc_m_h.bin";

/// Paths derived from the DLL's own file name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModulePaths {
    /// Folder of the DLL including the trailing separator; empty
    /// if the module path is unknown.
    pub dir: OsString,
    /// The DLL path without its extension (`...\BonDriver_DT295`).
    pub stem: OsString,
}

impl ModulePaths {
    /// Split a module file name: the last `\` or `/` ends the folder, the last `.`
    /// starts the extension.
    pub fn from_module_path(path: &OsStr) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let w: Vec<u16> = path.encode_wide().collect();
            let dir_end = w
                .iter()
                .rposition(|&c| c == u16::from(b'\\') || c == u16::from(b'/'))
                .map_or(0, |i| i + 1);
            let stem_end = w
                .iter()
                .rposition(|&c| c == u16::from(b'.'))
                .unwrap_or(w.len());
            Self {
                dir: OsString::from_wide(&w[..dir_end]),
                stem: OsString::from_wide(&w[..stem_end]),
            }
        }
        #[cfg(not(windows))]
        {
            let s = path.to_string_lossy();
            let dir_end = s.rfind(['\\', '/']).map_or(0, |i| i + 1);
            let stem_end = s.rfind('.').unwrap_or(s.len());
            Self {
                dir: OsString::from(&s[..dir_end]),
                stem: OsString::from(&s[..stem_end]),
            }
        }
    }

    /// `BonDriver_DT295<ext>` next to the DLL.
    pub fn sibling(&self, ext: &str) -> PathBuf {
        let mut s = self.stem.clone();
        s.push(ext);
        PathBuf::from(s)
    }

    /// An ini path: absolute (`X:...` or `\\...`) as is, else appended to the DLL
    /// folder (plain string concatenation).
    pub fn resolve(&self, value: &str) -> PathBuf {
        if is_absolute(value) {
            PathBuf::from(value)
        } else {
            let mut s = self.dir.clone();
            s.push(value);
            PathBuf::from(s)
        }
    }
}

/// A drive letter path or a UNC path.
fn is_absolute(p: &str) -> bool {
    let mut c = p.chars();
    matches!(
        (c.next(), c.next()),
        (Some(_), Some(':')) | (Some('\\'), Some('\\'))
    )
}

/// What `LoadConfig` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The idle firmware (`[Tuner] Firmware`).
    pub firmware: PathBuf,
    /// The System1 transcoder firmware; `None` (an empty `TrcFirmware=`) = no stream
    /// can start (the Rust port has only the System1 path).
    pub trc_firmware: Option<PathBuf>,
    /// Which device to open: `[Tuner] DeviceSerial`, else `DeviceIndex`.
    pub device: DeviceSel,
    /// `<dll>.log` with `[Tuner] LogFile=1`; `None` (absent or `0`) = OutputDebugString
    /// only.
    pub log_file: Option<PathBuf>,
    /// `[Tuner] LogLevel` (`error`, `warn`, `info`, `debug`, `trace`; absent or empty =
    /// `info`); `Err` holds a value that is none of them.
    pub log_level: Result<LevelFilter, String>,
    /// `[Tuner] StateFile`: absent or empty = `<dll>.state`, else that file. Always
    /// `Some` from an ini (it cannot be turned off); `None` only for a driver that must
    /// not touch the disk (tests).
    pub state_file: Option<PathBuf>,
    /// UHF, CATV (unless hidden), then BS and CS110 when they have lines.
    pub spaces: Vec<Space>,
}

impl Config {
    /// Read the configuration from a parsed ini (an empty one gives the defaults).
    pub fn from_ini(ini: &Ini, paths: &ModulePaths) -> Self {
        let firmware = paths.resolve(tuner_string(ini, "Firmware").unwrap_or(DEFAULT_FIRMWARE));

        // System1 (EP 0x81) transcoder firmware: the vendor viewing path, and the only
        // one the Rust port has (empty = streams cannot start).
        let trc = tuner_string(ini, "TrcFirmware").unwrap_or(DEFAULT_TRC_FIRMWARE);
        let trc_firmware = (!trc.is_empty()).then(|| paths.resolve(trc));

        let index = tuner_int(ini, "DeviceIndex", 0) as i32; // truncated to 32 bits
        let serial = tuner_string(ini, "DeviceSerial").unwrap_or("").trim();
        let device = if serial.is_empty() {
            DeviceSel::Index(usize::try_from(index).unwrap_or(usize::MAX))
        } else {
            DeviceSel::Serial(serial.to_ascii_uppercase())
        };

        // LogFile=1: the core log to <dll>.log, a fixed name -- one per DLL copy, and TVTest
        // needs a copy of the DLL and ini per unit. Absent or 0 = no file.
        let log_file = (tuner_int(ini, "LogFile", 0) != 0).then(|| paths.sibling(".log"));
        let log_level = match tuner_string(ini, "LogLevel").unwrap_or("").trim() {
            "" => Ok(LevelFilter::INFO),
            v => v.parse::<LevelFilter>().map_err(|_| v.to_owned()),
        };

        // The state file cannot be turned off: without it a re-open loses the link key
        // (no picture until a re-plug), every tune rediscovers its service, and
        // uncontracted pay channels drop out of scans. Absent or empty = <dll>.state; a
        // value = that file (relative to the DLL folder), e.g. one shared by two DLLs that
        // open the same unit.
        let state_file = Some(match tuner_string(ini, "StateFile").unwrap_or("") {
            "" => paths.sibling(".state"),
            p => paths.resolve(p),
        });

        let mut spaces = Vec::new();
        if tuner_int(ini, "UHF", 1) != 0 {
            spaces.push(uhf_space());
        }
        if tuner_int(ini, "CATV", 1) != 0 {
            spaces.push(catv_space());
        }
        spaces.extend(satellite_space(ini, "BS"));
        spaces.extend(satellite_space(ini, "CS110"));

        Self {
            firmware,
            trc_firmware,
            device,
            log_file,
            log_level,
            state_file,
            spaces,
        }
    }
}

/// Which device a DLL opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSel {
    /// `[Tuner] DeviceIndex`: the n-th PIX-DT295 this process can open (one in use by
    /// another host is skipped, so with several units the numbers shift).
    Index(usize),
    /// `[Tuner] DeviceSerial`: the unit with this USB serial number (upper case; the
    /// device is matched case-insensitively). Fixed to one unit whatever the order.
    Serial(String),
}

impl std::fmt::Display for DeviceSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceSel::Index(n) => write!(f, "DeviceIndex {n}"),
            DeviceSel::Serial(s) => write!(f, "serial {s}"),
        }
    }
}

/// A `[BS]` / `[CS110]` section as a space; `None` without valid lines.
fn satellite_space(ini: &Ini, section: &str) -> Option<Space> {
    let props = ini.section(Some(section))?;
    let channels: Vec<_> = props
        .iter()
        .filter_map(|(k, v)| satellite_channel(&restore_key(k), v))
        .collect();
    (!channels.is_empty()).then(|| Space {
        name: section.to_owned(),
        channels,
    })
}

/// `GetPrivateProfileStringW(L"Tuner", key, ...)`: the first value, with one pair of
/// enclosing quotes removed. `None` if the key is absent (an empty value is `Some("")`).
fn tuner_string<'a>(ini: &'a Ini, key: &str) -> Option<&'a str> {
    ini.get_from(Some("Tuner"), key).map(unquote)
}

/// `GetPrivateProfileIntW(L"Tuner", key, default)`.
fn tuner_int(ini: &Ini, key: &str, default: u32) -> u32 {
    match tuner_string(ini, key) {
        None | Some("") => default,
        Some(s) => profile_int(s),
    }
}

fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// The integer syntax of `GetPrivateProfileIntW` (`RtlUnicodeStringToInteger`, base
/// 0): leading blanks, a sign, `0x` / `0o` / `0b` prefixes, digits up to the first
/// other character (none = 0), modulo 2^32.
pub fn profile_int(s: &str) -> u32 {
    let t = s.trim_start_matches(|c: char| c <= ' ');
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let (radix, digits) = match t.as_bytes() {
        [b'0', b'x' | b'X', ..] => (16, &t[2..]),
        [b'0', b'o' | b'O', ..] => (8, &t[2..]),
        [b'0', b'b' | b'B', ..] => (2, &t[2..]),
        _ => (10, t),
    };
    let v = digits
        .chars()
        .map_while(|c| c.to_digit(radix))
        .fold(0u32, |acc, d| acc.wrapping_mul(radix).wrapping_add(d));
    if neg { v.wrapping_neg() } else { v }
}

/// The ini file's text: UTF-8 (with or without BOM), UTF-16LE with BOM, or else the
/// ANSI code page (what `GetPrivateProfileStringW` assumes for a BOM-less file).
pub fn decode_ini(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => crate::sys::decode_ansi(bytes),
    }
}

/// Stand-ins for characters `rust-ini` would treat specially inside a key (`:` is
/// also a key/value separator there; `\` before the separator would swallow it).
const KEY_STANDINS: [(char, char); 2] = [(':', '\u{E03A}'), ('\\', '\u{E05C}')];

fn restore_key(k: &str) -> String {
    KEY_STANDINS
        .iter()
        .fold(k.to_owned(), |s, &(orig, standin)| {
            s.replace(standin, &orig.to_string())
        })
}

/// Parse ini text with `rust-ini` (escapes and quotes off) after normalising it to
/// the lines the Win32 profile API would see: blank, `;` / `#` comment and
/// separator-less lines dropped, `[section]` headers cut after the `]`, keys
/// protected (see `KEY_STANDINS`), and no backslash line continuation. A
/// leading UTF-8 BOM is stripped.
pub fn parse_ini(text: &str) -> Result<Ini, ini::ParseError> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut norm = String::with_capacity(text.len());
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if line.starts_with('[') {
            match line.find(']') {
                Some(end) => norm.push_str(&line[..=end]),
                None => continue, // not a section header
            }
        } else {
            let Some(eq) = line.find('=').filter(|&eq| eq > 0) else {
                continue; // no key=value
            };
            let key = KEY_STANDINS
                .iter()
                .fold(line[..eq].to_owned(), |s, &(orig, standin)| {
                    s.replace(orig, &standin.to_string())
                });
            norm.push_str(&key);
            norm.push_str(&line[eq..]);
            if line.ends_with('\\') {
                norm.push(' '); // a trailing '\' would continue the value on the next line
            }
        }
        norm.push('\n');
    }
    Ini::load_from_str_opt(
        &norm,
        ParseOption {
            enabled_quote: false,
            enabled_escape: false,
            enabled_indented_mutiline_value: false,
            enabled_preserve_key_leading_whitespace: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dt295::System;

    fn paths() -> ModulePaths {
        ModulePaths::from_module_path(OsStr::new(r"C:\TVTest\BonDriver\BonDriver_DT295.dll"))
    }

    fn config(text: &str) -> Config {
        Config::from_ini(&parse_ini(text).expect("parses"), &paths())
    }

    #[test]
    fn log_level() {
        let level = |text: &str| config(text).log_level;
        assert_eq!(level(""), Ok(LevelFilter::INFO), "no key");
        assert_eq!(level("[Tuner]\r\nLogLevel=\r\n"), Ok(LevelFilter::INFO));
        assert_eq!(
            level("[Tuner]\r\nLogLevel=debug\r\n"),
            Ok(LevelFilter::DEBUG)
        );
        assert_eq!(
            level("[Tuner]\r\nLogLevel= Trace \r\n"),
            Ok(LevelFilter::TRACE)
        );
        assert_eq!(level("[Tuner]\r\nLogLevel=warn\r\n"), Ok(LevelFilter::WARN));
        assert_eq!(level("[Tuner]\r\nLogLevel=loud\r\n"), Err("loud".into()));
    }

    #[test]
    fn module_paths() {
        let p = paths();
        assert_eq!(p.dir, OsString::from(r"C:\TVTest\BonDriver\"));
        assert_eq!(
            p.stem,
            OsString::from(r"C:\TVTest\BonDriver\BonDriver_DT295")
        );
        assert_eq!(
            p.sibling(".ini"),
            PathBuf::from(r"C:\TVTest\BonDriver\BonDriver_DT295.ini")
        );
        assert_eq!(p.resolve(r"D:\fw\a.bin"), PathBuf::from(r"D:\fw\a.bin"));
        assert_eq!(
            p.resolve(r"\\nas\fw\a.bin"),
            PathBuf::from(r"\\nas\fw\a.bin")
        );
        assert_eq!(
            p.resolve(r"fw\a.bin"),
            PathBuf::from(r"C:\TVTest\BonDriver\fw\a.bin")
        );
        let none = ModulePaths::from_module_path(OsStr::new(""));
        assert_eq!(none.sibling(".ini"), PathBuf::from(".ini"));
    }

    #[test]
    fn defaults_without_ini() {
        let c = config("");
        assert_eq!(
            c.firmware,
            PathBuf::from(r"C:\TVTest\BonDriver\mb86m01_assp_osec_idle.bin")
        );
        assert_eq!(
            c.trc_firmware,
            Some(PathBuf::from(
                r"C:\TVTest\BonDriver\mb86m01_assp_osec_trc_m_h.bin"
            ))
        );
        assert_eq!(c.device, DeviceSel::Index(0));
        assert_eq!(c.log_file, None, "no LogFile key = no log file");
        assert_eq!(
            c.state_file,
            Some(PathBuf::from(r"C:\TVTest\BonDriver\BonDriver_DT295.state")),
            "the state file is always on"
        );
        let names: Vec<_> = c.spaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["UHF", "CATV"]);
    }

    #[test]
    fn tuner_section() {
        let c = config(
            "\u{FEFF}; comment\r\n\
             [tuner]\r\n\
             firmware = D:\\fw\\idle.bin\r\n\
             TrcFirmware=\r\n\
             DeviceIndex=2\r\n\
             LogFile=logs\\dt295core.log\r\n\
             StateFile=\"C:\\state\\dt295.state\"\r\n\
             UHF=0\r\n\
             CATV=yes\r\n",
        );
        assert_eq!(c.firmware, PathBuf::from(r"D:\fw\idle.bin"));
        assert_eq!(c.trc_firmware, None);
        assert_eq!(c.device, DeviceSel::Index(2));
        // A path is not a flag: LogFile reads as 0 (as GetPrivateProfileInt would).
        assert_eq!(c.log_file, None);
        assert_eq!(c.state_file, Some(PathBuf::from(r"C:\state\dt295.state")));
        // "yes" is not a number: GetPrivateProfileInt reads 0.
        assert!(c.spaces.is_empty());
    }

    #[test]
    fn log_flag_and_state_file() {
        let copy =
            ModulePaths::from_module_path(OsStr::new(r"C:\TVTest\BonDriver\BonDriver_DT295-1.dll"));
        let files = |text: &str| {
            let c = Config::from_ini(&parse_ini(text).expect("parses"), &copy);
            (c.log_file, c.state_file)
        };
        let log = || Some(PathBuf::from(r"C:\TVTest\BonDriver\BonDriver_DT295-1.log"));
        let state = || {
            Some(PathBuf::from(
                r"C:\TVTest\BonDriver\BonDriver_DT295-1.state",
            ))
        };
        // LogFile: 1 = <dll>.log (named after the DLL copy); absent, empty or 0 = off.
        assert_eq!(
            files(
                "[Tuner]
LogFile=1
"
            ),
            (log(), state())
        );
        for off in [
            "",
            "[Tuner]
LogFile=
",
            "[Tuner]
LogFile=0
",
            "[Tuner]
;LogFile=1
",
        ] {
            assert_eq!(files(off).0, None, "{off:?}");
        }
        // StateFile: absent or empty = <dll>.state; a value = that file. Never off.
        assert_eq!(
            files(
                "[Tuner]
StateFile=
"
            )
            .1,
            state()
        );
        assert_eq!(
            files(
                "[Tuner]
;StateFile=
"
            )
            .1,
            state()
        );
        assert_eq!(
            files(
                "[Tuner]
StateFile=BonDriver_DT295.state
"
            )
            .1,
            Some(PathBuf::from(r"C:\TVTest\BonDriver\BonDriver_DT295.state")),
            "shared with another DLL of the same unit"
        );
        assert_eq!(
            files(
                "[Tuner]
StateFile=-
"
            )
            .1,
            Some(PathBuf::from(r"C:\TVTest\BonDriver\-")),
            "no switch-off value: '-' is just a file name"
        );
        // Only the [Tuner] section counts; the device selection does not name anything.
        assert_eq!(
            files(
                "[BS]
LogFile=1
"
            )
            .0,
            None
        );
        assert_eq!(
            files(
                "[Tuner]
DeviceSerial=CD23456789
"
            )
            .1,
            state()
        );
    }

    #[test]
    fn device_selection() {
        // A serial selects the unit, whatever DeviceIndex says.
        let c = config(
            "[Tuner]
DeviceIndex=1
DeviceSerial= cd23456789 
",
        );
        assert_eq!(c.device, DeviceSel::Serial("CD23456789".into()));
        assert_eq!(c.device.to_string(), "serial CD23456789");
        assert_eq!(
            config(
                "[Tuner]
DeviceSerial=
DeviceIndex=3
"
            )
            .device,
            DeviceSel::Index(3),
            "empty = by index"
        );
        assert_eq!(DeviceSel::Index(3).to_string(), "DeviceIndex 3");
        // An empty Firmware= is not the default (Win32: present but empty).
        assert_eq!(
            config(
                "[Tuner]
Firmware=
"
            )
            .firmware,
            PathBuf::from(r"C:\TVTest\BonDriver\")
        );
    }

    #[test]
    fn satellite_sections() {
        let c = config(
            "[Tuner]\nUHF=0\nCATV=0\n\
             [BS]\n\
             ; BS-1 (11727.48 MHz): comment without '='\n\
             BS01/TS0=1049480,0x4010,151\n\
             BS01/TS1 = 1049480 , 0x4011\n\
             a stray line\n\
             =1049480,0x4012\n\
             J:COM BS=1471440,0x4973,260\n\
             back\\slash=1471440,0x4973\n\
             Out of range=100000,0x4010\n\
             [CS110]\n\
             ND02/296=1613000,0x6020,296 ; not a comment for Win32 either\n\
             [Empty]\n",
        );
        assert_eq!(c.spaces.len(), 2);
        let bs = &c.spaces[0];
        assert_eq!(bs.name, "BS");
        let names: Vec<_> = bs.channels.iter().map(|ch| ch.name.as_str()).collect();
        assert_eq!(names, ["BS01/TS0", "BS01/TS1", "J:COM BS", "back\\slash"]);
        assert_eq!(bs.channels[0].sid, Some(151));
        // wcstoul stops at the blank before ',': TSID stays "none".
        assert_eq!(bs.channels[1].tsid, 0xFFFF);
        assert_eq!(bs.channels[2].tsid, 0x4973);
        assert_eq!(bs.channels[2].sys, System::Satellite);
        let cs = &c.spaces[1];
        assert_eq!(cs.name, "CS110");
        assert_eq!(cs.channels.len(), 1);
        assert_eq!(cs.channels[0].sid, Some(296));
    }

    #[test]
    fn sample_ini_from_repo() {
        let text = include_str!("../BonDriver_DT295.ini");
        let c = config(text);
        let summary: Vec<_> = c
            .spaces
            .iter()
            .map(|s| (s.name.as_str(), s.channels.len()))
            .collect();
        assert_eq!(
            summary,
            [("UHF", 50), ("CATV", 51), ("BS", 26), ("CS110", 12)]
        );
        // one line per TS without a ServiceID: every service of it
        assert!(
            c.spaces[2..]
                .iter()
                .all(|s| s.channels.iter().all(|ch| ch.sid.is_none()))
        );
        let line = |space: usize, name: &str| {
            let ch = c.spaces[space]
                .channels
                .iter()
                .find(|ch| ch.name == name)
                .unwrap();
            (ch.freq_hz, ch.tsid)
        };
        assert_eq!(line(2, "BS15/TS1"), (1_318_000_000, 0x40F1));
        assert_eq!(line(2, "BS13/TS2"), (1_279_640_000, 0x46D2));
        assert_eq!(line(2, "BS01/TS1"), (1_049_480_000, 0x4011));
        assert_eq!(line(3, "ND06/TS0"), (1_693_000_000, 0x7060));
    }

    #[test]
    fn profile_int_syntax() {
        assert_eq!(profile_int("1"), 1);
        assert_eq!(profile_int("  42abc"), 42);
        assert_eq!(profile_int("0x10"), 16);
        assert_eq!(profile_int("010"), 10);
        assert_eq!(profile_int("-1"), u32::MAX);
        assert_eq!(profile_int("yes"), 0);
        assert_eq!(profile_int("+7"), 7);
    }

    #[test]
    fn decoding() {
        assert_eq!(decode_ini(b"\xEF\xBB\xBF[Tuner]"), "[Tuner]");
        assert_eq!(decode_ini(b"\xFF\xFE[\0T\0]\0"), "[T]");
        assert_eq!(decode_ini("[BS]\nあ=1".as_bytes()), "[BS]\nあ=1");
        // Not UTF-8: decoded with the ANSI code page (never fails).
        assert!(decode_ini(b"[BS]\n\x82\xa0=1").starts_with("[BS]\n"));
    }

    #[test]
    fn unquoting() {
        assert_eq!(unquote("\"a b\""), "a b");
        assert_eq!(unquote("'a'"), "a");
        assert_eq!(unquote("\"a'"), "\"a'");
        assert_eq!(unquote("\""), "\"");
    }
}
