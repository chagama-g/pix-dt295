//! `BonDriver_DT295.ini` (next to the DLL, same base name), read under the Win32
//! profile API's rules ([`dt295::profile`]).
//!
//! `[Tuner]`: `Firmware`, `TrcFirmware`, `DeviceIndex`, `DeviceSerial`, `StateFile`,
//! `RawTS`, `ChannelFile` ([`dt295::bonini::Tuner`]), `LogFile`, `LogLevel`, `UHF`,
//! `CATV`. `[BS]` / `[CS110]`: one `name=IF_kHz,TSID[,ServiceID]` line per channel. The
//! `[Tuner]` keys of the unit and the tuning spaces ([`dt295::chan::ini_spaces`]: the
//! ini's own, or those of the `ChannelFile`) are read by the shared code the `dt295`
//! command reads the same file with.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use dt295::bonini::{DeviceSel, Tuner};
use dt295::chan::{self, Space};
use dt295::profile::{self, Ini};
use tracing::level_filters::LevelFilter;

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
        cfg_select! {
            windows => {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let w: Vec<u16> = path.encode_wide().collect();
                let dir_end = w
                    .iter()
                    .rposition(|&c| c == u16::from(b'\\') || c == u16::from(b'/'))
                    .map_or_default(|i| i + 1);
                let stem_end = w
                    .iter()
                    .rposition(|&c| c == u16::from(b'.'))
                    .unwrap_or(w.len());
                Self {
                    dir: OsString::from_wide(&w[..dir_end]),
                    stem: OsString::from_wide(&w[..stem_end]),
                }
            }
            _ => {
                let s = path.to_string_lossy();
                let dir_end = s.rfind(['\\', '/']).map_or_default(|i| i + 1);
                let stem_end = s.rfind('.').unwrap_or(s.len());
                Self {
                    dir: OsString::from(&s[..dir_end]),
                    stem: OsString::from(&s[..stem_end]),
                }
            }
        }
    }

    /// `BonDriver_DT295<ext>` next to the DLL.
    pub fn sibling(&self, ext: &str) -> PathBuf {
        let mut s = self.stem.clone();
        s.push(ext);
        PathBuf::from(s)
    }
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
    /// `<dll>.ch2`, TVTest's channel-scan file (next to the DLL, independent of `StateFile`):
    /// read in raw-TS mode to prime on a known on-air channel at the very first open. Always
    /// `Some` from an ini; `None` only for a driver that must not touch the disk (tests).
    pub ch2_file: Option<PathBuf>,
    /// UHF, CATV (unless hidden), then BS and CS110 when they have lines; or the spaces of
    /// the `[Tuner] ChannelFile` (none when it cannot be read).
    pub spaces: Vec<Space>,
    /// What to log about the `ChannelFile` once the log is open: `Ok` lines (its format
    /// and spaces), or `Err` why it could not be read.
    pub channel_file_log: Result<Vec<String>, String>,
    /// `[Tuner] RawTS`: non-zero = the blob-free raw-TS path -- the host brings the chip up,
    /// tunes over the host-face I2C, arms the capture engine and runs its own TS pump (no
    /// resident, no aux core, no vendor firmware), streaming the scrambled TS for the PC-side
    /// B-CAS (TVTest's CAS plugin) to descramble. Absent / 0 = the System1 viewing path
    /// (vendor firmware `Firmware` / `TrcFirmware`, the device's internal B-CAS reader).
    pub raw_ts: bool,
}

impl Config {
    /// Read the configuration from a parsed ini (an empty one gives the defaults).
    pub fn from_ini(ini: &Ini, paths: &ModulePaths) -> Self {
        // The firmware, the unit, the state file and RawTS: the dt295 command reads them
        // from the same file with the same code. The state file cannot be turned off:
        // without it a re-open loses the link key (no picture until a re-plug), every tune
        // rediscovers its service, and uncontracted pay channels drop out of scans.
        let tuner = Tuner::from_ini(ini, Path::new(&paths.dir), Path::new(&paths.stem));

        // LogFile=1: the core log to <dll>.log, a fixed name -- one per DLL copy, and TVTest
        // needs a copy of the DLL and ini per unit. Absent or 0 = no file.
        let log_file = (tuner_int(ini, "LogFile", 0) != 0).then(|| paths.sibling(".log"));
        let log_level = match tuner_string(ini, "LogLevel").unwrap_or("").trim() {
            "" => Ok(LevelFilter::INFO),
            v => v.parse::<LevelFilter>().map_err(|_| v.to_owned()),
        };

        // TVTest writes <dll>.ch2 next to the DLL, not next to a relocated StateFile, so derive
        // it from the module path (the same sibling the log and the default state use).
        let ch2_file = Some(paths.sibling(".ch2"));

        // A ChannelFile that cannot be read lists no spaces: the ini's own would number
        // the channels unlike the .ch2 made with that file.
        let (spaces, channel_file_log) = match chan::ini_spaces(ini, tuner.channel_file.as_deref())
        {
            Ok((spaces, log)) => (spaces, Ok(log)),
            Err(e) => (Vec::new(), Err(e)),
        };

        Self {
            firmware: tuner.firmware,
            trc_firmware: tuner.trc_firmware,
            device: tuner.device,
            log_file,
            log_level,
            state_file: Some(tuner.state_file),
            ch2_file,
            spaces,
            channel_file_log,
            raw_ts: tuner.raw_ts,
        }
    }
}

/// `GetPrivateProfileStringW(L"Tuner", key, ...)` ([`profile::get`]).
fn tuner_string<'a>(ini: &'a Ini, key: &str) -> Option<&'a str> {
    profile::get(ini, "Tuner", key)
}

/// `GetPrivateProfileIntW(L"Tuner", key, default)` ([`profile::get_int`]).
fn tuner_int(ini: &Ini, key: &str, default: u32) -> u32 {
    profile::get_int(ini, "Tuner", key, default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> ModulePaths {
        ModulePaths::from_module_path(OsStr::new(r"C:\TVTest\BonDriver\BonDriver_DT295.dll"))
    }

    fn config(text: &str) -> Config {
        Config::from_ini(&profile::parse(text).expect("parses"), &paths())
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
        // Relative paths are under the DLL folder.
        let fw = |v: &str| config(&format!("[Tuner]\nFirmware={v}\n")).firmware;
        assert_eq!(fw(r"D:\fw\a.bin"), PathBuf::from(r"D:\fw\a.bin"));
        assert_eq!(fw(r"\\nas\fw\a.bin"), PathBuf::from(r"\\nas\fw\a.bin"));
        assert_eq!(
            fw(r"fw\a.bin"),
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
            let c = Config::from_ini(&profile::parse(text).expect("parses"), &copy);
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
    fn channel_file() {
        let c = config("");
        assert_eq!(
            c.channel_file_log,
            Ok(Vec::new()),
            "none: the ini's own spaces"
        );
        // One that cannot be read: no spaces (the ini's own would number them otherwise)
        let c =
            config("[Tuner]\nChannelFile=Z:\\absent\\BonDriver_PT3-S.ChSet.txt\n[BS]\nA=1049480\n");
        assert!(c.spaces.is_empty());
        let e = c.channel_file_log.unwrap_err();
        assert!(
            e.starts_with(r"ChannelFile Z:\absent\BonDriver_PT3-S.ChSet.txt: "),
            "{e}"
        );
    }

    #[test]
    fn raw_ts_mode() {
        // Absent / 0 = the System1 path; RawTS=1 = the blob-free raw-TS path.
        assert!(!config("").raw_ts);
        assert!(!config("[Tuner]\nRawTS=0\n").raw_ts);
        assert!(config("[Tuner]\nRawTS=1\n").raw_ts);
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
}
