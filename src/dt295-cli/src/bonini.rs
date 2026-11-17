//! A BonDriver_DT295 ini as the configuration of `dt295 capture` and `dt295 logo`,
//! read the way the BonDriver reads it. `[Tuner]` gives the firmware, the unit
//! (`DeviceSerial`, else `DeviceIndex`) and the state file: paths are relative to the
//! ini's folder, and the state file is named after the ini unless `StateFile` says
//! otherwise. The channels are the built-in UHF (`13ch`..`62ch`) and CATV
//! (`C13ch`..`C63ch`) ones plus the `[BS]` / `[CS110]` lines. On Linux, where there is
//! no DLL, a file of the same format is the configuration.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dt295::chan::{self, SatLine};

use crate::tuner::DeviceArgs;

const DEFAULT_FW: &str = "mb86m01_assp_osec_idle.bin";
const DEFAULT_TRC: &str = "mb86m01_assp_osec_trc_m_h.bin";

/// What a BonDriver_DT295 ini says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BonDriver {
    pub ini: PathBuf,
    pub serial: Option<String>,
    pub index: Option<usize>,
    pub fw: PathBuf,
    pub trc: PathBuf,
    /// The state file (with the link key the unit last used).
    pub state: PathBuf,
    /// The `[BS]` and `[CS110]` lines (name, value), in file order.
    pub satellite: Vec<(String, String)>,
}

/// What a channel name stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tune {
    /// ISDB-T at this frequency (Hz); the first TV service.
    Terrestrial(u32),
    Satellite(SatLine),
}

impl BonDriver {
    pub fn from_ini(ini: &Path, text: &str) -> Self {
        let folder = ini.parent().unwrap_or(Path::new("."));
        let tuner = ini_section(text, "Tuner");
        let value = |k: &str| tuner.get(k).map(String::as_str).filter(|v| !v.is_empty());
        let mut satellite = ini_lines(text, "BS");
        satellite.extend(ini_lines(text, "CS110"));
        Self {
            ini: ini.to_owned(),
            serial: value("deviceserial").map(str::to_owned),
            index: value("deviceindex").and_then(|v| usize::try_from(chan::strtol(v).0).ok()),
            fw: folder.join(value("firmware").unwrap_or(DEFAULT_FW)),
            trc: folder.join(value("trcfirmware").unwrap_or(DEFAULT_TRC)),
            state: value("statefile")
                .map_or_else(|| ini.with_extension("state"), |s| folder.join(s)),
            satellite,
        }
    }

    pub fn load(ini: &Path) -> Result<Self> {
        let bytes = fs::read(ini).with_context(|| format!("cannot read {}", ini.display()))?;
        Ok(Self::from_ini(ini, &decode_ini(&bytes)))
    }

    /// The `BonDriver_DT295*.ini` files in a folder, by name.
    pub fn find(dir: &Path) -> Result<Vec<Self>> {
        let entries =
            fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
        let mut out = Vec::new();
        for e in entries {
            let path = e?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if name.starts_with("bondriver_dt295") && name.ends_with(".ini") {
                out.push(Self::load(&path)?);
            }
        }
        out.sort_by(|a, b| a.ini.cmp(&b.ini));
        Ok(out)
    }

    /// The device options this ini gives, with the ones given on the command line
    /// winning (an explicit `-i` also wins over the ini's `DeviceSerial`).
    pub fn device_args(&self, given: &DeviceArgs) -> DeviceArgs {
        let explicit_index = given.index != 0;
        DeviceArgs {
            fw: given.fw.clone().or_else(|| Some(self.fw.clone())),
            trc: given.trc.clone().or_else(|| Some(self.trc.clone())),
            index: if explicit_index {
                given.index
            } else {
                self.index.unwrap_or(0)
            },
            serial: given
                .serial
                .clone()
                .or_else(|| (!explicit_index).then(|| self.serial.clone()).flatten()),
            state: given.state.clone().or_else(|| Some(self.state.clone())),
        }
    }
}

/// What a channel name stands for: one of the `[BS]` / `[CS110]` lines (names compare
/// case-insensitively), else a built-in `NNch` (UHF) or `CNNch` (CATV) name.
pub fn tune(name: &str, satellite: &[(String, String)]) -> Option<Tune> {
    if let Some((_, v)) = satellite.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
        return chan::satellite_line(v).map(Tune::Satellite);
    }
    let name = name.trim().to_ascii_lowercase();
    let number = name.strip_suffix("ch")?;
    let hz = match number.strip_prefix('c') {
        Some(n) => chan::catv_hz(n.parse().ok()?),
        None => chan::uhf_hz(number.parse().ok()?),
    };
    hz.map(Tune::Terrestrial)
}

/// An ini file's text as the BonDriver reads it: UTF-16 LE or UTF-8 with a BOM,
/// UTF-8, else the ANSI code page.
pub fn decode_ini(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(b"\xFF\xFE") {
        return String::from_utf16le_lossy(rest);
    }
    let b = b.trim_prefix(b"\xEF\xBB\xBF");
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => decode_ansi(b),
    }
}

#[cfg(windows)]
fn decode_ansi(b: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
    let Ok(len) = i32::try_from(b.len()) else {
        return String::from_utf8_lossy(b).into_owned();
    };
    if len == 0 {
        return String::new();
    }
    // SAFETY: `len` readable bytes; a null output of size 0 only asks for the length.
    let need = unsafe { MultiByteToWideChar(CP_ACP, 0, b.as_ptr(), len, std::ptr::null_mut(), 0) };
    let mut w = vec![0u16; usize::try_from(need).unwrap_or(0)];
    // SAFETY: as above, and `w` is writable for `need` UTF-16 units.
    let got = unsafe { MultiByteToWideChar(CP_ACP, 0, b.as_ptr(), len, w.as_mut_ptr(), need) };
    match usize::try_from(got) {
        Ok(n) if n > 0 => String::from_utf16_lossy(&w[..n]),
        _ => String::from_utf8_lossy(b).into_owned(),
    }
}

#[cfg(not(windows))]
fn decode_ansi(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The name of a `[name]` section header; what follows the `]` is ignored, as the
/// BonDriver (and the Win32 profile API) does.
fn header(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('[')?;
    Some(rest[..rest.find(']')?].trim())
}

/// The `key=value` lines of one section (keys and values trimmed), in file order;
/// `;` / `#` comments and lines without `=` are skipped.
pub fn ini_lines(text: &str, section: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if let Some(name) = header(line) {
            inside = name.eq_ignore_ascii_case(section);
            continue;
        }
        let line = line.trim();
        if !inside || line.starts_with([';', '#']) {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    out
}

/// The keys of one section, lower-cased, values without surrounding quotes; the first
/// of repeated keys wins (as for the Win32 profile API).
pub fn ini_section(text: &str, section: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in ini_lines(text, section) {
        let v = v.strip_circumfix('"', '"').unwrap_or(&v).to_owned();
        out.entry(k.to_ascii_lowercase()).or_insert(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "[Tuner]\r\n\
        DeviceIndex=2\r\n\
        TrcFirmware=\"fw\\trc.bin\"\r\n\
        ; StateFile=x\r\n\
        [BS]  ; satellite\r\n\
        BS15/101=1318000,0x40F1,101\r\n\
        BS01/151=1049480,0x4010\r\n\
        Bad=900000,0x4010\r\n\
        [CS110]\r\n\
        ND02/296=1613000,0x6020,296\r\n\
        [UHF]\r\n\
        27ch=nothing\r\n";

    fn ini() -> BonDriver {
        BonDriver::from_ini(&Path::new("cfg").join("dt295.ini"), INI)
    }

    #[test]
    fn tuner_section() {
        let b = ini();
        let dir = Path::new("cfg");
        assert_eq!((b.serial.as_deref(), b.index), (None, Some(2)));
        assert_eq!(b.fw, dir.join("mb86m01_assp_osec_idle.bin"));
        assert_eq!(b.trc, dir.join("fw\\trc.bin"));
        assert_eq!(b.state, dir.join("dt295.state"));
        assert_eq!(b.satellite.len(), 4);
    }

    #[test]
    fn channel_names() {
        let b = ini();
        let sat = |hz, tsid, sid| {
            Some(Tune::Satellite(SatLine {
                freq_hz: hz,
                tsid,
                sid,
            }))
        };
        assert_eq!(
            tune("BS15/101", &b.satellite),
            sat(1_318_000_000, 0x40F1, Some(101))
        );
        assert_eq!(
            tune("bs01/151", &b.satellite),
            sat(1_049_480_000, 0x4010, None)
        );
        assert_eq!(
            tune("ND02/296", &b.satellite),
            sat(1_613_000_000, 0x6020, Some(296))
        );
        assert_eq!(tune("Bad", &b.satellite), None, "IF out of range");
        assert_eq!(
            tune("27ch", &b.satellite),
            Some(Tune::Terrestrial(557_142_857))
        );
        assert_eq!(
            tune("C13CH", &b.satellite),
            Some(Tune::Terrestrial(111_142_857))
        );
        assert_eq!(tune("12ch", &b.satellite), None);
        assert_eq!(tune("BS99/1", &b.satellite), None);
        assert_eq!(tune("62ch", &[]), Some(Tune::Terrestrial(767_142_857)));
    }

    #[test]
    fn options_given_win() {
        let b = BonDriver::from_ini(
            Path::new("dt295.ini"),
            "[Tuner]\nDeviceSerial=EF34567890\nStateFile=s.state\n",
        );
        let none = DeviceArgs {
            fw: None,
            trc: None,
            index: 0,
            serial: None,
            state: None,
        };
        let d = b.device_args(&none);
        assert_eq!(d.serial.as_deref(), Some("EF34567890"));
        assert_eq!(d.state, Some(PathBuf::from("s.state")));
        assert_eq!(d.fw, Some(PathBuf::from("mb86m01_assp_osec_idle.bin")));

        let given = DeviceArgs {
            fw: Some("f".into()),
            serial: Some("X".into()),
            ..none.clone()
        };
        let d = b.device_args(&given);
        assert_eq!((d.fw, d.serial.as_deref()), (Some("f".into()), Some("X")));
        // -i given: the ini's serial no longer applies
        let d = b.device_args(&DeviceArgs { index: 1, ..none });
        assert_eq!((d.index, d.serial), (1, None));
    }

    #[test]
    fn sections_and_text() {
        let t = "[A]\nk=1\nK=2\n# c\n;x=1\nnoeq\n[ b ] tail\nv = \"q\" \n";
        assert_eq!(ini_section(t, "a").get("k").map(String::as_str), Some("1"));
        assert_eq!(ini_section(t, "B").get("v").map(String::as_str), Some("q"));
        assert_eq!(ini_lines(t, "A").len(), 2);
        assert_eq!(decode_ini(b"\xEF\xBB\xBF[T]"), "[T]");
        assert_eq!(decode_ini(b"\xFF\xFE[\0T\0]\0"), "[T]");
        // not UTF-8: the ANSI code page; never fails
        assert!(decode_ini(b"[T]\r\nA=\x82\xA0").starts_with("[T]\r\nA="));
    }
}
