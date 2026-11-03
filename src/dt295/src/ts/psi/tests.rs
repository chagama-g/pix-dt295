//! Tests of the PSI keeper: its scenario step by step (the `step*` tests), plus
//! multi-section tables, continuity, the timing rules, options and malformed
//! input.

use std::time::{Duration, Instant};

use super::*;
use crate::ts::testutil::{Rng, packetize};

/// service_descriptor.
const DESC_SERVICE: u8 = 0x48;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A stream with no tuned service known (a plain parser on CS).
fn view(delivered: bool) -> StreamView {
    StreamView {
        delivered_any: delivered,
        ..StreamView::default()
    }
}

/// A stream of service `program`.
fn tuned(program: u16, delivered: bool) -> StreamView {
    StreamView {
        program: Some(program),
        delivered_any: delivered,
        ..StreamView::default()
    }
}

/// Observe `ts`; returns (the buffer as handed to the host afterwards, the re-sends).
fn observe(k: &mut PsiKeeper, ts: &[u8], v: &StreamView, now: Instant) -> (Vec<u8>, Vec<u8>) {
    let mut buf = ts.to_vec();
    let mut out = Vec::new();
    k.observe(&mut buf, v, now, &mut out);
    (buf, out)
}

fn tick(k: &mut PsiKeeper, v: &StreamView, now: Instant) -> Vec<u8> {
    let mut out = Vec::new();
    k.tick(v, now, &mut out);
    out
}

fn find(k: &PsiKeeper, kind: Kind, id: u16, sub: u16) -> Option<&Entry> {
    k.cache.find(Key::new(kind, id, sub))
}

/// SDT section: table `tid`, `tsid`, onid 4, one service `sid` with a long
/// descriptor so the section can span two packets. Port of `make_sdt`.
fn make_sdt(tid: u8, tsid: u16, sid: u16, desclen: usize) -> Vec<u8> {
    let mut sec = long_header(tid, tsid, 3);
    sec.extend_from_slice(&[0x00, 0x04, 0xFF]);
    sec.extend_from_slice(&sid.to_be_bytes());
    sec.push(0xFC);
    let dl = desclen + 2;
    sec.extend_from_slice(&[
        0x80 | (dl >> 8) as u8,
        dl as u8,
        DESC_SERVICE,
        desclen as u8,
    ]);
    sec.extend((0..desclen).map(|i| b'A' + (i % 26) as u8));
    finish(&mut sec);
    sec
}

/// SDT section `num` of `last` (onid 6) listing (service_id, service_type) services,
/// each with a service_descriptor.
fn sdt(tid: u8, tsid: u16, num: u8, last: u8, services: &[(u16, u8)]) -> Vec<u8> {
    let mut sec = long_header(tid, tsid, 5);
    sec[6] = num;
    sec[7] = last;
    sec.extend_from_slice(&[0x00, 0x06, 0xFF]);
    for &(sid, ty) in services {
        sec.extend_from_slice(&sid.to_be_bytes());
        sec.extend_from_slice(&[0xFC, 0x80, 5, DESC_SERVICE, 3, ty, 0, 0]);
    }
    finish(&mut sec);
    sec
}

/// PAT section of `tsid` with (program_number, PID) entries.
fn pat(tsid: u16, version: u8, entries: &[(u16, u16)]) -> Vec<u8> {
    let mut sec = long_header(TID_PAT, tsid, version);
    for &(prog, pid) in entries {
        sec.extend_from_slice(&prog.to_be_bytes());
        sec.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    finish(&mut sec);
    sec
}

/// A PMT of `prog` without ES (PCR 0x0100).
fn pmt(prog: u16, version: u8) -> Vec<u8> {
    let mut sec = long_header(TID_PMT, prog, version);
    sec.extend_from_slice(&[0xE1, 0x00, 0xF0, 0x00]);
    finish(&mut sec);
    sec
}

/// NIT section `num` of `last` of network `nid`.
fn nit(nid: u16, num: u8, last: u8) -> Vec<u8> {
    let mut sec = long_header(TID_NIT, nid, 1);
    sec[6] = num;
    sec[7] = last;
    sec.extend_from_slice(&[0xF0, 0x00, 0xF0, 0x00]);
    finish(&mut sec);
    sec
}

/// The sections sent on `pid` by the keeper (each starts a packet with pointer_field
/// 0), parsed independently of the keeper.
fn sent_sections(ts: &[u8], pid: u16) -> Vec<Vec<u8>> {
    assert_eq!(ts.len() % PACKET, 0, "whole packets");
    let mut secs: Vec<Vec<u8>> = Vec::new();
    for p in ts.as_chunks::<PACKET>().0 {
        assert_eq!(p[0], SYNC);
        if packet::pid(p) != pid {
            continue;
        }
        if packet::pusi(p) {
            assert_eq!(p[4], 0, "pointer_field");
            secs.push(p[5..].to_vec());
        } else {
            secs.last_mut()
                .expect("a start first")
                .extend_from_slice(&p[4..]);
        }
    }
    for s in &mut secs {
        let n = pat::section_total(s);
        s.truncate(n);
        assert!(crc::section_crc_ok(s), "sent section CRC");
    }
    secs
}

/// The continuity counters of the packets on `pid`.
fn ccs(ts: &[u8], pid: u16) -> Vec<u8> {
    let (packets, _) = ts.as_chunks::<PACKET>();
    packets
        .iter()
        .filter(|p| packet::pid(*p) == pid)
        .map(|p| packet::cc(p))
        .collect()
}

fn pids(ts: &[u8]) -> Vec<u16> {
    let (packets, _) = ts.as_chunks::<PACKET>();
    packets.iter().map(|p| packet::pid(p)).collect()
}

// ---------------------------------------------------------------- the scenario

/// Step 1: while on TS 0x4010, an SDT\[other\] for 0x4632 passes by
/// (two packets, pointer_field 3); then an SDT\[actual\] for 0x4010 and an
/// \[other\] copy of it.
fn step1() -> (PsiKeeper, Vec<u8>, Instant) {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4010), false, t);
    let other = make_sdt(TID_SDT_OTHER, 0x4632, 251, 220);
    assert!(other.len() > 184, "test SDT spans two packets");
    let mut cc = 0;
    observe(
        &mut k,
        &packetize(SDT_PID, &other, &mut cc, 3),
        &view(true),
        t,
    );
    let actual = make_sdt(TID_SDT, 0x4010, 151, 10);
    observe(
        &mut k,
        &packetize(SDT_PID, &actual, &mut cc, 0),
        &view(true),
        t,
    );
    let other_of_actual = make_sdt(TID_SDT_OTHER, 0x4010, 999, 10);
    observe(
        &mut k,
        &packetize(SDT_PID, &other_of_actual, &mut cc, 0),
        &view(true),
        t,
    );
    (k, other, t)
}

#[test]
fn step1_sdt_other_reassembled_and_cached() {
    let (k, other, _) = step1();
    let e = find(&k, Kind::Sdt, 0x4632, 0).expect("SDT[other] cached");
    assert!(
        e.other && e.sec == other,
        "SDT[other] reassembled and cached"
    );
}

#[test]
fn step1_other_does_not_displace_actual() {
    let (k, _, _) = step1();
    let e = find(&k, Kind::Sdt, 0x4010, 0).expect("SDT[actual] cached");
    assert!(
        !e.other && be16(&e.sec, 11) == 151,
        "[other] does not displace [actual]"
    );
}

/// 2. Tune 0x4632, the device delivers nothing: PAT synthesized, SDT relabelled.
#[test]
fn step2_starved_ts_gets_synthesized_pat_and_relabelled_sdt() {
    let (mut k, _, t) = step1();
    k.begin(Some(0x4632), false, t);
    assert!(
        tick(&mut k, &view(true), t).is_empty(),
        "nothing sent in the first second"
    );
    k.begin(Some(0x4632), true, t);
    assert!(
        !tick(&mut k, &view(false), t).is_empty(),
        "known-silent TS: PSI sent immediately"
    );
    k.begin(Some(0x4632), false, t);
    let now = t + ms(5000);
    let out = tick(&mut k, &view(true), now);
    assert!(!out.is_empty(), "starved TS gets PSI");

    // parse what was sent with a second keeper tuned to the same TS
    let mut c = PsiKeeper::new();
    c.begin(Some(0x4632), false, t);
    observe(&mut c, &out, &view(true), t); // PAT first: registers the PMT PIDs ...
    observe(&mut c, &out, &view(true), t); // ... so the second pass catches a PMT
    let e = find(&c, Kind::Pat, 0x4632, 0).expect("PAT");
    assert!(
        e.sec.len() == 8 + 8 + 4 && be16(&e.sec, 12) == 251 && be16(&e.sec, 14) & 0x1FFF == SYN_PMT,
        "synthesized PAT: NIT + service 251 -> PMT 0x1F00"
    );
    assert_eq!(be16(&e.sec, 8), 0, "NIT entry first");
    let e = find(&c, Kind::Sdt, 0x4632, 0).expect("SDT");
    assert!(
        !e.other && e.sec[0] == TID_SDT,
        "SDT re-sent as [actual] (0x42, CRC valid)"
    );
    assert!(
        find(&c, Kind::Pmt, 0x4632, 251).is_none() && k.sent.pmt == 0,
        "no made-up PMT (TVTest would drop an ES-less service)"
    );
    assert!(
        k.sent.pat == 1 && k.sent.sdt == 1,
        "PAT and SDT once this tick"
    );
    assert!(
        out.len().is_multiple_of(PACKET)
            && out.as_chunks::<PACKET>().0.iter().all(|p| p[0] == SYNC),
        "whole TS packets"
    );
    assert!(
        tick(&mut k, &view(true), now).is_empty(),
        "PAT/SDT re-sent at most every 0.5 / 1 s"
    );
}

/// 3. Healthy stream: the device's own PAT and SDT keep flowing -> nothing added.
#[test]
fn step3_healthy_stream_gets_nothing() {
    let (mut k, _, t) = step1();
    k.begin(Some(0x4010), false, t);
    let now = t + ms(5000);
    let mut cc = 0;
    observe(
        &mut k,
        &packetize(PAT_PID, &pat(0x4010, 2, &[(151, 0x01F0)]), &mut cc, 0),
        &view(true),
        now,
    );
    observe(
        &mut k,
        &packetize(SDT_PID, &make_sdt(TID_SDT, 0x4010, 151, 10), &mut cc, 0),
        &view(true),
        now,
    );
    assert!(
        tick(&mut k, &view(true), now).is_empty(),
        "healthy stream: nothing injected"
    );
    assert!(
        find(&k, Kind::Pat, 0x4010, 0).is_some(),
        "device PAT cached"
    );
}

/// 4. Undecryptable stream: leave it alone.
#[test]
fn step4_wrong_link_key_no_injection() {
    let (mut k, _, t) = step1();
    k.begin(Some(0x4632), false, t);
    let v = StreamView {
        undecryptable: true,
        ..view(true)
    };
    assert!(
        tick(&mut k, &v, t + ms(5000)).is_empty(),
        "wrong link key: no injection"
    );
    let v = StreamView {
        undecryptable: true,
        ..view(false)
    };
    assert!(tick(&mut k, &v, t + ms(5000)).is_empty());
}

/// 5. Export / import round trip (what the host keeps in its state file).
#[test]
fn step5_export_import_round_trip() {
    let (mut k, _, _) = step1();
    let recs = k.export();
    assert_eq!(recs.len(), k.cache.ent.len());
    let mut r = PsiKeeper::new();
    let today = crate::today();
    assert!(
        recs.iter().all(|rec| r.import(rec, today)),
        "every record loads"
    );
    assert_eq!(r.cache.ent.len(), recs.len(), "cache round trip");
    assert!(
        find(&r, Kind::Sdt, 0x4632, 0).is_some_and(|e| e.other),
        "loaded SDT[other] keeps its flag"
    );
    assert_eq!(r.export(), recs, "same records, same order");
}

/// 6. A cached real PAT/PMT wins over synthesis.
#[test]
fn step6_cached_real_pat_and_pmt_win_over_synthesis() {
    let (mut k, _, t) = step1();
    k.begin(Some(0x4450), false, t);
    let pat_sec = pat(0x4450, 5, &[(192, 0x0401)]);
    let pmt_sec = pmt(192, 7);
    let mut cc = 5; // the device's PAT goes out with cc 5, its PMT with 6
    observe(
        &mut k,
        &packetize(PAT_PID, &pat_sec, &mut cc, 0),
        &view(true),
        t,
    );
    observe(
        &mut k,
        &packetize(0x0401, &pmt_sec, &mut cc, 0),
        &view(true),
        t,
    );
    assert!(
        find(&k, Kind::Pmt, 0x4450, 192).is_some(),
        "device PMT cached via its PAT"
    );

    let t2 = t + ms(60_000); // next tune of 0x4450: device silent
    k.begin(Some(0x4450), false, t2);
    let out = tick(&mut k, &view(false), t2 + ms(5000));
    assert!(
        out.len() == 2 * PACKET
            && out[5..5 + pat_sec.len()] == pat_sec[..]
            && out[PACKET + 5..PACKET + 5 + pmt_sec.len()] == pmt_sec[..],
        "silent re-tune re-sends the real PAT + PMT (no SDT known for it)"
    );
    assert_eq!(pids(&out), vec![PAT_PID, 0x0401]);
    // continuity counters continue the device's on the same TS
    assert_eq!(ccs(&out, PAT_PID), vec![6], "continuity counter set");
    assert_eq!(ccs(&out, 0x0401), vec![7]);
    assert_eq!(k.sent.pmt, 1);
}

/// 7. Satellite: PAT/SDT\[actual\] reduced to the tuned service (CS; BS radio too).
#[test]
fn step7_satellite_reduced_to_the_tuned_service() {
    let svc: [u16; 4] = [55, 218, 318, 349];
    let typ: [u8; 4] = [0x01, 0x01, 0x01, 0xC0];
    let mut entries = vec![(0, 0x0010)];
    entries.extend(svc.iter().enumerate().map(|(k, &s)| (s, 0x040C + k as u16)));
    let pat_sec = pat(0x6080, 3, &entries);
    // SDT[actual] 0x6080 with four services, each a service_descriptor(type)
    let mut sdt_sec = sdt(
        TID_SDT,
        0x6080,
        0,
        0,
        &svc.iter().copied().zip(typ).collect::<Vec<_>>(),
    );
    sdt_sec[1] |= 0xF0; // SDT: reserved_future_use set
    crc::set_section_crc(&mut sdt_sec);

    let t = Instant::now();
    let mut x = PsiKeeper::new();
    x.begin(Some(0x6080), false, t);
    let mut cc = 0;
    let mut ts = packetize(PAT_PID, &pat_sec, &mut cc, 0);
    let n = ts.len();
    ts.extend(packetize(SDT_PID, &sdt_sec, &mut cc, 0));
    let (buf, out) = observe(&mut x, &ts, &tuned(218, true), t);
    assert_eq!(x.own, Some(218), "CS stream with a tuned service is owned");
    assert!(
        packet::pid(&buf) == NULL_PID && packet::pid(&buf[n..]) == NULL_PID,
        "device PAT/SDT packets blanked to NULL"
    );
    assert!(
        buf.as_chunks::<PACKET>()
            .0
            .iter()
            .all(|p| p[4..].iter().all(|&b| b == 0xFF))
    );
    {
        // parse what was re-sent with a plain parser (owns nothing)
        let mut y = PsiKeeper::new();
        y.begin(Some(0x6080), false, t);
        observe(&mut y, &out, &view(true), t);
        let ep = find(&y, Kind::Pat, 0x6080, 0).expect("PAT");
        assert!(
            ep.sec.len() == 8 + 8 + 4 && be16(&ep.sec, 8) == 0 && be16(&ep.sec, 12) == 218,
            "re-sent PAT: NIT + 218 only"
        );
        let ep = find(&y, Kind::Sdt, 0x6080, 0).expect("SDT");
        assert!(
            ep.sec.len() == 11 + 10 + 4 && be16(&ep.sec, 11) == 218 && ep.sec[1] & 0xF0 == 0xF0,
            "re-sent SDT[actual]: 218 only, header bits kept, CRC valid"
        );
    }
    // stale SID: tables go out unchanged
    x.begin(Some(0x6080), false, t);
    let (_, out) = observe(
        &mut x,
        &packetize(PAT_PID, &pat_sec, &mut cc, 0),
        &tuned(999, true),
        t,
    );
    assert!(
        out.len() == PACKET && out[5..5 + pat_sec.len()] == pat_sec[..],
        "stale SID: PAT re-sent unchanged"
    );
    // BS: the device's PAT lists every program of the TS; tuned to the radio service it
    // goes out as NIT + 531 only
    x.begin(Some(0x46D2), false, t);
    let bs_pat = pat(
        0x46D2,
        1,
        &[(0, 0x0010), (231, 0x01F0), (232, 0x02F0), (531, 0x09F0)],
    );
    let (buf, out) = observe(
        &mut x,
        &packetize(PAT_PID, &bs_pat, &mut cc, 0),
        &tuned(531, true),
        t,
    );
    assert_eq!(x.own, Some(531), "BS stream with a tuned service is owned");
    assert_eq!(packet::pid(&buf), NULL_PID, "device PAT blanked");
    let mut y = PsiKeeper::new();
    y.begin(Some(0x46D2), false, t);
    observe(&mut y, &out, &view(true), t);
    let ep = find(&y, Kind::Pat, 0x46D2, 0).expect("PAT");
    assert!(
        ep.sec.len() == 8 + 8 + 4 && be16(&ep.sec, 8) == 0 && be16(&ep.sec, 12) == 531,
        "re-sent BS PAT: NIT + 531 only"
    );
}

/// 8. Export / import for the BonDriver state file; 90-day ageing.
#[test]
fn step8_export_import_and_ageing() {
    let today = crate::today();
    let mut s = PsiKeeper::new();
    let sec = make_sdt(TID_SDT, 0x4011, 161, 4);
    let rec = |id: u16, seen_day: u32, section: &[u8]| PsiRecord {
        kind: Kind::Sdt as u8,
        other: false,
        id,
        sub: 0,
        seen_day,
        section: section.to_vec(),
    };
    assert!(s.import(&rec(0x4011, today, &sec), today));
    assert!(!s.changed(), "import alone does not mark the cache changed");
    assert!(
        !s.import(&rec(0x4012, today - PSI_KEEP_DAYS - 1, &sec), today),
        "a section unseen for over 90 days is dropped"
    );
    let mut bad = sec.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(
        !s.import(&rec(0x4013, today, &bad), today),
        "bad CRC rejected"
    );
    let recs = s.export();
    assert_eq!(recs.len(), 1, "export visits every cached section");
    let mut t = PsiKeeper::new();
    assert!(recs.iter().all(|r| t.import(r, today)));
    let e = find(&t, Kind::Sdt, 0x4011, 0).expect("imported");
    assert!(
        e.seen == today && e.sec.len() == sec.len(),
        "export -> import round trip keeps the seen day"
    );
}

// ---------------------------------------------------------------- more cases

#[test]
fn ageing_boundary_and_import_checks() {
    let today = 20_000;
    let sec = make_sdt(TID_SDT, 0x4011, 161, 4);
    let rec = PsiRecord {
        kind: 4,
        other: false,
        id: 0x4011,
        sub: 0,
        seen_day: today - PSI_KEEP_DAYS,
        section: sec.clone(),
    };
    let mut k = PsiKeeper::new();
    assert!(k.import(&rec, today), "exactly 90 days: kept");
    assert!(
        k.import(
            &PsiRecord {
                seen_day: u32::MAX,
                ..rec.clone()
            },
            today
        ),
        "no overflow"
    );
    for kind in [0, 5, 255] {
        assert!(
            !k.import(
                &PsiRecord {
                    kind,
                    ..rec.clone()
                },
                today
            ),
            "kind {kind}"
        );
    }
    assert!(
        !k.import(
            &PsiRecord {
                section: sec[..11].to_vec(),
                ..rec.clone()
            },
            today
        ),
        "too short"
    );
    let mut big = long_header(TID_SDT, 0x4011, 0);
    big.resize(SEC_MAX - 3, 0xFF);
    finish(&mut big);
    assert_eq!(big.len(), SEC_MAX + 1);
    assert!(
        !k.import(
            &PsiRecord {
                section: big,
                ..rec.clone()
            },
            today
        ),
        "over 1024 bytes"
    );
}

#[test]
fn unchanged_section_refreshes_its_seen_day() {
    let t = Instant::now();
    let today = crate::today();
    let mut k = PsiKeeper::new();
    let sec = make_sdt(TID_SDT, 0x4011, 161, 4);
    let rec = PsiRecord {
        kind: 4,
        other: false,
        id: 0x4011,
        sub: 0,
        seen_day: today - 10,
        section: sec.clone(),
    };
    assert!(k.import(&rec, today));
    assert!(!k.changed());
    k.begin(Some(0x4011), false, t);
    let mut cc = 0;
    observe(
        &mut k,
        &packetize(SDT_PID, &sec, &mut cc, 0),
        &view(true),
        t,
    );
    assert!(k.changed(), "still on air: seen day refreshed");
    assert_eq!(find(&k, Kind::Sdt, 0x4011, 0).unwrap().seen, today);
    assert_eq!(k.cache.ent.len(), 1);
    k.export();
    assert!(!k.changed(), "export clears it");
    observe(
        &mut k,
        &packetize(SDT_PID, &sec, &mut cc, 0),
        &view(true),
        t,
    );
    assert!(!k.changed(), "same section, same day: no change");
}

#[test]
fn multi_section_sdt_cached_resent_in_order_and_shrinks() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4031), false, t);
    let s0 = sdt(TID_SDT, 0x4031, 0, 1, &[(101, 1), (102, 1)]);
    let s1 = sdt(TID_SDT, 0x4031, 1, 1, &[(103, 1)]);
    let mut cc = 0;
    // section 1 first, and both in one packet sequence after one another
    let mut ts = packetize(SDT_PID, &s1, &mut cc, 0);
    ts.extend(packetize(SDT_PID, &s0, &mut cc, 0));
    observe(&mut k, &ts, &view(true), t);
    assert!(find(&k, Kind::Sdt, 0x4031, 0).is_some() && find(&k, Kind::Sdt, 0x4031, 1).is_some());

    // next tune: nothing delivered -> SDT in section order, PAT from both sections
    let t2 = t + ms(10_000);
    k.begin(Some(0x4031), false, t2);
    let out = tick(&mut k, &view(true), t2 + ms(3001));
    assert_eq!(sent_sections(&out, SDT_PID), vec![s0.clone(), s1.clone()]);
    assert_eq!(k.sent.sdt, 2);
    let pats = sent_sections(&out, PAT_PID);
    assert_eq!(pats.len(), 1);
    assert_eq!(
        pat::parse_pat(&pats[0]),
        vec![(101, SYN_PMT), (102, SYN_PMT + 1), (103, SYN_PMT + 2)],
        "services of every SDT section, in section order"
    );
    assert_eq!(
        pat::pat_entries(&pats[0]).next(),
        Some((0, 0x0010)),
        "NIT entry"
    );

    // the table shrinks to one section: section 1 goes
    let s0_only = sdt(TID_SDT, 0x4031, 0, 0, &[(101, 1), (102, 1), (103, 1)]);
    observe(
        &mut k,
        &packetize(SDT_PID, &s0_only, &mut cc, 0),
        &view(true),
        t2,
    );
    assert!(
        find(&k, Kind::Sdt, 0x4031, 1).is_none(),
        "higher section dropped"
    );
    assert_eq!(find(&k, Kind::Sdt, 0x4031, 0).unwrap().sec, s0_only);
    assert_eq!(k.cache.ent.len(), 1);
}

#[test]
fn sdt_other_never_drops_actual_sections_but_actual_replaces_other() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4010), false, t);
    let mut cc = 0;
    let mut feed = |k: &mut PsiKeeper, sec: Vec<u8>| {
        observe(k, &packetize(SDT_PID, &sec, &mut cc, 0), &view(true), t);
    };
    // TS 0x4632 known through SDT[other] sections 0 and 1
    feed(&mut k, sdt(TID_SDT_OTHER, 0x4632, 0, 1, &[(251, 1)]));
    feed(&mut k, sdt(TID_SDT_OTHER, 0x4632, 1, 1, &[(252, 1)]));
    // TS 0x4633: [actual] section 1 of 1; an [other] 0 of 0 must not drop it
    feed(&mut k, sdt(TID_SDT, 0x4633, 1, 1, &[(262, 1)]));
    feed(&mut k, sdt(TID_SDT_OTHER, 0x4633, 0, 0, &[(261, 1)]));
    assert!(
        find(&k, Kind::Sdt, 0x4633, 1).is_some_and(|e| !e.other),
        "[other] never drops [actual]"
    );
    assert!(find(&k, Kind::Sdt, 0x4633, 0).is_some_and(|e| e.other));
    // the real SDT[actual] of 0x4632 (one section) replaces both [other] sections
    let actual = sdt(TID_SDT, 0x4632, 0, 0, &[(251, 1), (252, 1)]);
    feed(&mut k, actual.clone());
    let e = find(&k, Kind::Sdt, 0x4632, 0).unwrap();
    assert!(!e.other && e.sec == actual);
    assert!(find(&k, Kind::Sdt, 0x4632, 1).is_none());
    // and an [other] copy never comes back over it
    feed(&mut k, sdt(TID_SDT_OTHER, 0x4632, 0, 0, &[(999, 1)]));
    assert_eq!(find(&k, Kind::Sdt, 0x4632, 0).unwrap().sec, actual);
}

#[test]
fn malformed_section_numbers_never_corrupt_the_cache() {
    let today = crate::today();
    let mut k = PsiKeeper::new();
    let a = sdt(TID_SDT, 0x4031, 0, 0, &[(1, 1)]);
    let b = sdt(TID_SDT, 0x4031, 0, 0, &[(2, 1)]);
    let rec = |id: u16, sub: u16, section: &[u8]| PsiRecord {
        kind: 4,
        other: false,
        id,
        sub,
        seen_day: today,
        section: section.to_vec(),
    };
    assert!(k.import(&rec(0x4000, 0, &a), today));
    // a record numbered past the section's own last_section_number is malformed:
    // ignored, also when it comes twice
    assert!(k.import(&rec(0x4031, 300, &a), today));
    assert!(k.import(&rec(0x4031, 300, &b), today));
    assert_eq!(k.cache.ent.len(), 1);
    assert!(find(&k, Kind::Sdt, 0x4031, 300).is_none());
    assert_eq!(find(&k, Kind::Sdt, 0x4000, 0).unwrap().sec, a);
    // from the stream: section_number 5 of last 2, twice with different content
    let t = Instant::now();
    k.begin(Some(0x4010), false, t);
    let bad1 = sdt(TID_SDT, 0x4040, 5, 2, &[(7, 1)]);
    let bad2 = sdt(TID_SDT, 0x4040, 5, 2, &[(8, 1)]);
    let mut cc = 0;
    observe(
        &mut k,
        &packetize(SDT_PID, &bad1, &mut cc, 0),
        &view(true),
        t,
    );
    observe(
        &mut k,
        &packetize(SDT_PID, &bad2, &mut cc, 0),
        &view(true),
        t,
    );
    // from the stream too: ignored, the cache untouched
    assert!(find(&k, Kind::Sdt, 0x4040, 5).is_none());
    assert_eq!(k.cache.ent.len(), 1);
}

#[test]
fn continuity_continues_the_device_and_restarts_on_another_ts() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4450), false, t);
    let mut cc = 7;
    observe(
        &mut k,
        &packetize(PAT_PID, &pat(0x4450, 1, &[(192, 0x0401)]), &mut cc, 0),
        &view(true),
        t,
    );
    let services: Vec<(u16, u8)> = (1..=40).map(|s| (s, 1)).collect();
    let long = sdt(TID_SDT, 0x4450, 0, 0, &services);
    assert!(long.len() > 2 * PACKET - 9, "three packets");
    cc = 14;
    observe(
        &mut k,
        &packetize(SDT_PID, &long, &mut cc, 0),
        &view(true),
        t,
    );
    // the device's SDT went out with cc 14, 15, 0
    let t2 = t + ms(10_000);
    k.begin(Some(0x4450), false, t2);
    let out = tick(&mut k, &view(false), t2 + ms(1000));
    assert_eq!(
        ccs(&out, PAT_PID),
        vec![8],
        "continues the device's PAT count"
    );
    assert_eq!(
        ccs(&out, SDT_PID),
        vec![1, 2, 3],
        "multi-packet re-send counts up"
    );
    let out = tick(&mut k, &view(false), t2 + ms(1500));
    assert_eq!(ccs(&out, PAT_PID), vec![9]);
    assert_eq!(ccs(&out, SDT_PID), vec![4, 5, 6]);
    // another TS: counters start afresh
    k.begin(Some(0x4451), false, t2);
    k.begin(Some(0x4450), false, t2);
    let out = tick(&mut k, &view(false), t2 + ms(1000));
    assert_eq!(ccs(&out, PAT_PID), vec![0]);
    assert_eq!(ccs(&out, SDT_PID), vec![0, 1, 2]);
    assert!(
        out.as_chunks::<PACKET>()
            .0
            .iter()
            .all(|p| packet::afc(p) == 1 && packet::tsc(p) == 0)
    );
}

#[test]
fn timing_of_a_delivering_stream() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    let today = crate::today();
    for (kind, id, sec) in [
        (4, 0x4031, sdt(TID_SDT, 0x4031, 0, 0, &[(101, 1)])),
        (3, 6, nit(6, 0, 0)),
    ] {
        assert!(k.import(
            &PsiRecord {
                kind,
                other: false,
                id,
                sub: 0,
                seen_day: today,
                section: sec
            },
            today
        ));
    }
    k.begin(Some(0x4031), false, t);
    let v = view(true);
    let at = |k: &mut PsiKeeper, n: u64| {
        let out = tick(k, &v, t + ms(n));
        (
            sent_sections(&out, PAT_PID).len(),
            sent_sections(&out, SDT_PID).len(),
            sent_sections(&out, NIT_PID).len(),
        )
    };
    assert_eq!(at(&mut k, 999), (0, 0, 0), "grace");
    assert_eq!(at(&mut k, 1500), (0, 0, 0), "PAT only after 1.5 s");
    assert_eq!(at(&mut k, 1501), (1, 0, 0));
    assert_eq!(at(&mut k, 2000), (0, 0, 0), "499 ms later");
    assert_eq!(at(&mut k, 2001), (1, 0, 0), "PAT every 500 ms");
    assert_eq!(at(&mut k, 3000), (1, 0, 0), "SDT only after 3 s");
    assert_eq!(at(&mut k, 3001), (0, 1, 0));
    assert_eq!(at(&mut k, 3500), (1, 0, 0));
    assert_eq!(at(&mut k, 4000), (1, 0, 0), "SDT 999 ms later: not yet");
    assert_eq!(at(&mut k, 4001), (0, 1, 0), "SDT every 1 s");
    assert_eq!(at(&mut k, 5000), (1, 0, 0), "NIT only after 5 s");
    assert_eq!(at(&mut k, 5001), (0, 1, 1));
    assert_eq!(at(&mut k, 7000), (1, 1, 0), "NIT 1999 ms later: not yet");
    assert_eq!(at(&mut k, 7001), (0, 0, 1), "NIT every 2 s");
    // the device's PAT arrives: no more PAT, and no more NIT (only while the PAT is
    // missing); the SDT is still missing
    let mut cc = 0;
    observe(
        &mut k,
        &packetize(PAT_PID, &pat(0x4031, 0, &[(101, 0x0101)]), &mut cc, 0),
        &v,
        t + ms(7100),
    );
    assert_eq!(at(&mut k, 10_000), (0, 1, 0));
    observe(
        &mut k,
        &packetize(
            SDT_PID,
            &sdt(TID_SDT, 0x4031, 0, 0, &[(101, 1)]),
            &mut cc,
            0,
        ),
        &v,
        t + ms(10_100),
    );
    assert_eq!(at(&mut k, 20_000), (0, 0, 0), "nothing missing");
    assert_eq!(k.sent.nit, 2);
    assert_eq!(
        k.end_summary().unwrap(),
        "psi: TS 4031 -- re-sent PAT x7, PMT x0, SDT x5, NIT x2 | cache: SDT of 1 TS(s) \
         (0 only via SDT[other]), 1 PAT(s), 0 PMT(s), 1 NIT section(s)"
    );
}

#[test]
fn timing_of_a_silent_stream_and_known_silent_start() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    let today = crate::today();
    let sec = sdt(TID_SDT, 0x4031, 0, 0, &[(101, 1)]);
    assert!(k.import(
        &PsiRecord {
            kind: 4,
            other: false,
            id: 0x4031,
            sub: 0,
            seen_day: today,
            section: sec
        },
        today
    ));
    let v = view(false);
    let at = |k: &mut PsiKeeper, n: u64| {
        let out = tick(k, &v, t + ms(n));
        (
            sent_sections(&out, PAT_PID).len(),
            sent_sections(&out, SDT_PID).len(),
        )
    };
    k.begin(Some(0x4031), false, t);
    assert_eq!(at(&mut k, 999), (0, 0), "grace even when silent");
    assert_eq!(at(&mut k, 1000), (1, 1), "silent: at once after the grace");
    assert_eq!(at(&mut k, 1399), (0, 0));
    assert_eq!(at(&mut k, 1400), (1, 0), "PAT every 400 ms");
    assert_eq!(at(&mut k, 1500), (0, 1), "SDT every 500 ms");
    // known silent: no grace
    k.begin(Some(0x4031), true, t);
    assert_eq!(at(&mut k, 0), (1, 1));
    // known silent but delivering after all: grace skipped, the delivering rules apply
    k.begin(Some(0x4031), true, t);
    assert!(tick(&mut k, &view(true), t + ms(10)).is_empty());
    assert!(!tick(&mut k, &view(true), t + ms(1501)).is_empty());
}

#[test]
fn pmt_resent_only_when_the_device_did_not_deliver_it() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4450), false, t);
    let mut cc = 0;
    let pat_sec = pat(0x4450, 1, &[(192, 0x0401), (193, 0x0402)]);
    observe(
        &mut k,
        &packetize(PAT_PID, &pat_sec, &mut cc, 0),
        &view(true),
        t,
    );
    observe(
        &mut k,
        &packetize(0x0401, &pmt(192, 1), &mut cc, 0),
        &view(true),
        t,
    );
    observe(
        &mut k,
        &packetize(0x0402, &pmt(193, 1), &mut cc, 0),
        &view(true),
        t,
    );
    observe(
        &mut k,
        &packetize(0x0403, &pmt(194, 1), &mut cc, 0),
        &view(true),
        t,
    );
    assert!(
        find(&k, Kind::Pmt, 0x4450, 194).is_none(),
        "an unlisted PMT PID is not watched"
    );
    // a PMT on the wrong PID, or of another program, is not cached
    observe(
        &mut k,
        &packetize(0x0402, &pmt(192, 2), &mut cc, 0),
        &view(true),
        t,
    );
    assert_eq!(find(&k, Kind::Pmt, 0x4450, 192).unwrap().sec, pmt(192, 1));

    let t2 = t + ms(10_000);
    k.begin(Some(0x4450), false, t2);
    // this time the device delivers PMT 192 but no PAT
    observe(
        &mut k,
        &packetize(0x0401, &pmt(192, 1), &mut cc, 0),
        &view(true),
        t2,
    );
    let out = tick(&mut k, &view(true), t2 + ms(1600));
    assert_eq!(sent_sections(&out, PAT_PID), vec![pat_sec]);
    assert!(
        sent_sections(&out, 0x0401).is_empty(),
        "delivered PMT not re-sent"
    );
    assert_eq!(sent_sections(&out, 0x0402), vec![pmt(193, 1)]);
    assert_eq!(k.sent.pmt, 1);
}

#[test]
fn reassembly_of_packed_sections_tails_and_losses() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4010), false, t);
    let a = make_sdt(TID_SDT_OTHER, 0x4601, 1, 10);
    let b = make_sdt(TID_SDT_OTHER, 0x4602, 2, 10);
    let c = make_sdt(TID_SDT_OTHER, 0x4603, 3, 200); // two packets
    let d = make_sdt(TID_SDT_OTHER, 0x4604, 4, 10);
    // packet 1: a, b, then the start of c; packet 2: rest of c (pointer_field) + d
    let mut p1 = vec![SYNC, 0x40, 0x11, 0x10, 0x00];
    p1.extend_from_slice(&a);
    p1.extend_from_slice(&b);
    let room = PACKET - p1.len();
    p1.extend_from_slice(&c[..room]);
    let rest = &c[room..];
    let mut p2 = vec![SYNC, 0x40, 0x11, 0x11, rest.len() as u8];
    p2.extend_from_slice(rest);
    p2.extend_from_slice(&d);
    p2.resize(PACKET, 0xFF);
    observe(&mut k, &[p1.clone(), p2.clone()].concat(), &view(true), t);
    for (id, sec) in [(0x4601, &a), (0x4602, &b), (0x4603, &c), (0x4604, &d)] {
        assert_eq!(
            &find(&k, Kind::Sdt, id, 0).expect("cached").sec,
            sec,
            "{id:04x}"
        );
    }
    // a lost packet (CC gap) drops the section in progress
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4010), false, t);
    let mut p2_gap = p2.clone();
    p2_gap[3] = 0x13;
    observe(&mut k, &[p1.clone(), p2_gap].concat(), &view(true), t);
    assert!(find(&k, Kind::Sdt, 0x4603, 0).is_none(), "CC gap: c lost");
    assert!(
        find(&k, Kind::Sdt, 0x4604, 0).is_some(),
        "d starts after the pointer_field"
    );
    // TEI packets are skipped entirely
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4010), false, t);
    let mut p1_tei = p1.clone();
    p1_tei[1] |= 0x80;
    observe(&mut k, &p1_tei, &view(true), t);
    assert!(k.cache.ent.is_empty());
}

#[test]
fn only_current_long_form_crc_valid_sections_count() {
    let t = Instant::now();
    let mut k = PsiKeeper::new();
    k.begin(Some(0x4031), false, t);
    let mut cc = 0;
    let mut next = pat(0x4031, 0, &[(101, 0x0101)]);
    next[5] &= !0x01; // current_next_indicator 0
    crc::set_section_crc(&mut next);
    let mut bad_crc = pat(0x4031, 0, &[(101, 0x0101)]);
    bad_crc[9] ^= 1;
    let mut multi = pat(0x4031, 0, &[(101, 0x0101)]);
    multi[7] = 1; // section 0 of 1: noted as a device PAT, not cached
    crc::set_section_crc(&mut multi);
    for sec in [&next, &bad_crc] {
        observe(&mut k, &packetize(PAT_PID, sec, &mut cc, 0), &view(true), t);
    }
    assert!(k.cache.ent.is_empty() && k.dev_pat.is_none());
    observe(
        &mut k,
        &packetize(PAT_PID, &multi, &mut cc, 0),
        &view(true),
        t,
    );
    assert!(k.cache.ent.is_empty() && k.dev_pat.is_some());
}

#[test]
fn owned_cs_stream_ticks_reduced_tables() {
    let t = Instant::now();
    let today = crate::today();
    let mut k = PsiKeeper::new();
    let sdt_sec = sdt(TID_SDT, 0x6080, 0, 0, &[(55, 1), (218, 1)]);
    let pat_sec = pat(0x6080, 1, &[(0, 0x10), (55, 0x040C), (218, 0x040D)]);
    for (kind, sec) in [(4u8, &sdt_sec), (1, &pat_sec)] {
        let rec = PsiRecord {
            kind,
            other: false,
            id: 0x6080,
            sub: 0,
            seen_day: today,
            section: sec.clone(),
        };
        assert!(k.import(&rec, today));
    }
    // silent CS stream of 218: the cached PAT / SDT go out reduced
    k.begin(Some(0x6080), false, t);
    let out = tick(&mut k, &tuned(218, false), t + ms(1000));
    assert_eq!(k.own, Some(218), "owned even without data (tick)");
    let pats = sent_sections(&out, PAT_PID);
    assert_eq!(pat::parse_pat(&pats[0]), vec![(218, 0x040D)]);
    let sdts = sent_sections(&out, SDT_PID);
    assert_eq!(
        sdt_services(&sdts[0])
            .map(|(_, s, _)| s)
            .collect::<Vec<_>>(),
        vec![218]
    );
    // every service of the TS (no program): not owned, not reduced
    k.begin(Some(0x6080), false, t);
    let v = StreamView {
        program: None,
        ..tuned(218, false)
    };
    let out = tick(&mut k, &v, t + ms(1000));
    assert!(k.own.is_none());
    assert_eq!(sent_sections(&out, PAT_PID), vec![pat_sec.clone()]);
    let mut cc = 0;
    let ts = packetize(PAT_PID, &pat_sec, &mut cc, 0);
    let (buf, out) = observe(&mut k, &ts, &v, t);
    assert!(
        buf == ts && out.is_empty(),
        "registering: device packets pass"
    );
}

#[test]
fn owned_stream_with_only_sdt_other_sends_it_relabelled_unreduced() {
    let t = Instant::now();
    let today = crate::today();
    let mut k = PsiKeeper::new();
    let other = sdt(TID_SDT_OTHER, 0x6080, 0, 0, &[(55, 1), (218, 1)]);
    let rec = PsiRecord {
        kind: 4,
        other: true,
        id: 0x6080,
        sub: 0,
        seen_day: today,
        section: other.clone(),
    };
    assert!(k.import(&rec, today));
    k.begin(Some(0x6080), false, t);
    let out = tick(&mut k, &tuned(218, false), t + ms(1000));
    let sdts = sent_sections(&out, SDT_PID);
    let mut want = other;
    want[0] = TID_SDT;
    crc::set_section_crc(&mut want);
    assert_eq!(
        sdts,
        vec![want],
        "never announce nothing: no [actual] lists 218"
    );
    // the synthesized PAT is reduced
    let pats = sent_sections(&out, PAT_PID);
    assert_eq!(pat::parse_pat(&pats[0]), vec![(218, SYN_PMT + 1)]);
}

#[test]
fn terrestrial_streams_pass_untouched() {
    let t = Instant::now();
    let today = crate::today();
    let sdt_sec = sdt(TID_SDT, 0x6080, 0, 0, &[(218, 1)]);
    let rec = PsiRecord {
        kind: 4,
        other: false,
        id: 0x6080,
        sub: 0,
        seen_day: today,
        section: sdt_sec.clone(),
    };
    let mut cc = 0;
    let ts = packetize(SDT_PID, &sdt_sec, &mut cc, 0);
    let mut k = PsiKeeper::new();
    assert!(k.import(&rec, today));
    for tsid in [None, Some(0)] {
        k.begin(tsid, true, t);
        let (buf, out) = observe(&mut k, &ts, &tuned(218, false), t);
        assert!(buf == ts && out.is_empty());
        assert!(tick(&mut k, &tuned(218, false), t + ms(10_000)).is_empty());
        assert!(k.end_summary().is_none());
    }
}

#[test]
fn cache_full_keeps_what_it_has() {
    let today = crate::today();
    let mut k = PsiKeeper::new();
    let sec = pmt(1, 0);
    for i in 0..MAX_ENT as u16 {
        let rec = PsiRecord {
            kind: 2,
            other: false,
            id: 0x4000,
            sub: i,
            seen_day: today,
            section: sec.clone(),
        };
        assert!(k.import(&rec, today));
    }
    let extra = PsiRecord {
        kind: 2,
        other: false,
        id: 0x4001,
        sub: 0,
        seen_day: today,
        section: sec.clone(),
    };
    assert!(k.import(&extra, today), "accepted ...");
    assert_eq!(k.cache.ent.len(), MAX_ENT, "... but not stored");
    assert!(k.cache.full_logged);
    let newer = PsiRecord {
        kind: 2,
        other: false,
        id: 0x4000,
        sub: 7,
        seen_day: today,
        section: pmt(1, 3),
    };
    assert!(k.import(&newer, today));
    assert_eq!(
        find(&k, Kind::Pmt, 0x4000, 7).unwrap().sec,
        pmt(1, 3),
        "replacing still works"
    );
}

#[test]
fn end_summary_counts_sdt_tses_and_other_only() {
    let t = Instant::now();
    let today = crate::today();
    let mut k = PsiKeeper::new();
    let recs = [
        (
            4,
            true,
            0x4632,
            0,
            sdt(TID_SDT_OTHER, 0x4632, 0, 0, &[(251, 1)]),
        ),
        (4, false, 0x4010, 0, sdt(TID_SDT, 0x4010, 0, 1, &[(151, 1)])),
        (4, false, 0x4010, 1, sdt(TID_SDT, 0x4010, 1, 1, &[(152, 1)])),
        (1, false, 0x4010, 0, pat(0x4010, 0, &[(151, 0x01F0)])),
        (2, false, 0x4010, 151, pmt(151, 0)),
        (3, false, 4, 0, nit(4, 0, 1)),
        (3, false, 4, 1, nit(4, 1, 1)),
    ];
    for (kind, other, id, sub, section) in recs {
        assert!(k.import(
            &PsiRecord {
                kind,
                other,
                id,
                sub,
                seen_day: today,
                section
            },
            today
        ));
    }
    k.begin(Some(0x4632), false, t);
    tick(&mut k, &view(false), t + ms(1000));
    assert_eq!(
        k.end_summary().unwrap(),
        "psi: TS 4632 -- re-sent PAT x1, PMT x0, SDT x1, NIT x0 | cache: SDT of 2 TS(s) \
         (1 only via SDT[other]), 1 PAT(s), 1 PMT(s), 2 NIT section(s)"
    );
}

#[test]
fn nit_follows_the_sdt_network_in_section_order() {
    let t = Instant::now();
    let today = crate::today();
    let mut k = PsiKeeper::new();
    let n0 = nit(6, 0, 1);
    let n1 = nit(6, 1, 1);
    let other_net = nit(7, 0, 0);
    for (id, sub, sec) in [(6, 1, n1.clone()), (7, 0, other_net), (6, 0, n0.clone())] {
        assert!(k.import(
            &PsiRecord {
                kind: 3,
                other: false,
                id,
                sub,
                seen_day: today,
                section: sec
            },
            today
        ));
    }
    let sdt_sec = sdt(TID_SDT, 0x4031, 0, 0, &[(101, 1)]); // onid 6
    assert!(k.import(
        &PsiRecord {
            kind: 4,
            other: false,
            id: 0x4031,
            sub: 0,
            seen_day: today,
            section: sdt_sec
        },
        today
    ));
    k.begin(Some(0x4031), false, t);
    let out = tick(&mut k, &view(true), t + ms(5001));
    assert_eq!(sent_sections(&out, NIT_PID), vec![n0, n1]);
    assert_eq!(ccs(&out, NIT_PID), vec![0, 1]);
}

/// Random packets and random CRC-valid sections through every entry point: nothing
/// may panic, and the output is always whole packets.
#[test]
fn no_panic_on_malformed_input() {
    let mut rng = Rng(0x00DD_2950);
    let t = Instant::now();
    let today = crate::today();
    let pids_of_interest = [0x0000, 0x0010, 0x0011, 0x0401, 0x1F00, 0x1FFF, 0x0123];
    let random_section = |rng: &mut Rng| {
        let tid = [TID_PAT, TID_PMT, TID_NIT, TID_SDT, TID_SDT_OTHER][rng.below(5)];
        let len = 12 + rng.below(SEC_MAX - 11);
        let mut sec = vec![0u8; len];
        rng.fill(&mut sec);
        sec[0] = tid;
        let sl = len - 3;
        sec[1] = 0x80 | (sec[1] & 0x70) | (sl >> 8) as u8;
        sec[2] = sl as u8;
        if rng.below(4) > 0 {
            sec[3] = 0x60;
            sec[4] = 0x80; // the tuned TS
        }
        sec[5] |= 0x01;
        if rng.below(2) == 0 {
            sec[6] = 0;
            sec[7] = 0;
        }
        crc::set_section_crc(&mut sec);
        sec
    };
    for round in 0..300 {
        let mut k = PsiKeeper::new();
        for _ in 0..rng.below(20) {
            let sec = random_section(&mut rng);
            let rec = PsiRecord {
                kind: rng.below(6) as u8,
                other: rng.below(2) == 0,
                id: [0x6080, 0x0006, rng.byte().into()][rng.below(3)],
                sub: rng.below(300) as u16,
                seen_day: today,
                section: sec,
            };
            k.import(&rec, today);
        }
        let tsid = [0x6080, 0x4031][round % 2];
        k.begin(Some(tsid), rng.below(2) == 0, t);
        let mut now = t;
        for _ in 0..20 {
            let v = StreamView {
                program: [None, Some(218), Some(rng.byte().into())][rng.below(3)],
                delivered_any: rng.below(2) == 0,
                undecryptable: rng.below(8) == 0,
            };
            let mut ts = Vec::new();
            for _ in 0..rng.below(12) {
                let pid = pids_of_interest[rng.below(pids_of_interest.len())];
                if rng.below(3) == 0 {
                    let sec = random_section(&mut rng);
                    let mut cc = rng.byte();
                    ts.extend(packetize(pid, &sec, &mut cc, rng.below(4)));
                } else {
                    let mut p = [0u8; PACKET];
                    rng.fill(&mut p);
                    p[0] = SYNC;
                    p[1] = (p[1] & 0xE0) | (pid >> 8) as u8;
                    p[2] = pid as u8;
                    ts.extend_from_slice(&p);
                }
            }
            if rng.below(4) == 0 {
                let cut = rng.below(ts.len() + 1);
                ts.truncate(cut);
            }
            let mut out = Vec::new();
            k.observe(&mut ts, &v, now, &mut out);
            now += ms(rng.below(1500) as u64);
            k.tick(&v, now, &mut out);
            assert_eq!(out.len() % PACKET, 0);
            assert!(out.as_chunks::<PACKET>().0.iter().all(|p| p[0] == SYNC));
        }
        let _ = k.end_summary();
        let recs = k.export();
        let mut r = PsiKeeper::new();
        for rec in &recs {
            r.import(rec, today);
        }
    }
}
