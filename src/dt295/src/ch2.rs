//! TVTest's `.ch2` channel file, what a channel scan registered: one line per
//! service, `name,space,channel,remote,service_type,service_id,network_id,tsid,enabled`
//! (`;` starts a comment). `space` and `channel` are the BonDriver's tuning space and
//! channel indices.

use std::fs;
use std::io;
use std::path::Path;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub space: u32,
    pub channel: u32,
    pub service_type: u32,
    pub sid: u16,
    pub nid: u16,
    pub tsid: u16,
    pub enabled: bool,
}

/// The services of a `.ch2` text, in file order; malformed lines are skipped.
pub fn parse(text: &str) -> Vec<Service> {
    let mut out = Vec::new();
    for line in text.split(['\r', '\n']) {
        let s = line.trim();
        if s.is_empty() || s.starts_with(';') {
            continue;
        }
        // split from the right: the name may contain commas
        let mut f: Vec<&str> = s.rsplitn(9, ',').collect();
        if f.len() != 9 {
            continue;
        }
        f.reverse();
        let num = |s: &str| s.trim().parse::<u32>().ok();
        let id = |s: &str| s.trim().parse::<u16>().ok();
        let (Some(space), Some(channel), Some(service_type), Some(sid), Some(nid), Some(tsid)) = (
            num(f[1]),
            num(f[2]),
            num(f[4]),
            id(f[5]),
            id(f[6]),
            id(f[7]),
        ) else {
            continue;
        };
        out.push(Service {
            name: f[0].trim().to_owned(),
            space,
            channel,
            service_type,
            sid,
            nid,
            tsid,
            enabled: num(f[8]) == Some(1),
        });
    }
    out
}

/// Read a `.ch2` file: UTF-16 (LE or BE) with a BOM, else UTF-8 (BOM optional).
pub fn read(path: &Path) -> io::Result<Vec<Service>> {
    let data = fs::read(path)?;
    let text = match data.as_slice() {
        [0xFF, 0xFE, rest @ ..] => String::from_utf16le_lossy(rest),
        [0xFE, 0xFF, rest @ ..] => String::from_utf16be_lossy(rest),
        d => String::from_utf8_lossy(d.trim_prefix(UTF8_BOM)).into_owned(),
    };
    Ok(parse(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services() {
        let text = "; TVTest Channel File\r\n\
                    ＮＨＫ総合１・東京,0,14,1,1,1024,32736,32736,1\r\n\
                    Name, with comma,2,40,1,1,101,4,16625,0\r\n\
                    broken,line\r\n\
                    big,3,0,1,1,70000,7,1,1\r\n";
        let s = parse(text);
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0],
            Service {
                name: "ＮＨＫ総合１・東京".into(),
                space: 0,
                channel: 14,
                service_type: 1,
                sid: 1024,
                nid: 32736,
                tsid: 32736,
                enabled: true,
            }
        );
        assert_eq!(s[1].name, "Name, with comma");
        assert!(!s[1].enabled);
    }
}
