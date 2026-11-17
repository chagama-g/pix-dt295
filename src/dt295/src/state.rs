//! The host's persistent state file (`BonDriver_DT295.state`), shared by the
//! BonDriver and the CLI:
//!
//! ```text
//! # comments
//! dt295-state 1
//! linkkey <32 hex>
//! linkkey-pending <32 hex>
//! psi <kind> <other> <id> <sub> <seen_day> <section hex>
//! rawchannel <space> <channel>
//! ```
//! One record per line; unknown keywords (also an older version's `service` lines) are
//! skipped and go at the next save. Saved via `<file>.tmp` and a rename so a crash keeps
//! the previous file.
//!
//! Parsing: the first whitespace-separated token of a line is the keyword, `#...` lines are
//! comments, unknown keywords (including the `dt295-state` version line) are
//! skipped, and a record whose fields do not parse is skipped on its own. Extra
//! tokens after a record's fields are ignored. Hex fields are pairs of hex digits
//! (any case); a `linkkey` must be exactly 16 bytes, a `psi` section non-empty.
//!
//! `linkkey` is the link key a stream proved; `linkkey-pending` (newest first, at most
//! [`MAX_PENDING_KEYS`]) are keys the device handed out to processes that ended before
//! a stream proved them. After the device's secure session lapsed, the first key it
//! hands out is the one it applies, so one of these may be it.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::PsiRecord;

/// The whole state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub link_key: Option<[u8; 16]>,
    /// Keys handed out but never proved, newest first.
    pub pending_keys: Vec<[u8; 16]>,
    pub psi: Vec<PsiRecord>,
    /// Raw-TS: the last-viewed (space, channel), primed on at the next open so the first
    /// SetChannel is a fast resume.
    pub raw_channel: Option<(u32, u32)>,
}

/// The most `linkkey-pending` keys kept.
pub const MAX_PENDING_KEYS: usize = 4;

/// The comment lines at the top of every state file.
const HEADER: &str = "# BonDriver_DT295 state: written by the driver, rebuilt if deleted.\n\
                      # Do not edit while the tuner is open.\n\
                      dt295-state 1\n";

impl State {
    /// Keep a key the device handed out (before a stream proves it): newest first,
    /// at most [`MAX_PENDING_KEYS`], not the proved key itself.
    pub fn add_pending_key(&mut self, key: [u8; 16]) {
        if self.link_key == Some(key) {
            return;
        }
        self.pending_keys.retain(|k| *k != key);
        self.pending_keys.insert(0, key);
        self.pending_keys.truncate(MAX_PENDING_KEYS);
    }

    /// Parse the text format.
    pub fn parse(text: &str) -> Self {
        let mut st = State::default();
        for line in text.lines() {
            let mut tok = line.split_ascii_whitespace();
            let Some(keyword) = tok.next() else { continue };
            match keyword {
                "linkkey" => {
                    if let Some(key) = tok
                        .next()
                        .and_then(parse_hex)
                        .and_then(|v| <[u8; 16]>::try_from(v).ok())
                    {
                        st.link_key = Some(key);
                    }
                }
                "linkkey-pending" => {
                    if let Some(key) = tok
                        .next()
                        .and_then(parse_hex)
                        .and_then(|v| <[u8; 16]>::try_from(v).ok())
                        && st.pending_keys.len() < MAX_PENDING_KEYS
                        && !st.pending_keys.contains(&key)
                    {
                        st.pending_keys.push(key);
                    }
                }
                "psi" => st.psi.extend(parse_psi(&mut tok)),
                "rawchannel" => {
                    if let (Some(s), Some(c)) = (
                        tok.next().and_then(|t| t.parse().ok()),
                        tok.next().and_then(|t| t.parse().ok()),
                    ) {
                        st.raw_channel = Some((s, c));
                    }
                }
                _ => {} // '#' comments, "dt295-state", and keywords of a newer driver
            }
        }
        st
    }

    /// Render the text format (`\n` line ends; [`State::save`] writes the platform's
    /// text-mode line ends: CRLF on Windows).
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::from(HEADER);
        if let Some(key) = &self.link_key {
            out.push_str("linkkey ");
            push_hex(&mut out, key);
            out.push('\n');
        }
        for key in &self.pending_keys {
            out.push_str("linkkey-pending ");
            push_hex(&mut out, key);
            out.push('\n');
        }
        for r in &self.psi {
            let _ = write!(
                out,
                "psi {} {} {} {} {} ",
                r.kind,
                u8::from(r.other),
                r.id,
                r.sub,
                r.seen_day
            );
            push_hex(&mut out, &r.section);
            out.push('\n');
        }
        if let Some((space, channel)) = self.raw_channel {
            let _ = writeln!(out, "rawchannel {space} {channel}");
        }
        out
    }

    /// Load `path`; `Ok(None)` if it does not exist.
    pub fn load(path: &Path) -> std::io::Result<Option<Self>> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(Self::parse(&String::from_utf8_lossy(&bytes)))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Write `path` atomically (`<path>.tmp` + rename over).
    ///
    /// The data is synced to disk before the rename, which replaces an existing
    /// file (on Windows too). On any error the `.tmp` file is removed and the
    /// previous `path` is left as it was.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = tmp_path(path);
        let mut text = self.render();
        if cfg!(windows) {
            text = text.replace('\n', "\r\n");
        }
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

/// `<path>.tmp` (the suffix is appended to the full file name).
fn tmp_path(path: &Path) -> PathBuf {
    path.with_added_extension("tmp")
}

fn parse_psi<'a>(tok: &mut impl Iterator<Item = &'a str>) -> Option<PsiRecord> {
    let kind = tok.next()?.parse::<i64>().ok()?;
    let other = tok.next()?.parse::<i64>().ok()?;
    let id = tok.next()?.parse::<i64>().ok()?;
    let sub = tok.next()?.parse::<i64>().ok()?;
    let seen = tok.next()?.parse::<i64>().ok()?;
    let section = parse_hex(tok.next()?)?;
    Some(PsiRecord {
        kind: u8::try_from(kind).ok()?,
        other: other != 0,
        id: u16::try_from(id).ok()?,
        sub: u16::try_from(sub).ok()?,
        seen_day: u32::try_from(seen).ok()?,
        section,
    })
}

/// Pairs of hex digits -> bytes; `None` if empty, odd-length or not hex.
fn parse_hex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || !s.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, _) = s.as_bytes().as_chunks::<2>();
    pairs
        .iter()
        .map(|&[hi, lo]| Some((hex_val(hi)? << 4) | hex_val(lo)?))
        .collect()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn push_hex(out: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.reserve(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0F)]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> State {
        State {
            pending_keys: Vec::new(),
            raw_channel: Some((2, 5)),
            link_key: Some([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
                0xEE, 0xFF,
            ]),
            psi: vec![
                PsiRecord {
                    kind: 1,
                    other: false,
                    id: 0x40F1,
                    sub: 0,
                    seen_day: 20_745,
                    section: vec![0x00, 0xB0, 0x0D, 0x40, 0xF1, 0xC1, 0x00, 0x00],
                },
                PsiRecord {
                    kind: 4,
                    other: true,
                    id: 0x4010,
                    sub: 1,
                    seen_day: 20_700,
                    section: vec![0x46, 0xF0, 0x11, 0xAB],
                },
            ],
        }
    }

    /// A unique scratch directory under the system temp dir.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dt295-state-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pending_keys() {
        let mut st = sample_state();
        let proved = st.link_key.unwrap();
        st.add_pending_key(proved);
        assert!(st.pending_keys.is_empty(), "the proved key is not pending");
        for b in 1..=6u8 {
            st.add_pending_key([b; 16]);
        }
        st.add_pending_key([5; 16]); // again: to the front, once
        assert_eq!(st.pending_keys, [[5; 16], [6; 16], [4; 16], [3; 16]]);
        let text = st.render();
        assert!(text.contains("linkkey-pending 05050505050505050505050505050505\n"));
        assert_eq!(State::parse(&text), st);
    }

    #[test]
    fn round_trip() {
        let st = sample_state();
        assert_eq!(State::parse(&st.render()), st);
        assert_eq!(State::parse(&State::default().render()), State::default());
    }

    /// A state file as written on Windows (CRLF).
    const SAMPLE: &str = "# BonDriver_DT295 state: written by the driver, rebuilt if deleted.\r\n\
        # Do not edit while the tuner is open.\r\n\
        dt295-state 1\r\n\
        linkkey 0123456789abcdeffedcba9876543210\r\n\
        psi 1 0 16400 0 20745 0000b00d4010c10000e010e0100000ffff\r\n\
        psi 4 1 16401 0 20700 46f0114011c100000004ff\r\n\
        psi 3 0 4 0 20745 40f00f0004c10000f0000000ffffffff\r\n";

    #[test]
    fn c_written_sample() {
        let st = State::parse(SAMPLE);
        assert_eq!(
            st.link_key,
            Some([
                0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54,
                0x32, 0x10
            ])
        );
        assert_eq!(st.psi.len(), 3);
        assert_eq!(
            st.psi[1],
            PsiRecord {
                kind: 4,
                other: true,
                id: 16401,
                sub: 0,
                seen_day: 20_700,
                section: vec![
                    0x46, 0xF0, 0x11, 0x40, 0x11, 0xC1, 0x00, 0x00, 0x00, 0x04, 0xFF
                ],
            }
        );
        assert_eq!(st.psi[2].kind, 3);
        assert_eq!(st.psi[2].id, 4);
        // The writer reproduces the file line for line.
        assert_eq!(st.render(), SAMPLE.replace("\r\n", "\n"));
    }

    #[test]
    fn unknown_keywords_and_comments_skipped() {
        let text = "dt295-state 2\n\
                    # linkkey 00112233445566778899aabbccddeeff\n\
                    \x20  # indented comment\n\
                    \n\
                    future-record 1 2 3\n\
                    services 0 1 2 3\n\
                    service 0 473142857 65535 1024 trailing tokens\n\
                    pin 0 0\n";
        let st = State::parse(text);
        assert_eq!(st.link_key, None);
        assert!(st.psi.is_empty());
    }

    #[test]
    fn malformed_lines_skipped() {
        let text = "linkkey 0011\n\
                    linkkey 00112233445566778899aabbccddeeffaa\n\
                    linkkey 00112233445566778899aabbccddeegg\n\
                    linkkey\n\
                    psi 1 0 16400 0 20745\n\
                    psi 1 0 16400 0 20745 abc\n\
                    psi 1 0 16400 0 20745 zz\n\
                    psi 1 0 70000 0 20745 00\n\
                    psi 1 0 16400 0 -1 00\n\
                    linkkey 00112233445566778899AABBCCDDEEFF\n\
                    psi 2 0 16400 151 20745 02b0\n";
        let st = State::parse(text);
        assert_eq!(
            st.link_key,
            Some([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
                0xEE, 0xFF
            ])
        );
        assert_eq!(st.psi.len(), 1);
        assert_eq!(st.psi[0].section, vec![0x02, 0xB0]);
        assert_eq!(st.psi[0].sub, 151);
    }

    #[test]
    fn last_linkkey_wins() {
        let st = State::parse(
            "linkkey 00000000000000000000000000000000\nlinkkey ffffffffffffffffffffffffffffffff\n",
        );
        assert_eq!(st.link_key, Some([0xFF; 16]));
    }

    #[test]
    fn load_missing_is_none() {
        let dir = scratch_dir("missing");
        assert_eq!(State::load(&dir.join("nope.state")).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_load_and_replace() {
        let dir = scratch_dir("save");
        let path = dir.join("BonDriver_DT295.state");
        fs::write(&path, "old contents\n").unwrap();

        let st = sample_state();
        st.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), Some(st.clone()));
        assert!(!tmp_path(&path).exists(), "the .tmp file is renamed away");
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with("# BonDriver_DT295 state"));
        if cfg!(windows) {
            assert!(raw.contains("dt295-state 1\r\n"));
        }

        // Saving again replaces the existing file.
        let smaller = State {
            pending_keys: Vec::new(),
            link_key: None,
            psi: vec![],
            raw_channel: None,
        };
        smaller.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), Some(smaller));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_save_keeps_previous_file() {
        let dir = scratch_dir("fail");
        // The target is a directory: the rename fails, the .tmp file is cleaned up.
        let path = dir.join("is-a-dir");
        fs::create_dir_all(path.join("child")).unwrap();
        assert!(sample_state().save(&path).is_err());
        assert!(!tmp_path(&path).exists());
        assert!(path.join("child").is_dir());
        let _ = fs::remove_dir_all(&dir);
    }
}
