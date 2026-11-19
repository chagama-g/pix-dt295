//! The `[Tuner]` section of a `BonDriver_DT295.ini` as the BonDriver reads it, shared
//! with the `dt295` command, which takes the same file as its configuration: the
//! firmware, the unit, the state file, the raw-TS switch and the channel file of another
//! BonDriver. Relative paths are under the ini's folder. The tuning spaces of the file are
//! [`chan::ini_spaces`](crate::chan::ini_spaces).

use std::ffi::OsString;
use std::fmt;
use std::path::{self, Path, PathBuf};

use crate::fwdl::{IDLE_FIRMWARE, TRC_FIRMWARE};
use crate::profile::{self, Ini};

/// Which device to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSel {
    /// `DeviceIndex`: the n-th PIX-DT295 this process can open (one in use by another
    /// host is skipped, so with several units the numbers shift).
    Index(usize),
    /// `DeviceSerial`: the unit with this USB serial number (upper case; the device is
    /// matched case-insensitively). Fixed to one unit whatever the order.
    Serial(String),
}

impl fmt::Display for DeviceSel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceSel::Index(n) => write!(f, "DeviceIndex {n}"),
            DeviceSel::Serial(s) => write!(f, "serial {s}"),
        }
    }
}

/// What `[Tuner]` says about the unit and its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuner {
    /// `Firmware`, the idle firmware: absent = the vendor's name. An empty value is the
    /// folder itself (Win32: present but empty), which does not boot.
    pub firmware: PathBuf,
    /// `TrcFirmware`, the System1 transcoder firmware: absent = the vendor's name; empty
    /// = `None`, no stream can start (there is no System2 path).
    pub trc_firmware: Option<PathBuf>,
    /// `DeviceSerial` (blanks trimmed, upper case), else `DeviceIndex`
    /// (`GetPrivateProfileInt`, absent = 0; a negative one opens nothing).
    pub device: DeviceSel,
    /// `StateFile`: absent or empty = the ini's name with `.state`, else that file.
    pub state_file: PathBuf,
    /// `RawTS`: non-zero = the blob-free raw-TS path (absent = the System1 path).
    pub raw_ts: bool,
    /// `ChannelFile`: the channel file of another BonDriver whose tuning spaces to list
    /// instead of the ini's own ([`dt295_chdef::load`]); absent or empty = none.
    pub channel_file: Option<PathBuf>,
}

impl Tuner {
    /// `[Tuner]` of `ini`, the file `stem` + `.ini` in the folder `dir` (the base of
    /// relative paths; empty = the working directory). The default state file is `stem`
    /// + `.state`.
    pub fn from_ini(ini: &Ini, dir: &Path, stem: &Path) -> Self {
        let string = |key: &str| profile::get(ini, "Tuner", key);
        let int = |key: &str, default| profile::get_int(ini, "Tuner", key, default);

        let firmware = resolve(dir, string("Firmware").unwrap_or(IDLE_FIRMWARE));
        let trc = string("TrcFirmware").unwrap_or(TRC_FIRMWARE);
        let trc_firmware = (!trc.is_empty()).then(|| resolve(dir, trc));

        let index = int("DeviceIndex", 0) as i32; // truncated to 32 bits
        let serial = string("DeviceSerial").unwrap_or("").trim();
        let device = if serial.is_empty() {
            DeviceSel::Index(usize::try_from(index).unwrap_or(usize::MAX))
        } else {
            DeviceSel::Serial(serial.to_ascii_uppercase())
        };

        // The state file cannot be turned off (no switch-off value).
        let state_file = match string("StateFile").unwrap_or("") {
            "" => {
                let mut s = OsString::from(stem);
                s.push(".state");
                PathBuf::from(s)
            }
            p => resolve(dir, p),
        };
        let raw_ts = int("RawTS", 0) != 0;
        let channel_file = match string("ChannelFile").unwrap_or("").trim() {
            "" => None,
            p => Some(resolve(dir, p)),
        };
        Self {
            firmware,
            trc_firmware,
            device,
            state_file,
            raw_ts,
            channel_file,
        }
    }
}

/// An ini path: absolute (`X:...`, `\\...`, or absolute on this OS) as is, else
/// appended to `dir` as text, so a `\x` stays under it as in the BonDriver. A `dir`
/// ending in `\` or `/` (an ini is a Windows file) gets no second separator.
pub fn resolve(dir: &Path, value: &str) -> PathBuf {
    let dir = dir.as_os_str();
    if dir.is_empty() || is_absolute(value) || Path::new(value).is_absolute() {
        return PathBuf::from(value);
    }
    let mut s = dir.to_owned();
    if !s.to_string_lossy().ends_with(['\\', '/']) {
        s.push(path::MAIN_SEPARATOR_STR);
    }
    s.push(value);
    PathBuf::from(s)
}

/// A drive letter path or a UNC path.
fn is_absolute(p: &str) -> bool {
    let mut c = p.chars();
    matches!(
        (c.next(), c.next()),
        (Some(_), Some(':')) | (Some('\\'), Some('\\'))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        Path::new("TVTest").join("BonDriver")
    }

    fn tuner(text: &str) -> Tuner {
        let stem = dir().join("BonDriver_DT295");
        Tuner::from_ini(&profile::parse(text).expect("parses"), &dir(), &stem)
    }

    #[test]
    fn defaults() {
        let t = tuner("");
        assert_eq!(t.firmware, dir().join("mb86m01_assp_osec_idle.bin"));
        assert_eq!(
            t.trc_firmware,
            Some(dir().join("mb86m01_assp_osec_trc_m_h.bin"))
        );
        assert_eq!(t.device, DeviceSel::Index(0));
        assert_eq!(t.state_file, dir().join("BonDriver_DT295.state"));
        assert!(!t.raw_ts);
        assert_eq!(t.channel_file, None);
    }

    #[test]
    fn channel_file() {
        assert_eq!(
            tuner("[Tuner]\nChannelFile=BonDriver_PT3-S.ChSet.txt\n").channel_file,
            Some(dir().join("BonDriver_PT3-S.ChSet.txt"))
        );
        assert_eq!(
            tuner("[Tuner]\nChannelFile=\"D:\\BDA\\BonDriver_BDA.ini\"\n").channel_file,
            Some(PathBuf::from(r"D:\BDA\BonDriver_BDA.ini"))
        );
        assert_eq!(tuner("[Tuner]\nChannelFile= \n").channel_file, None);
    }

    #[test]
    fn values() {
        let t = tuner(
            "[tuner]\r\n\
             firmware = D:\\fw\\idle.bin\r\n\
             TrcFirmware=\r\n\
             DeviceIndex=010\r\n\
             StateFile=\"C:\\state\\dt295.state\"\r\n\
             RawTS=1\r\n",
        );
        assert_eq!(t.firmware, PathBuf::from(r"D:\fw\idle.bin"));
        assert_eq!(t.trc_firmware, None, "empty: no stream can start");
        assert_eq!(
            t.device,
            DeviceSel::Index(10),
            "GetPrivateProfileInt: decimal"
        );
        assert_eq!(t.state_file, PathBuf::from(r"C:\state\dt295.state"));
        assert!(t.raw_ts);
        // An empty Firmware= is not the default (Win32: present but empty).
        assert_eq!(tuner("[Tuner]\nFirmware=\n").firmware, dir());
        assert_eq!(
            tuner("[Tuner]\nStateFile=\n").state_file,
            tuner("").state_file,
            "empty = the default"
        );
        assert_eq!(
            tuner("[Tuner]\nStateFile=shared.state\n").state_file,
            dir().join("shared.state")
        );
    }

    #[test]
    fn device_selection() {
        let device = |text: &str| tuner(text).device;
        // A serial selects the unit, whatever DeviceIndex says.
        assert_eq!(
            device("[Tuner]\nDeviceIndex=1\nDeviceSerial= cd23456789 \n"),
            DeviceSel::Serial("CD23456789".into())
        );
        assert_eq!(
            device("[Tuner]\nDeviceSerial=\" cd23456789 \"\n"),
            DeviceSel::Serial("CD23456789".into()),
            "blanks inside quotes too"
        );
        assert_eq!(
            device("[Tuner]\nDeviceSerial=\" \"\nDeviceIndex=3\n"),
            DeviceSel::Index(3),
            "blank = by index"
        );
        assert_eq!(
            device("[Tuner]\nDeviceIndex=-1\n"),
            DeviceSel::Index(usize::MAX)
        );
        assert_eq!(device("[Tuner]\nDeviceIndex=0x2\n"), DeviceSel::Index(2));
        assert_eq!(DeviceSel::Index(3).to_string(), "DeviceIndex 3");
        assert_eq!(
            DeviceSel::Serial("CD23456789".into()).to_string(),
            "serial CD23456789"
        );
    }

    #[test]
    fn relative_paths() {
        let d = dir();
        assert_eq!(resolve(&d, r"D:\fw\a.bin"), PathBuf::from(r"D:\fw\a.bin"));
        assert_eq!(
            resolve(&d, r"\\nas\fw\a.bin"),
            PathBuf::from(r"\\nas\fw\a.bin")
        );
        assert_eq!(resolve(&d, "a.bin"), d.join("a.bin"), "a separator added");
        assert_eq!(resolve(Path::new(""), "a.bin"), PathBuf::from("a.bin"));
        let t = Tuner::from_ini(
            &profile::parse("[Tuner]\nFirmware=a.bin\n").unwrap(),
            Path::new(""),
            Path::new("dt295"),
        );
        assert_eq!(
            (t.firmware, t.state_file),
            (PathBuf::from("a.bin"), PathBuf::from("dt295.state")),
            "an ini in the working directory"
        );
    }

    #[test]
    fn windows_paths() {
        let d = Path::new(r"C:\TVTest\BonDriver\");
        assert_eq!(
            resolve(d, r"fw\a.bin"),
            PathBuf::from(r"C:\TVTest\BonDriver\fw\a.bin"),
            "no second separator"
        );
        assert_eq!(
            resolve(d, r"\fw\a.bin"),
            PathBuf::from(r"C:\TVTest\BonDriver\\fw\a.bin"),
            "a root-relative path stays under the folder"
        );
    }
}
