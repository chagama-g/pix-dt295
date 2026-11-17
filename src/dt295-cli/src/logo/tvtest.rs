//! TVTest's logo files: the logo data file, the logo ID map and the logo folder, read
//! and merged with TVTest's own rules; and the BonDriver_DT295 ini files next to it.
//!
//! `LogoData`: a header ("LogoData", version 1, the count), then per logo a 24-byte
//! header (network_id, logo_id, version, logo_type, size, time: seconds since 1601-01-01
//! in Japan time; little-endian), the PNG and the CRC32/MPEG-2 of those two.
//! `LogoData.ini`: its `[LogoIDMap]` maps `network_id << 16 | service_id` (8 hex digits)
//! to the logo_id. The `Logo` folder holds `NNNN_LLL_VVV_TT`, PNGs without an extension.
//!
//! TVTest drops a logo of 93 bytes or less as transparent. It reads the files when it
//! starts; when it ends it reads them again if they are newer, merges, and writes them.
//! The version comes from the SDTT; without one TVTest uses 0 too.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use dt295::ts::crc::crc32;

use super::carousel::Logo;
use crate::bonini::{BonDriver, ini_section};

/// TVTest skips a logo of this size or smaller (a transparent placeholder).
pub(super) const TRANSPARENT_MAX: usize = 93;
/// The largest logo TVTest loads from its logo data file.
pub(super) const MAX_LOGO_BYTES: usize = 1296;

pub(super) const DATA_MAGIC: &[u8; 8] = b"LogoData";
pub(super) const DATA_HEADER: usize = 16;
/// Record header sizes of file versions 0 and 1.
pub(super) const REC_HEADER_V0: usize = 10;
pub(super) const REC_HEADER_V1: usize = 24;
pub(super) const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
pub(super) const ID_MAP_SECTION: &str = "LogoIDMap";

/// A logo in TVTest's logo data file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub(super) version: u16,
    /// Seconds since 1601-01-01 (Japan time).
    pub(super) time: u64,
    pub(super) png: Vec<u8>,
}

/// TVTest's logo data file: (network_id, logo_id, logo_type) -> logo.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct LogoData {
    pub(super) records: BTreeMap<(u16, u16, u8), Record>,
}

/// What a merge changed.
#[derive(Debug, Default)]
pub(super) struct Report {
    pub(super) added: Vec<(u16, u16, u8)>,
    pub(super) updated: Vec<(u16, u16, u8)>,
    pub(super) unchanged: usize,
    pub(super) transparent: usize,
    pub(super) too_large: usize,
    /// (network_id << 16 | service_id) -> (logo_id, may replace an existing entry)
    pub(super) id_map: BTreeMap<u32, (u16, bool)>,
}

impl Report {
    pub(super) fn summary(&self, logos: &[Logo]) -> String {
        let count = |nets: &[u16]| {
            logos
                .iter()
                .filter(|l| nets.contains(&l.network_id))
                .count()
        };
        let mut s = format!(
            "received {} logos (BS {}, CS {}); {} transparent placeholders skipped",
            logos.len(),
            count(&[4]),
            count(&[6, 7]),
            self.transparent
        );
        if self.too_large > 0 {
            s += &format!(", {} too large for TVTest skipped", self.too_large);
        }
        s += &format!(
            "\nlogo data: {} added, {} updated, {} unchanged; {} services in the logo ID map",
            self.added.len(),
            self.updated.len(),
            self.unchanged,
            self.id_map.len()
        );
        s
    }
}

/// TVTest's logo version order: 0..=2047 and 2048..=4095 wrap around.
pub(super) fn compare_version(a: u16, b: u16) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if a == b {
        Ordering::Equal
    } else if (a <= 2047) == (b <= 2047) {
        a.cmp(&b)
    } else if a <= 2047 {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

impl LogoData {
    /// Parse TVTest's logo data file. Any damage is an error: the file is not touched
    /// unless it was understood completely.
    pub(super) fn parse(b: &[u8]) -> Result<Self, String> {
        let header = b.get(..DATA_HEADER).ok_or("shorter than its header")?;
        if &header[..8] != DATA_MAGIC {
            return Err("not a TVTest logo data file".into());
        }
        let le32 =
            |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap_or_default());
        let (version, count) = (le32(8), le32(12));
        let rec_header = match version {
            0 => REC_HEADER_V0,
            1 => REC_HEADER_V1,
            v => return Err(format!("file version {v} is not known")),
        };
        let mut records = BTreeMap::new();
        let mut pos = DATA_HEADER;
        for i in 0..count {
            let bad = |what: &str| format!("logo {i}: {what}");
            let h = b
                .get(pos..pos + rec_header)
                .ok_or_else(|| bad("cut short"))?;
            let w = |at: usize| u16::from_le_bytes([h[at], h[at + 1]]);
            let (network_id, logo_id, logo_version, logo_type) = (w(0), w(2), w(4), h[6]);
            let size = usize::from(w(8));
            let time = if rec_header == REC_HEADER_V1 {
                u64::from_le_bytes(h[16..24].try_into().unwrap_or_default())
            } else {
                0
            };
            let png = b
                .get(pos + rec_header..pos + rec_header + size)
                .ok_or_else(|| bad("cut short"))?;
            let crc = b
                .get(pos + rec_header + size..pos + rec_header + size + 4)
                .ok_or_else(|| bad("cut short"))?;
            if crc32(&b[pos..pos + rec_header + size]).to_le_bytes() != crc {
                return Err(bad("CRC error"));
            }
            if logo_type > 5 || !png.starts_with(PNG_SIGNATURE) {
                return Err(bad("not a logo"));
            }
            records.insert(
                (network_id, logo_id, logo_type),
                Record {
                    version: logo_version,
                    time,
                    png: png.to_vec(),
                },
            );
            pos += rec_header + size + 4;
        }
        Ok(Self { records })
    }

    /// The file in TVTest's current format (version 1).
    pub(super) fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(DATA_MAGIC);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&u32::try_from(self.records.len()).unwrap_or(0).to_le_bytes());
        for (&(network_id, logo_id, logo_type), r) in &self.records {
            let start = b.len();
            b.extend_from_slice(&network_id.to_le_bytes());
            b.extend_from_slice(&logo_id.to_le_bytes());
            b.extend_from_slice(&r.version.to_le_bytes());
            b.push(logo_type);
            b.push(0);
            b.extend_from_slice(&u16::try_from(r.png.len()).unwrap_or(0).to_le_bytes());
            b.extend_from_slice(&[0; 6]);
            b.extend_from_slice(&r.time.to_le_bytes());
            b.extend_from_slice(&r.png);
            let crc = crc32(&b[start..]);
            b.extend_from_slice(&crc.to_le_bytes());
        }
        b
    }

    /// Merge received logos the way TVTest stores a downloaded logo: a newer version,
    /// or the same version received later, replaces different data. The logos carry
    /// version 0 (the version comes from an SDTT, which this command does not read;
    /// TVTest stores the same without one).
    pub(super) fn merge(&mut self, logos: &[Logo], time: u64) -> Report {
        let mut r = Report::default();
        for logo in logos {
            if logo.png.len() <= TRANSPARENT_MAX {
                r.transparent += 1;
                continue;
            }
            if logo.png.len() > MAX_LOGO_BYTES {
                r.too_large += 1;
                continue;
            }
            let key = (logo.network_id, logo.logo_id, logo.logo_type);
            let version = 0;
            let updated = match self.records.get_mut(&key) {
                None => {
                    self.records.insert(
                        key,
                        Record {
                            version,
                            time,
                            png: logo.png.clone(),
                        },
                    );
                    r.added.push(key);
                    true
                }
                Some(rec) => {
                    let newer = match compare_version(version, rec.version) {
                        std::cmp::Ordering::Greater => true,
                        std::cmp::Ordering::Equal => time > rec.time,
                        std::cmp::Ordering::Less => false,
                    };
                    if newer && rec.png != logo.png {
                        *rec = Record {
                            version,
                            time,
                            png: logo.png.clone(),
                        };
                        r.updated.push(key);
                        true
                    } else {
                        r.unchanged += 1;
                        false
                    }
                }
            };
            for &(network_id, _, service_id) in &logo.services {
                let k = (u32::from(network_id) << 16) | u32::from(service_id);
                let e = r.id_map.entry(k).or_insert((logo.logo_id, updated));
                e.1 |= updated;
            }
        }
        r
    }
}

/// Set the logo IDs in the text of TVTest's logo ID map (`[LogoIDMap]`, `XXXXXXXX=id`
/// with the key network_id << 16 | service_id in hex). An entry is added if missing
/// and replaced only if its logo was added or updated. Other lines stay as they are.
/// Returns the new text and (added, changed).
pub(super) fn set_logo_ids(text: &str, ids: &BTreeMap<u32, (u16, bool)>) -> (String, usize, usize) {
    let nl = if text.contains("\r\n") || text.is_empty() {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let is_header = |l: &str| l.trim_start().starts_with('[');
    let section = lines.iter().position(|l| {
        l.trim()
            .strip_circumfix('[', ']')
            .is_some_and(|s| s.trim().eq_ignore_ascii_case(ID_MAP_SECTION))
    });
    let start = match section {
        Some(i) => i + 1,
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("[{ID_MAP_SECTION}]"));
            lines.len()
        }
    };
    let end = lines[start..]
        .iter()
        .position(|l| is_header(l))
        .map_or(lines.len(), |i| start + i);

    let mut present: HashMap<u32, usize> = HashMap::new();
    for (i, l) in lines.iter().enumerate().take(end).skip(start) {
        if let Some((k, _)) = l.split_once('=')
            && let Ok(k) = u32::from_str_radix(k.trim(), 16)
        {
            present.insert(k, i);
        }
    }
    let (mut added, mut changed) = (0, 0);
    let mut new_lines = Vec::new();
    for (&key, &(id, replace)) in ids {
        let line = format!("{key:08X}={id}");
        match present.get(&key) {
            Some(&i) => {
                let current = lines[i].split_once('=').map(|(_, v)| v.trim());
                if replace && current != Some(&id.to_string()[..]) {
                    lines[i] = line;
                    changed += 1;
                }
            }
            None => {
                new_lines.push(line);
                added += 1;
            }
        }
    }
    // after the section's last entry (blank lines at its end stay at the end)
    let mut at = end;
    while at > start && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    lines.splice(at..at, new_lines);
    let mut out = lines.join(nl);
    out.push_str(nl);
    (out, added, changed)
}

/// A TVTest folder and the `[Settings]` of its TVTest.ini.
#[derive(Debug)]
pub(super) struct TvTest {
    pub(super) dir: PathBuf,
    settings: HashMap<String, String>,
}

impl TvTest {
    pub(super) fn open(dir: &Path) -> Result<Self> {
        if !dir.is_dir() {
            bail!("{} is not a folder", dir.display());
        }
        let ini_path = dir.join("TVTest.ini");
        let settings = match fs::read(&ini_path) {
            Ok(b) => ini_section(&decode_text(&b), "Settings"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!(
                    "note: no {}: using TVTest's default file names",
                    ini_path.display()
                );
                HashMap::new()
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", ini_path.display())),
        };
        Ok(Self {
            dir: dir.to_owned(),
            settings,
        })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.settings
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn yes(&self, key: &str) -> bool {
        self.get(key)
            .is_some_and(|v| v.eq_ignore_ascii_case("yes") || v == "1")
    }

    /// The BonDriver folder (`DriverDirectory`, default the TVTest folder itself).
    pub(super) fn driver_dir(&self) -> PathBuf {
        self.dir.join(self.get("DriverDirectory").unwrap_or("."))
    }
}

/// TVTest's logo files, found from TVTest.ini.
#[derive(Debug)]
pub(super) struct Store {
    pub(super) data: PathBuf,
    /// The logo ID map: the data file's path with ".ini" appended.
    pub(super) id_map: PathBuf,
    /// The logo folder, when TVTest saves logos there (or it exists).
    pub(super) dir: Option<PathBuf>,
}

impl Store {
    pub(super) fn locate(tv: &TvTest) -> Result<Self> {
        if tv
            .get("SaveLogoData")
            .is_some_and(|v| v.eq_ignore_ascii_case("no") || v == "0")
        {
            eprintln!("note: TVTest is set not to keep a logo data file (SaveLogoData=no)");
        }
        let data = tv
            .dir
            .join(tv.get("LogoDataFileName").unwrap_or("LogoData"));
        let id_map = data.with_added_extension("ini");
        let dir = tv.dir.join(tv.get("LogoDirectory").unwrap_or("Logo"));
        let dir = (tv.yes("SaveRawLogo") || dir.is_dir()).then_some(dir);
        Ok(Self { data, id_map, dir })
    }

    pub(super) fn load_data(&self) -> Result<LogoData> {
        match fs::read(&self.data) {
            Ok(b) => {
                LogoData::parse(&b).map_err(|e| anyhow::anyhow!("{}: {e}", self.data.display()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(LogoData::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", self.data.display())),
        }
    }

    pub(super) fn save(&self, data: &LogoData, report: &Report) -> Result<()> {
        if !report.added.is_empty() || !report.updated.is_empty() {
            replace_file(&self.data, &data.to_bytes())?;
            println!("wrote {}", self.data.display());
        }

        let old = match fs::read(&self.id_map) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", self.id_map.display()));
            }
        };
        let utf16 = old.as_deref().is_some_and(|b| b.starts_with(b"\xFF\xFE"));
        let text = old.as_deref().map_or_default(decode_text);
        let (text, added, changed) = set_logo_ids(&text, &report.id_map);
        if added + changed > 0 {
            replace_file(&self.id_map, &encode_text(&text, utf16))?;
            println!(
                "wrote {}: {added} services added, {changed} changed",
                self.id_map.display()
            );
        }

        // TVTest's own files (raw PNG, named network_logo_version_type); a running
        // TVTest reads a logo from here when it is not in memory. An existing file is
        // kept for an added logo, as TVTest does, and replaced for an updated one (the
        // version in the name stays 0).
        if let Some(dir) = &self.dir {
            let mut written = 0;
            let added = report.added.iter().map(|k| (k, false));
            for (key, replace) in added.chain(report.updated.iter().map(|k| (k, true))) {
                let Some(rec) = data.records.get(key) else {
                    continue;
                };
                let (network_id, logo_id, logo_type) = *key;
                let name = format!(
                    "{network_id:04X}_{logo_id:03X}_{:03X}_{logo_type:02X}",
                    rec.version
                );
                let path = dir.join(name);
                if !replace && path.exists() {
                    continue;
                }
                fs::create_dir_all(dir)
                    .with_context(|| format!("cannot create {}", dir.display()))?;
                fs::write(&path, &rec.png)
                    .with_context(|| format!("cannot write {}", path.display()))?;
                written += 1;
            }
            if written > 0 {
                println!("wrote {written} logo files to {}", dir.display());
            }
        }
        Ok(())
    }
}

/// Replace a file through a temporary one, keeping the old one as `.bak`.
pub(super) fn replace_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let with = |ext: &str| {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        PathBuf::from(p)
    };
    let tmp = with(".tmp");
    fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    if path.exists() {
        let bak = with(".bak");
        fs::copy(path, &bak).with_context(|| format!("cannot write {}", bak.display()))?;
    }
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))
}

/// A text file as TVTest writes them: UTF-16 LE with a BOM, or UTF-8 / ASCII.
pub(super) fn decode_text(b: &[u8]) -> String {
    match b.strip_prefix(b"\xFF\xFE") {
        Some(rest) => String::from_utf16le_lossy(rest),
        None => String::from_utf8_lossy(b.trim_prefix(b"\xEF\xBB\xBF")).into_owned(),
    }
}

pub(super) fn encode_text(text: &str, utf16: bool) -> Vec<u8> {
    if utf16 {
        let mut b = vec![0xFF, 0xFE];
        b.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        b
    } else {
        text.as_bytes().to_vec()
    }
}

/// The unit to use: a free one, preferably one a BonDriver ini names by serial (then
/// its state file gives the link key the unit may still be using). An ini without a
/// serial is taken for the only unit connected. `free` are the free units' serials,
/// `connected` the number of units connected.
pub(super) fn pick_unit<'a>(
    free: &[String],
    connected: usize,
    bons: &'a [BonDriver],
) -> Option<(String, Option<&'a BonDriver>)> {
    for s in free {
        let named = bons.iter().find(|b| {
            b.serial
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(s))
        });
        if let Some(b) = named {
            return Some((s.clone(), Some(b)));
        }
    }
    let first = free.first()?;
    let unnamed: Vec<_> = bons.iter().filter(|b| b.serial.is_none()).collect();
    let bon = (connected == 1 && unnamed.len() == 1).then(|| unnamed[0]);
    Some((first.clone(), bon))
}
