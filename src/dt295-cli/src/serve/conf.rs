//! `/etc/dt295.conf`, the configuration of `dt295 serve` (ini syntax):
//!
//! ```ini
//! [dt295]
//! ; the vendor's idle and trc images
//! firmware-dir = /usr/lib/firmware/dt295
//! ; <serial>.state per unit
//! state-dir = /var/lib/dt295
//! socket = /run/dt295/dt295.sock
//!
//! [unit AB12345678]
//! [unit CD23456789]
//! ```
//!
//! The `[dt295]` settings are optional (these are the defaults); there is one
//! `[unit <serial>]` per PIX-DT295 to serve. Unknown sections and settings are
//! errors, so a typo does not go unnoticed. Comments take a line of their own (`;`
//! or `#` first): after a value they would be part of it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ini::Ini;

/// Where `dt295 serve` reads its configuration by default.
pub const DEFAULT_CONF: &str = "/etc/dt295.conf";
/// Where `dt295 serve` listens and `dt295 tune` connects by default.
pub const DEFAULT_SOCKET: &str = "/run/dt295/dt295.sock";
const DEFAULT_FIRMWARE_DIR: &str = "/usr/lib/firmware/dt295";
const DEFAULT_STATE_DIR: &str = "/var/lib/dt295";
/// The idle and transcoder firmware images, under the vendor's names.
const IDLE_FIRMWARE: &str = "mb86m01_assp_osec_idle.bin";
const TRC_FIRMWARE: &str = "mb86m01_assp_osec_trc_m_h.bin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conf {
    pub firmware_dir: PathBuf,
    pub state_dir: PathBuf,
    pub socket: PathBuf,
    /// The USB serial numbers of the units to serve, in file order.
    pub units: Vec<String>,
}

impl Conf {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let ini = Ini::load_from_str(text)?;
        let mut conf = Self {
            firmware_dir: DEFAULT_FIRMWARE_DIR.into(),
            state_dir: DEFAULT_STATE_DIR.into(),
            socket: DEFAULT_SOCKET.into(),
            units: Vec::new(),
        };
        for (section, props) in &ini {
            let words: Vec<&str> = section.unwrap_or_default().split_whitespace().collect();
            match words.as_slice() {
                [] if props.is_empty() => {}
                [] => bail!("settings before the first section"),
                [name] if name.eq_ignore_ascii_case("dt295") => {
                    for (key, value) in props {
                        let path = PathBuf::from(value);
                        match key.to_ascii_lowercase().as_str() {
                            "firmware-dir" => conf.firmware_dir = path,
                            "state-dir" => conf.state_dir = path,
                            "socket" => conf.socket = path,
                            _ => bail!("[dt295]: unknown setting {key:?}"),
                        }
                    }
                }
                [kind, serial] if kind.eq_ignore_ascii_case("unit") => {
                    if let Some((key, _)) = props.iter().next() {
                        bail!("[unit {serial}]: unknown setting {key:?}");
                    }
                    if conf.units.iter().any(|u| u.eq_ignore_ascii_case(serial)) {
                        bail!("[unit {serial}] appears twice");
                    }
                    conf.units.push((*serial).to_owned());
                }
                _ => bail!(
                    "unknown section [{}] (expected [dt295] or [unit <serial>])",
                    section.unwrap_or_default()
                ),
            }
        }
        if conf.units.is_empty() {
            bail!("no [unit <serial>] section: nothing to serve");
        }
        Ok(conf)
    }

    pub fn idle_firmware(&self) -> PathBuf {
        self.firmware_dir.join(IDLE_FIRMWARE)
    }

    pub fn trc_firmware(&self) -> PathBuf {
        self.firmware_dir.join(TRC_FIRMWARE)
    }

    /// The state file of a unit (link key, PSI), named after its serial.
    pub fn state_file(&self, serial: &str) -> PathBuf {
        self.state_dir
            .join(format!("{}.state", serial.to_ascii_uppercase()))
    }
}

/// The socket a client (`dt295 tune`, a scan) connects to: `socket` if given, else
/// the one of the configuration (`config`, or /etc/dt295.conf if it exists), else
/// the default.
pub fn client_socket(config: Option<&Path>, socket: Option<&Path>) -> Result<PathBuf> {
    if let Some(s) = socket {
        return Ok(s.to_owned());
    }
    match config {
        Some(c) => Ok(Conf::load(c)?.socket),
        None if Path::new(DEFAULT_CONF).exists() => Ok(Conf::load(Path::new(DEFAULT_CONF))?.socket),
        None => Ok(DEFAULT_SOCKET.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_units() {
        let c = Conf::parse("[unit AB12345678]\n[unit cd23456789]\n").unwrap();
        assert_eq!(c.firmware_dir, Path::new(DEFAULT_FIRMWARE_DIR));
        assert_eq!(c.socket, Path::new(DEFAULT_SOCKET));
        assert_eq!(c.units, ["AB12345678", "cd23456789"]);
        assert_eq!(
            c.idle_firmware(),
            Path::new("/usr/lib/firmware/dt295/mb86m01_assp_osec_idle.bin")
        );
        assert_eq!(
            c.state_file("cd23456789"),
            Path::new("/var/lib/dt295/CD23456789.state")
        );
    }

    #[test]
    fn settings() {
        let c = Conf::parse(
            "; comment\n[dt295]\nfirmware-dir = /opt/fw\nState-Dir=/srv/dt295\n\
             socket = /tmp/dt295.sock\n\n[unit AB12345678]\n",
        )
        .unwrap();
        assert_eq!(c.firmware_dir, Path::new("/opt/fw"));
        assert_eq!(c.state_dir, Path::new("/srv/dt295"));
        assert_eq!(c.socket, Path::new("/tmp/dt295.sock"));
        assert_eq!(
            c.trc_firmware(),
            Path::new("/opt/fw/mb86m01_assp_osec_trc_m_h.bin")
        );
    }

    #[test]
    fn mistakes_are_errors() {
        let err = |text: &str| Conf::parse(text).unwrap_err().to_string();
        assert!(err("").contains("no [unit"));
        assert!(err("[dt295]\nfirmware = /x\n[unit A]\n").contains("unknown setting"));
        assert!(err("[unit A]\nname = x\n").contains("unknown setting"));
        assert!(err("[units A]\n").contains("unknown section"));
        assert!(err("[unit A B]\n").contains("unknown section"));
        assert!(err("[unit A]\n[unit a]\n").contains("twice"));
        assert!(err("socket = /x\n[unit A]\n").contains("before the first section"));
    }

    #[test]
    fn the_documented_example_parses() {
        // the example above (and in docs/usage.md): comments on lines of their own
        let c = Conf::parse(
            "[dt295]\n; the vendor's idle and trc images\nfirmware-dir = /usr/lib/firmware/dt295\n\
             ; <serial>.state per unit\nstate-dir = /var/lib/dt295\n\
             socket = /run/dt295/dt295.sock\n\n[unit AB12345678]\n[unit CD23456789]\n",
        )
        .unwrap();
        assert_eq!(c.firmware_dir, Path::new(DEFAULT_FIRMWARE_DIR));
        assert_eq!(c.units.len(), 2);
    }

    #[test]
    fn the_client_socket() {
        let given = Path::new("/tmp/x.sock");
        assert_eq!(client_socket(None, Some(given)).unwrap(), given);
        let conf = std::env::temp_dir().join(format!("dt295-conf-{}.conf", std::process::id()));
        std::fs::write(&conf, "[dt295]\nsocket = /srv/s.sock\n[unit A]\n").unwrap();
        let got = client_socket(Some(&conf), None);
        let _ = std::fs::remove_file(&conf);
        assert_eq!(got.unwrap(), Path::new("/srv/s.sock"));
        assert!(client_socket(Some(Path::new("/no/such.conf")), None).is_err());
    }
}
