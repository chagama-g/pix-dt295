use super::*;
use clap::Parser;
use dt295::ts::crc::{crc32, set_section_crc};

/// The day the test state files are written.
const DAY: u32 = 20_744;
const TODAY: u32 = 20_745;

/// A long-form section: `table_id`, `byte1_hi` (the flag bits of byte 1), the
/// bytes after section_length up to the CRC; the length and CRC are filled in.
fn section(table_id: u8, byte1_hi: u8, body: &[u8]) -> Vec<u8> {
    let mut sec = vec![table_id, 0, 0];
    sec.extend_from_slice(body);
    sec.extend_from_slice(&[0; 4]);
    let sl = sec.len() - 3;
    sec[1] = byte1_hi | (sl >> 8) as u8;
    sec[2] = sl as u8;
    set_section_crc(&mut sec);
    sec
}

/// The SDT section for `s` (version 1); a service with a type gets a
/// service_descriptor.
fn build_sdt(s: &Sdt) -> Vec<u8> {
    let mut body = s.tsid.to_be_bytes().to_vec();
    body.extend_from_slice(&[0xC1 | (1 << 1), s.section_number, s.last_section_number]);
    body.extend_from_slice(&s.onid.to_be_bytes());
    body.push(0xFF);
    for &(sid, stype) in &s.services {
        let desc = match stype {
            Some(t) => vec![TAG_SERVICE, 6, t, 0, 3, 0x0E, 0x41, 0x42],
            None => vec![],
        };
        body.extend_from_slice(&sid.to_be_bytes());
        body.push(0xE3);
        body.extend_from_slice(&((4 << 13) | desc.len() as u16).to_be_bytes());
        body.extend_from_slice(&desc);
    }
    let table_id = if s.actual {
        TID_SDT_ACTUAL
    } else {
        TID_SDT_OTHER
    };
    section(table_id, 0xF0, &body)
}

fn sdt_full(
    actual: bool,
    tsid: u16,
    onid: u16,
    sections: (u8, u8),
    services: &[(u16, u8)],
) -> Vec<u8> {
    let s = Sdt {
        actual,
        tsid,
        onid,
        version: 1,
        section_number: sections.0,
        last_section_number: sections.1,
        services: services.iter().map(|&(sid, t)| (sid, Some(t))).collect(),
    };
    build_sdt(&s)
}

/// An `[actual]` SDT, section 0 of 0.
fn sdt(tsid: u16, onid: u16, services: &[(u16, u8)]) -> Vec<u8> {
    sdt_full(true, tsid, onid, (0, 0), services)
}

fn make_pat(tsid: u16, programs: &[(u16, u16)]) -> Vec<u8> {
    let mut body = tsid.to_be_bytes().to_vec();
    body.extend_from_slice(&[0xC3, 0, 0]);
    for &(prog, pid) in programs {
        body.extend_from_slice(&prog.to_be_bytes());
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    section(0x00, 0xB0, &body)
}

fn record(kind: u8, other: bool, id: u16, sub: u16, seen_day: u32, sec: Vec<u8>) -> PsiRecord {
    PsiRecord {
        kind,
        other,
        id,
        sub,
        seen_day,
        section: sec,
    }
}

fn sdt_record(sec: Vec<u8>, seen_day: u32) -> PsiRecord {
    let other = sec[0] == TID_SDT_OTHER;
    let (tsid, sub) = (u16::from_be_bytes([sec[3], sec[4]]), u16::from(sec[6]));
    record(KIND_SDT, other, tsid, sub, seen_day, sec)
}

/// A small lineup: three BS and two CS110 transport streams, and a PAT.
fn records(day: u32) -> Vec<PsiRecord> {
    let sdts = [
        sdt(
            0x40F1,
            4,
            &[
                (101, 0x01),
                (102, 0x01),
                (103, 0x01),
                (700, 0xC0),
                (929, 0xA4),
            ],
        ),
        sdt(0x4631, 4, &[(236, 0x01)]),
        sdt(0x4973, 4, &[(260, 0x01), (531, 0x02)]),
        sdt(
            0x6020,
            6,
            &[(296, 0x01), (298, 0x01), (1, 0xA5), (900, 0xC0)],
        ),
        sdt(0x7180, 7, &[(300, 0x01), (223, 0x01), (561, 0xAD)]),
    ];
    let mut out: Vec<PsiRecord> = sdts.into_iter().map(|s| sdt_record(s, day)).collect();
    out.push(record(
        1,
        false,
        0x4631,
        0,
        day,
        make_pat(0x4631, &[(0, 0x10), (236, 0x1F0)]),
    ));
    out
}

/// A state file as the driver writes it (CRLF), with records the tool ignores.
fn state_text(records: &[PsiRecord]) -> String {
    let mut lines = vec![
        "# written by the test".to_owned(),
        "dt295-state 1".to_owned(),
        format!("linkkey {}", "ab".repeat(16)),
        "service 1 1318000000 16625 101".to_owned(),
        "futurekeyword 1 2 3".to_owned(),
    ];
    for r in records {
        let hex: String = r.section.iter().map(|b| format!("{b:02x}")).collect();
        lines.push(format!(
            "psi {} {} {} {} {} {hex}",
            r.kind,
            u8::from(r.other),
            r.id,
            r.sub,
            r.seen_day
        ));
    }
    lines.join("\r\n") + "\r\n"
}

fn ts_of(records: &[PsiRecord]) -> BTreeMap<u16, TsInfo> {
    collect_ts(records, None, TODAY, &mut io::sink())
}

fn bs(ts: &BTreeMap<u16, TsInfo>, names: &ChannelNames, promo: bool) -> Vec<String> {
    bs_lines(ts, names, promo, false, &mut io::sink())
}

fn cs(ts: &BTreeMap<u16, TsInfo>, names: &ChannelNames, promo: bool) -> Vec<String> {
    cs_lines(ts, names, promo, false, &mut io::sink())
}

const CH2: &str = ";TVTest channel file\r\n\
    ;#SPACE(3,CS110)\r\n\
    ＴＢＳチャンネル１,3,0,296,1,296,6,24608,1\r\n\
    映画・ｃｈＮＥＣＯ,3,50,223,1,223,7,29056,1\r\n\
    Name, with comma,3,51,300,1,300,7,29056,1\r\n\
    broken line\r\n";

fn utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    out
}

/// A scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dt295-lineup-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn args(state: &Path) -> LineupArgs {
    LineupArgs {
        state: state.to_owned(),
        ch2: None,
        bondriver: None,
        write: false,
        only: None,
        promo: false,
        max_age_days: None,
        per_service: true,
    }
}

/// (exit code, stdout, stderr) of a run.
fn run_args(a: &LineupArgs) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(a, TODAY, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

// ---------------------------------------------------------------- SDT parsing

#[test]
fn sdt_parse() {
    let sec = &records(DAY)[0].section;
    assert_eq!(crc32(sec), 0, "a whole section including its CRC yields 0");
    let sdt = parse_sdt(sec).unwrap();
    assert!(sdt.actual);
    assert_eq!((sdt.tsid, sdt.onid, sdt.section_number), (0x40F1, 4, 0));
    assert_eq!(
        sdt.services,
        [
            (101, Some(0x01)),
            (102, Some(0x01)),
            (103, Some(0x01)),
            (700, Some(0xC0)),
            (929, Some(0xA4))
        ]
    );

    // [other], a later section, a service without a service_descriptor.
    let s = Sdt {
        actual: false,
        tsid: 0x7060,
        onid: 7,
        version: 1,
        section_number: 1,
        last_section_number: 1,
        services: vec![(294, Some(0x01)), (295, None)],
    };
    assert_eq!(parse_sdt(&build_sdt(&s)), Some(s));
}

#[test]
fn transponders_of_satellite_tsids() {
    let tp = |onid, tsid| transponder(onid, tsid).map(|t| (t.bs, t.num, t.if_khz));
    assert_eq!(tp(4, 0x40F1), Some((true, 15, 1_318_000)));
    assert_eq!(tp(4, 0x4030), Some((true, 3, 1_087_840)));
    assert_eq!(
        tp(4, 0x46D2),
        Some((true, 13, 1_279_640)),
        "bits 9-11 are not the number"
    );
    assert_eq!(tp(6, 0x6020), Some((false, 2, 1_613_000)));
    assert_eq!(tp(7, 0x7040), Some((false, 4, 1_653_000)));
    assert_eq!(tp(7, 0x7160), Some((false, 22, 2_013_000)));
    assert_eq!(tp(4, 0x4020), None, "BS-2: left-hand");
    assert_eq!(tp(7, 0x7030), None, "ND03: left-hand");
    // terrestrial TSIDs are 0x7xxx too (TOKYO MX: 0x7E87, bits 4-8 = 8)
    assert_eq!(tp(32391, 0x7E87), None);
    assert_eq!(tp(32736, 0x7FE0), None);
}

#[test]
fn bad_sections_rejected() {
    let good = sdt(0x4631, 4, &[(236, 0x01)]);
    let mut bad = good.clone();
    bad[12] ^= 0x01;
    assert_eq!(parse_sdt(&bad), None, "bad CRC");
    assert_eq!(parse_sdt(&make_pat(1, &[(1, 0x100)])), None, "not an SDT");
    assert_eq!(parse_sdt(&good[..good.len() - 1]), None, "truncated");

    let mut next = good.clone();
    next[5] &= !0x01; // current_next_indicator
    set_section_crc(&mut next);
    assert_eq!(parse_sdt(&next), None, "not current");

    // Trailing bytes after the section are ignored.
    let mut padded = good.clone();
    padded.extend_from_slice(&[0xFF; 3]);
    assert!(parse_sdt(&padded).is_some());
}

#[test]
fn bad_sections_skipped() {
    let mut sec = sdt(0x4631, 4, &[(236, 0x01)]);
    *sec.last_mut().unwrap() ^= 0xFF;
    let mut err = Vec::new();
    let ts = collect_ts(&[sdt_record(sec, DAY)], None, TODAY, &mut err);
    assert!(ts.is_empty());
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("dt295 lineup: 1 SDT section(s) failed the CRC/format check, skipped")
    );
}

// ---------------------------------------------------------------- input files

#[test]
fn state_text_round_trip() {
    let recs = records(DAY);
    let mut err = Vec::new();
    assert_eq!(parse_state(&state_text(&recs), &mut err).unwrap(), recs);
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
}

#[test]
fn state_header_and_bad_records() {
    let mut err = Vec::new();
    assert_eq!(
        parse_state("psi 4 0 1 0 0 00\n", &mut err).unwrap_err(),
        "line 1: expected a 'dt295-state 1' header"
    );
    assert_eq!(
        parse_state("# only a comment\n\n", &mut err).unwrap_err(),
        "no 'dt295-state' header (empty file?)"
    );
    assert!(err.is_empty());

    // A newer version is read anyway; hex ids and kind names are not supported.
    let sec = sdt(0x4631, 4, &[(236, 0x01)]);
    let hex: String = sec.iter().map(|b| format!("{b:02X}")).collect();
    let text = format!(
        "  # comment\ndt295-state 2\npsi sdt 0 0x4631 0 0 {hex}\npsi 4 0 17969 0 0 {hex}\n"
    );
    let recs = parse_state(&text, &mut err).unwrap();
    assert_eq!(recs, [record(KIND_SDT, false, 0x4631, 0, 0, sec)]);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "dt295 lineup: state file version 2 (expected 1); reading it anyway\n\
         dt295 lineup: line 3: malformed psi record, skipped\n"
    );
}

#[test]
fn ch2_encodings_and_lookups() {
    let dir = Scratch::new("ch2");
    let utf16be: Vec<u8> = [0xFE, 0xFF]
        .into_iter()
        .chain(CH2.encode_utf16().flat_map(u16::to_be_bytes))
        .collect();
    let files = [
        ("le.ch2", utf16le_with_bom(CH2)),
        ("be.ch2", utf16be),
        ("bom8.ch2", [UTF8_BOM, CH2.as_bytes()].concat()),
        ("utf8.ch2", CH2.as_bytes().to_vec()),
    ];
    for (name, data) in files {
        let p = dir.path(name);
        fs::write(&p, data).unwrap();
        let names = read_ch2(&p).unwrap();
        assert_eq!(names.len(), 3, "{name}");
        assert_eq!(names.get(0x6020, 6, 296), "ＴＢＳチャンネル１", "{name}");
        // Via network_id.
        assert_eq!(names.get(0x9999, 7, 223), "映画・ｃｈＮＥＣＯ", "{name}");
        assert_eq!(names.get(0x7180, 7, 300), "Name, with comma", "{name}");
        assert_eq!(names.get(0x7180, 7, 999), "", "{name}");
    }
    assert!(read_ch2(&dir.path("missing.ch2")).is_err());
}

// ---------------------------------------------------------------- lineup

#[test]
fn bs_one_line_per_service() {
    let ts = ts_of(&records(DAY));
    assert_eq!(
        bs(&ts, &ChannelNames::default(), false),
        [
            "; BS-3  (11765.84 MHz): 236",
            "BS03/236=1087840,0x4631,236",
            "; BS-15 (11996.00 MHz): 101 / 102 / 103 / 700",
            "BS15/101=1318000,0x40F1,101",
            "BS15/102=1318000,0x40F1,102",
            "BS15/103=1318000,0x40F1,103",
            "BS15/700=1318000,0x40F1,700",
            "; BS-23 (12149.44 MHz): 260 / 531",
            "BS23/260=1471440,0x4973,260",
            "BS23/531=1471440,0x4973,531",
        ]
    );
}

#[test]
fn cs_one_line_per_service() {
    let ts = ts_of(&records(DAY));
    assert_eq!(
        cs(&ts, &parse_ch2(CH2), false),
        [
            "; ND02: 296 ＴＢＳチャンネル１ / 298 / 900",
            "ND02/296=1613000,0x6020,296",
            "ND02/298=1613000,0x6020,298",
            "ND02/900=1613000,0x6020,900",
            "; ND24: 223 映画・ｃｈＮＥＣＯ / 300 Name, with comma / 561",
            "ND24/223=2053000,0x7180,223",
            "ND24/300=2053000,0x7180,300",
            "ND24/561=2053000,0x7180,561",
        ]
    );
    assert_eq!(
        cs(&ts, &ChannelNames::default(), false)[0],
        "; ND02: 296 / 298 / 900"
    );
    // Promo is opt-in.
    let promo = cs(&ts, &ChannelNames::default(), true);
    assert_eq!(
        promo[..2],
        ["; ND02: 1 / 296 / 298 / 900", "ND02/1=1613000,0x6020,1"]
    );
}

#[test]
fn one_line_per_ts() {
    let ts = ts_of(&records(DAY));
    let names = parse_ch2(CH2);
    assert_eq!(
        bs_lines(&ts, &ChannelNames::default(), false, true, &mut io::sink()),
        [
            "; BS-3  TS1 (11765.84 MHz): 236",
            "BS03/TS1=1087840,0x4631",
            "; BS-15 TS1 (11996.00 MHz): 101 / 102 / 103 / 700",
            "BS15/TS1=1318000,0x40F1",
            "; BS-23 TS3 (12149.44 MHz): 260 / 531",
            "BS23/TS3=1471440,0x4973",
        ]
    );
    assert_eq!(
        cs_lines(&ts, &names, false, true, &mut io::sink()),
        [
            "; ND02 TS0: 296 ＴＢＳチャンネル１ / 298 / 900",
            "ND02/TS0=1613000,0x6020",
            "; ND24 TS0: 223 映画・ｃｈＮＥＣＯ / 300 Name, with comma / 561",
            "ND24/TS0=2053000,0x7180",
        ]
    );
}

#[test]
fn actual_preferred_over_other() {
    let recs = [
        sdt_record(sdt_full(false, 0x4631, 4, (0, 0), &[(230, 1)]), DAY),
        sdt_record(sdt(0x4631, 4, &[(236, 1)]), DAY),
        sdt_record(sdt_full(false, 0x4632, 4, (0, 0), &[(251, 1)]), DAY),
    ];
    let lines = bs(&ts_of(&recs), &ChannelNames::default(), false);
    assert!(lines.contains(&"BS03/236=1087840,0x4631,236".to_owned()));
    // [other] only: used.
    assert!(lines.contains(&"BS03/251=1087840,0x4632,251".to_owned()));
    assert!(!lines.contains(&"BS03/230=1087840,0x4631,230".to_owned()));
}

#[test]
fn newest_copy_wins() {
    let recs = [
        sdt_record(sdt(0x4631, 4, &[(237, 1)]), DAY),
        sdt_record(sdt(0x4631, 4, &[(236, 1)]), DAY - 1),
        sdt_record(sdt(0x4632, 4, &[(250, 1)]), DAY),
        sdt_record(sdt(0x4632, 4, &[(251, 1)]), DAY),
    ];
    let lines = bs(&ts_of(&recs), &ChannelNames::default(), false);
    assert_eq!(
        lines[1..],
        ["BS03/237=1087840,0x4631,237", "BS03/251=1087840,0x4632,251"]
    );
}

#[test]
fn sections_merged() {
    let recs = [
        sdt_record(sdt_full(true, 0x7060, 7, (0, 1), &[(324, 1)]), DAY),
        sdt_record(sdt_full(true, 0x7060, 7, (1, 1), &[(294, 1)]), DAY),
    ];
    let lines = cs(&ts_of(&recs), &ChannelNames::default(), false);
    assert_eq!(
        lines,
        [
            "; ND06: 294 / 324",
            "ND06/294=1693000,0x7060,294",
            "ND06/324=1693000,0x7060,324"
        ]
    );
}

#[test]
fn max_age() {
    let recs = records(20_000);
    let mut err = Vec::new();
    assert!(collect_ts(&recs, Some(30), 20_744, &mut err).is_empty());
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("5 SDT section(s) older than 30 days, skipped")
    );
    assert_eq!(
        collect_ts(&recs, Some(30), 20_010, &mut io::sink()).len(),
        5
    );
    // A zero day is never too old.
    assert_eq!(
        collect_ts(&records(0), Some(30), 20_744, &mut io::sink()).len(),
        5
    );
}

#[test]
fn bad_transponders_and_duplicates_warned() {
    let recs = [
        sdt_record(sdt(0x4631, 4, &[(236, 1)]), DAY),
        sdt_record(sdt(0x4639, 4, &[(236, 1)]), DAY), // also BS-3, same service id
        sdt_record(sdt(0x4021, 4, &[(150, 1)]), DAY), // BS-2: left-hand
        sdt_record(sdt(0x4031, 4, &[(929, 0xA4)]), DAY), // engineering service only
        sdt_record(sdt(0x6030, 6, &[(400, 1)]), DAY), // ND03
    ];
    let ts = ts_of(&recs);
    let mut err = Vec::new();
    let lines = bs_lines(&ts, &ChannelNames::default(), false, false, &mut err);
    assert_eq!(
        lines,
        [
            "; BS-3  (11765.84 MHz): 236 / 236",
            "BS03/236=1087840,0x4631,236",
            "BS03/236=1087840,0x4639,236"
        ]
    );
    assert!(cs_lines(&ts, &ChannelNames::default(), false, false, &mut err).is_empty());
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "dt295 lineup: BS TSID 0x4021: transponder 2 is not a right-hand BS transponder, skipped\n\
         dt295 lineup: BS TSID 0x4031 has no TV, radio or data service, skipped\n\
         dt295 lineup: [BS]: duplicate channel name BS03/236\n\
         dt295 lineup: CS TSID 0x6030: ND03 is not a right-hand CS110 transponder, skipped\n"
    );
}

// ---------------------------------------------------------------- ini rewrite

#[test]
fn replace_section_edges() {
    let body = ["a=1".to_owned()];
    // Missing: appended after a blank line, with the file's line end.
    assert_eq!(
        replace_section("[X]\r\nk=v", "BS", &body),
        "[X]\r\nk=v\r\n\r\n[BS]\r\na=1\r\n"
    );
    assert_eq!(
        replace_section("[X]\n\n", "BS", &body),
        "[X]\n\n[BS]\na=1\n"
    );
    assert_eq!(replace_section("", "BS", &body), "[BS]\na=1\n");
    // Header at the end without a line end; case-insensitive header match.
    assert_eq!(replace_section("[bs]", "BS", &body), "[bs]\na=1\n");
    // What follows the `]` does not matter, as for the BonDriver: the section is
    // found, and it ends at such a header too.
    assert_eq!(
        replace_section("[BS] ; sat\nold\n[CS110] ; x\nc=1\n", "BS", &body),
        "[BS] ; sat\na=1\n\n[CS110] ; x\nc=1\n"
    );
    assert_eq!(
        replace_section("[BS]\nb=1\n[ CS110 ]  x\nold\n", "CS110", &body),
        "[BS]\nb=1\n[ CS110 ]  x\na=1\n"
    );
    // No comment before the next header: a blank line is put in.
    assert_eq!(
        replace_section(" [BS] \nold\n[Next]\n", "BS", &body),
        " [BS] \na=1\n\n[Next]\n"
    );
    // CR-only line ends are lines too.
    assert_eq!(
        split_lines_keepends("a\rb\r\nc\nd"),
        ["a\r", "b\r\n", "c\n", "d"]
    );
}

const INI: &str = "; BonDriver_DT295.ini - 日本語のコメント\r\n\
    \r\n\
    [Tuner]\r\n\
    UHF=1\r\n\
    \r\n\
    ; BS notes\r\n\
    [BS]\r\n\
    ; BS-1  (11727.48 MHz): old\r\n\
    BS01/TS0=1049480,0x4010,151\r\n\
    \r\n\
    ; CS notes 1\r\n\
    \r\n\
    ; CS notes 2\r\n\
    [CS110]\r\n\
    ; ND02: old\r\n\
    ND02/296=1613000,0x6020,296\r\n";

const GEN_BS: &str = "; BS-3  (11765.84 MHz): 236\r\n\
    BS03/236=1087840,0x4631,236\r\n\
    ; BS-15 (11996.00 MHz): 101 / 102 / 103 / 700\r\n\
    BS15/101=1318000,0x40F1,101\r\n\
    BS15/102=1318000,0x40F1,102\r\n\
    BS15/103=1318000,0x40F1,103\r\n\
    BS15/700=1318000,0x40F1,700\r\n\
    ; BS-23 (12149.44 MHz): 260 / 531\r\n\
    BS23/260=1471440,0x4973,260\r\n\
    BS23/531=1471440,0x4973,531\r\n";

const GEN_CS: &str = "; ND02: 296 / 298 / 900\r\n\
    ND02/296=1613000,0x6020,296\r\n\
    ND02/298=1613000,0x6020,298\r\n\
    ND02/900=1613000,0x6020,900\r\n\
    ; ND24: 223 / 300 / 561\r\n\
    ND24/223=2053000,0x7180,223\r\n\
    ND24/300=2053000,0x7180,300\r\n\
    ND24/561=2053000,0x7180,561\r\n";

/// A scratch dir with BonDriver_DT295.state (the small lineup) and
/// BonDriver_DT295.ini (`INI`).
fn ini_setup(name: &str) -> (Scratch, PathBuf, PathBuf) {
    let dir = Scratch::new(name);
    let state = dir.path("BonDriver_DT295.state");
    fs::write(&state, state_text(&records(DAY))).unwrap();
    let ini = dir.path("BonDriver_DT295.ini");
    fs::write(&ini, INI).unwrap();
    (dir, state, ini)
}

fn read(path: &Path) -> String {
    String::from_utf8(fs::read(path).unwrap()).unwrap()
}

#[test]
fn stdout_output() {
    let (_dir, state, _) = ini_setup("stdout");
    let (rc, out, err) = run_args(&args(&state));
    assert_eq!(rc, 0);
    assert_eq!(
        out,
        format!("[BS]\n{GEN_BS}\n[CS110]\n{GEN_CS}").replace("\r\n", "\n")
    );
    assert_eq!(
        err,
        format!(
            "dt295 lineup: {}: 6 sections; [BS] 7 lines, [CS110] 6 lines; names: none\n",
            state.display()
        )
    );

    let only_cs = LineupArgs {
        only: Some(Band::Cs),
        ..args(&state)
    };
    let (_, out, _) = run_args(&only_cs);
    assert!(
        out.starts_with("[CS110]\n; ND02: 296 / 298 / 900\n"),
        "{out}"
    );

    let promo = LineupArgs {
        promo: true,
        ..only_cs
    };
    let (_, out, _) = run_args(&promo);
    assert!(
        out.starts_with("[CS110]\n; ND02: 1 / 296 / 298 / 900\nND02/1=1613000,0x6020,1\n"),
        "{out}"
    );
    assert!(!out.contains("[BS]"));
}

#[test]
fn ch2_next_to_input() {
    let (dir, state, _) = ini_setup("ch2-next");
    let ch2 = dir.path("BonDriver_DT295.ch2");
    fs::write(&ch2, utf16le_with_bom(CH2)).unwrap();
    let only_cs = LineupArgs {
        only: Some(Band::Cs),
        ..args(&state)
    };
    let (_, out, err) = run_args(&only_cs);
    assert!(
        out.contains("; ND02: 296 ＴＢＳチャンネル１ / 298 / 900\n"),
        "{out}"
    );
    assert!(
        err.contains(&format!("names: {} (3)", ch2.display())),
        "{err}"
    );

    let no_names = LineupArgs {
        ch2: Some(PathBuf::from("-")),
        ..only_cs
    };
    let (_, out, _) = run_args(&no_names);
    assert!(out.contains("; ND02: 296 / 298 / 900\n"), "{out}");
}

#[test]
fn input_errors() {
    let dir = Scratch::new("input-errors");
    let (rc, out, err) = run_args(&args(&dir.path("missing.state")));
    assert_eq!((rc, out.as_str()), (2, ""));
    assert!(err.contains("missing.state: "), "{err}");

    let state = dir.path("empty.state");
    fs::write(&state, "").unwrap();
    let (rc, _, err) = run_args(&args(&state));
    assert_eq!(rc, 2);
    assert!(
        err.ends_with("no 'dt295-state' header (empty file?)\n"),
        "{err}"
    );

    // No PSI: both sections would be empty.
    fs::write(&state, "dt295-state 1\n").unwrap();
    let (rc, out, err) = run_args(&args(&state));
    assert_eq!((rc, out.as_str()), (1, "[BS]\n\n[CS110]\n"));
    assert!(err.contains(
        "[BS] would be empty: no valid SDT with a TV, radio or data service for that band"
    ));
    assert!(err.contains("[CS110] would be empty"));

    let missing_ch2 = LineupArgs {
        ch2: Some(dir.path("missing.ch2")),
        ..args(&state)
    };
    assert_eq!(run_args(&missing_ch2).0, 2);
}

#[test]
fn ini_dry_run_and_write() {
    let (_dir, state, ini) = ini_setup("write");
    let dry = LineupArgs {
        bondriver: Some(ini.clone()),
        ..args(&state)
    };
    let (rc, out, err) = run_args(&dry);
    assert_eq!(rc, 0);
    let shown = ini.display();
    assert!(
        out.starts_with(&format!("--- {shown}\n+++ {shown} (regenerated)\n@@ ")),
        "{out}"
    );
    assert!(out.contains("\n+BS03/236=1087840,0x4631,236\n"), "{out}");
    assert!(out.contains("\n-BS01/TS0=1049480,0x4010,151\n"), "{out}");
    assert!(
        err.ends_with("dt295 lineup: dry run; add --write to apply\n"),
        "{err}"
    );
    assert_eq!(read(&ini), INI);
    assert!(!suffixed(&ini, ".bak").exists());

    let write = LineupArgs { write: true, ..dry };
    let (rc, out, err) = run_args(&write);
    assert_eq!((rc, out.as_str()), (0, ""));
    assert!(
        err.contains(&format!("{shown} updated (backup: {shown}.bak)")),
        "{err}"
    );
    let expect = format!(
        "; BonDriver_DT295.ini - 日本語のコメント\r\n\r\n[Tuner]\r\nUHF=1\r\n\r\n\
         ; BS notes\r\n[BS]\r\n{GEN_BS}\
         \r\n; CS notes 1\r\n\r\n; CS notes 2\r\n[CS110]\r\n{GEN_CS}"
    );
    assert_eq!(read(&ini), expect);
    assert_eq!(read(&suffixed(&ini, ".bak")), INI);
    assert!(!suffixed(&ini, ".tmp").exists());
    let raw = fs::read(&ini).unwrap();
    assert!(!raw.starts_with(UTF8_BOM));
    assert_eq!(
        raw.iter().filter(|&&b| b == b'\n').count(),
        raw.windows(2).filter(|w| *w == b"\r\n").count()
    );

    // Idempotent.
    let (rc, out, err) = run_args(&write);
    assert_eq!((rc, out.as_str()), (0, ""));
    assert!(err.contains("already up to date"), "{err}");
    assert_eq!(read(&ini), expect);
}

#[test]
fn write_section_before_another() {
    let (_dir, state, ini) = ini_setup("before");
    fs::write(
        &ini,
        "[CS110]\r\nold=1\r\n; tail note\r\n[Other]\r\nkey=1\r\n[BS]\r\nBS99=x",
    )
    .unwrap();
    let write = LineupArgs {
        bondriver: Some(ini.clone()),
        write: true,
        ..args(&state)
    };
    assert_eq!(run_args(&write).0, 0);
    assert_eq!(
        read(&ini),
        format!("[CS110]\r\n{GEN_CS}; tail note\r\n[Other]\r\nkey=1\r\n[BS]\r\n{GEN_BS}")
    );
}

#[test]
fn bom_kept_and_lf_ini() {
    let (_dir, state, ini) = ini_setup("bom");
    fs::write(&ini, [UTF8_BOM, b"[BS]\nold\n".as_slice()].concat()).unwrap();
    let write = LineupArgs {
        bondriver: Some(ini.clone()),
        write: true,
        only: Some(Band::Bs),
        ..args(&state)
    };
    assert_eq!(run_args(&write).0, 0);
    let expect = format!("[BS]\n{GEN_BS}").replace("\r\n", "\n");
    assert_eq!(
        fs::read(&ini).unwrap(),
        [UTF8_BOM, expect.as_bytes()].concat()
    );

    // Not UTF-8: left alone.
    fs::write(&ini, b"[BS]\n\xFF\n").unwrap();
    let (rc, _, err) = run_args(&write);
    assert_eq!(rc, 2);
    assert!(err.contains("is not UTF-8"), "{err}");
    assert_eq!(fs::read(&ini).unwrap(), b"[BS]\n\xFF\n");
}

#[test]
fn refuses_empty_section() {
    let (_dir, state, ini) = ini_setup("refuse");
    let bs_only: Vec<PsiRecord> = records(DAY)
        .into_iter()
        .filter(|r| r.id >> 12 == 0x4)
        .collect();
    fs::write(&state, state_text(&bs_only)).unwrap();
    let write = LineupArgs {
        bondriver: Some(ini.clone()),
        write: true,
        ..args(&state)
    };
    let (rc, _, err) = run_args(&write);
    assert_eq!(rc, 1);
    assert!(err.contains("[CS110] would be empty"), "{err}");
    assert!(err.contains("refusing to write"), "{err}");
    assert_eq!(read(&ini), INI);
    assert!(!suffixed(&ini, ".bak").exists());

    // A dry run shows the diff but still exits 1.
    let dry = LineupArgs {
        write: false,
        bondriver: Some(ini.clone()),
        ..args(&state)
    };
    let (rc, out, _) = run_args(&dry);
    assert_eq!(rc, 1);
    assert!(out.contains("+BS03/236=1087840,0x4631,236"), "{out}");

    let only_bs = LineupArgs {
        only: Some(Band::Bs),
        ..write
    };
    assert_eq!(run_args(&only_bs).0, 0);
    assert_eq!(
        read(&ini),
        INI.replace(
            "; BS-1  (11727.48 MHz): old\r\nBS01/TS0=1049480,0x4010,151\r\n",
            GEN_BS
        )
    );
}

// ---------------------------------------------------------------- command line

#[derive(Debug, Parser)]
struct Cli {
    #[command(flatten)]
    args: LineupArgs,
}

fn parse(argv: &[&str]) -> Result<LineupArgs, clap::Error> {
    Cli::try_parse_from(std::iter::once("lineup").chain(argv.iter().copied())).map(|c| c.args)
}

#[test]
fn command_line() {
    let a = parse(&[
        "s.state",
        "--ini",
        "x.ini",
        "--write",
        "--only",
        "cs",
        "--promo",
        "--max-age-days",
        "30",
        "--ch2",
        "-",
    ])
    .unwrap();
    assert_eq!(a.state, Path::new("s.state"));
    assert_eq!(a.bondriver.as_deref(), Some(Path::new("x.ini")));
    assert!(a.write && a.promo);
    assert_eq!(a.only, Some(Band::Cs));
    assert_eq!(a.max_age_days, Some(30));
    assert_eq!(a.ch2.as_deref(), Some(Path::new("-")));

    let a = parse(&["s.state"]).unwrap();
    assert_eq!(
        (a.bondriver, a.write, a.only, a.ch2),
        (None, false, None, None)
    );
    let a = parse(&["s.state", "--bondriver", "y.ini"]).unwrap();
    assert_eq!(a.bondriver.as_deref(), Some(Path::new("y.ini")));

    assert!(
        parse(&["s.state", "--write"]).is_err(),
        "--write needs --bondriver"
    );
    assert!(parse(&["s.state", "--only", "x"]).is_err());
    assert!(parse(&[]).is_err());
}
