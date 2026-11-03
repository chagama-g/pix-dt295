//! `dt295 state show`: a summary of a BonDriver state file.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use dt295::PSI_KEEP_DAYS;
use dt295::state::State;

pub fn run(path: &Path) -> Result<()> {
    let st = State::load(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .with_context(|| format!("{} does not exist", path.display()))?;
    print!("{}", summarize(&st, dt295::today()));
    Ok(())
}

fn table_name(kind: u8, other: bool) -> String {
    match (kind, other) {
        (1, _) => "PAT".into(),
        (2, _) => "PMT".into(),
        (3, _) => "NIT".into(),
        (4, false) => "SDT".into(),
        (4, true) => "SDT[other]".into(),
        (k, _) => format!("kind {k}"),
    }
}

/// `YYYY-MM-DD` of a Unix day number (days since 1970-01-01).
fn date(day: u32) -> String {
    // civil_from_days (H. Hinnant)
    let z = i64::from(day) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn ago(day: u32, today: u32) -> String {
    match today.checked_sub(day) {
        Some(0) => "today".into(),
        Some(1) => "1 day ago".into(),
        Some(n) => format!("{n} days ago"),
        None => "in the future".into(),
    }
}

/// The text `state show` prints (`today` = Unix day, for the ages).
pub fn summarize(st: &State, today: u32) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "link key: {}",
        if st.link_key.is_some() {
            "present"
        } else {
            "absent"
        }
    );
    if !st.pending_keys.is_empty() {
        let _ = writeln!(
            s,
            "unproved link keys: {} (handed out to runs that ended before a stream proved them)",
            st.pending_keys.len()
        );
    }

    let _ = writeln!(s, "PSI sections: {}", st.psi.len());
    let (Some(oldest), Some(newest)) = (
        st.psi.iter().map(|r| r.seen_day).min(),
        st.psi.iter().map(|r| r.seen_day).max(),
    ) else {
        return s;
    };
    let _ = writeln!(
        s,
        "  oldest seen {} ({}), newest seen {} ({})",
        date(oldest),
        ago(oldest, today),
        date(newest),
        ago(newest, today)
    );
    if oldest.saturating_add(PSI_KEEP_DAYS) < today {
        let _ = writeln!(
            s,
            "  (sections unseen for more than {PSI_KEEP_DAYS} days are dropped at the next open)"
        );
    }

    // Per table and TS (network for the NIT): count, seen range, bytes.
    let mut groups: BTreeMap<(u8, bool, u16), Group> = BTreeMap::new();
    for r in &st.psi {
        let g = groups.entry((r.kind, r.other, r.id)).or_insert(Group {
            sections: 0,
            first: u32::MAX,
            last: 0,
            bytes: 0,
        });
        g.sections += 1;
        g.first = g.first.min(r.seen_day);
        g.last = g.last.max(r.seen_day);
        g.bytes += r.section.len();
    }
    for ((kind, other, id), g) in groups {
        let what = if kind == 3 { "network" } else { "tsid" };
        let seen = if g.first == g.last {
            date(g.first)
        } else {
            format!("{} .. {}", date(g.first), date(g.last))
        };
        let _ = writeln!(
            s,
            "  {:<10} {what:<7} 0x{id:04X}: {:3} section(s), {:6} bytes, seen {seen}",
            table_name(kind, other),
            g.sections,
            g.bytes
        );
    }
    s
}

/// The PSI sections of one table of one TS.
struct Group {
    sections: usize,
    first: u32,
    last: u32,
    bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use dt295::PsiRecord;

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(10_957), "2000-01-01");
        assert_eq!(date(11_016), "2000-02-29");
        assert_eq!(date(20_745), "2026-10-19");
        assert_eq!(ago(20_745, 20_745), "today");
        assert_eq!(ago(20_744, 20_745), "1 day ago");
        assert_eq!(ago(20_700, 20_745), "45 days ago");
    }

    fn psi(kind: u8, other: bool, id: u16, seen_day: u32) -> PsiRecord {
        PsiRecord {
            kind,
            other,
            id,
            sub: 0,
            seen_day,
            section: vec![0; 16],
        }
    }

    #[test]
    fn summary() {
        let st = State {
            link_key: Some([1; 16]),
            pending_keys: Vec::new(),
            psi: vec![
                psi(1, false, 0x40F1, 20_740),
                psi(4, false, 0x40F1, 20_745),
                psi(4, true, 0x40F2, 20_745),
                psi(4, false, 0x40F1, 20_744),
                psi(3, false, 0x0004, 20_745),
            ],
        };
        let text = summarize(&st, 20_745);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "link key: present");
        assert_eq!(lines[1], "PSI sections: 5");
        assert_eq!(
            lines[2],
            "  oldest seen 2026-10-14 (5 days ago), newest seen 2026-10-19 (today)"
        );
        assert_eq!(
            lines[3],
            "  PAT        tsid    0x40F1:   1 section(s),     16 bytes, seen 2026-10-14"
        );
        assert_eq!(
            lines[4],
            "  NIT        network 0x0004:   1 section(s),     16 bytes, seen 2026-10-19"
        );
        assert_eq!(
            lines[5],
            "  SDT        tsid    0x40F1:   2 section(s),     32 bytes, seen 2026-10-18 .. 2026-10-19"
        );
        assert_eq!(
            lines[6],
            "  SDT[other] tsid    0x40F2:   1 section(s),     16 bytes, seen 2026-10-19"
        );
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn empty_and_expired() {
        let text = summarize(&State::default(), 20_745);
        assert_eq!(text, "link key: absent\nPSI sections: 0\n");
        let st = State {
            psi: vec![psi(1, false, 1, 20_000)],
            ..State::default()
        };
        assert!(summarize(&st, 20_745).contains("dropped at the next open"));
    }
}
