use super::*;

fn spaces(text: &str) -> Vec<Space> {
    parse(&profile::parse(text).expect("parses"))
}

fn names(s: &Space) -> Vec<&str> {
    s.channels.iter().map(|c| c.name.as_str()).collect()
}

fn sat(khz: u32, ts: Ts) -> Tuning {
    Tuning::Satellite {
        if_hz: khz * 1000,
        ts,
    }
}

fn ter(khz: u32) -> Tuning {
    Tuning::Terrestrial { hz: khz * 1000 }
}

// In the form of BonDriver_BDA's BS / CS110 sample.
const BSCS: &str = "[Tuner]\r\n\
    Name=\"ISDB-S\"\r\n\
    DefaultNetwork=\"BS/CS110\"\r\n\
    [Channel]\r\n\
    ReserveUnusedCh=NO\r\n\
    [TuningSpace00]\r\n\
    TuningSpaceName=\"BS\"\r\n\
    CH000=1,11727.480,R,0,BS01/TS0,0x0,0x4010\r\n\
    CH001=1,11727.480,R,0,BS01/TS1,0x0,0x4011\r\n\
    CH013=1,11996.000,R,0,BS15/TS0,0x0,0x40f1\r\n\
    CH025=1,12149.440,R,0,BS23/TS2,0x0,0x4973,ＢＳ松竹東急\r\n\
    [TuningSpace01]\r\n\
    TuningSpaceName=\"CS110\"\r\n\
    CH000=1,12291.000,R,0,ND02/TS0,0x0,0x0\r\n\
    CH011=1,12731.000,R,0,ND24/TS0,0x0,0x0\r\n";

#[test]
fn bs_cs110_lines() {
    let ini = profile::parse(BSCS).unwrap();
    assert!(is_bda(&ini));
    let s = parse(&ini);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].name, "BS");
    // ReserveUnusedCh=NO: in file order, whatever the numbers
    assert_eq!(
        names(&s[0]),
        ["BS01/TS0", "BS01/TS1", "BS15/TS0", "BS23/TS2"]
    );
    let t: Vec<_> = s[0].channels.iter().map(|c| c.tuning.clone()).collect();
    assert_eq!(
        t,
        [
            sat(1_049_480, Ts::Id(0x4010)),
            sat(1_049_480, Ts::Id(0x4011)),
            sat(1_318_000, Ts::Id(0x40F1)),
            sat(1_471_440, Ts::Id(0x4973)),
        ]
    );
    assert_eq!(s[1].name, "CS110");
    assert_eq!(s[1].channels[0].tuning, sat(1_613_000, Ts::Number(0)));
    assert_eq!(s[1].channels[1].tuning, sat(2_053_000, Ts::Number(0)));
}

// In the form of BonDriver_BDA's UHF sample, with its 53ch line and CATV taken in.
const UHF: &str = "[Tuner]\r\n\
    DefaultNetwork=\"UHF/CATV\"\r\n\
    [Channel]\r\n\
    ReserveUnusedCh=NO\r\n\
    [TuningSpace00]\r\n\
    TuningSpaceName=\"UHF\"\r\n\
    ChannelSettingsAuto=\"UHF\"\r\n\
    CH041=0,713.143, ,0,53ch\r\n\
    [TuningSpace01]\r\n\
    TuningSpaceName=\"CATV\"\r\n\
    ChannelSettingsAuto=\"CATV\"\r\n";

#[test]
fn generated_terrestrial_channels() {
    let s = spaces(UHF);
    assert_eq!(s.len(), 2);
    let uhf = &s[0];
    // UHF:0:40 = 13-52ch; a line after generated channels goes by its number
    assert_eq!(uhf.channels.len(), 42);
    assert_eq!(uhf.channels[0].name, "13ch");
    assert_eq!(uhf.channels[0].tuning, ter(473_143));
    assert_eq!(uhf.channels[39].name, "52ch");
    assert_eq!(uhf.channels[40].name, "----");
    assert!(matches!(uhf.channels[40].tuning, Tuning::Untunable(_)));
    assert_eq!(uhf.channels[41].name, "53ch");
    assert_eq!(uhf.channels[41].tuning, ter(713_143));
    let catv = &s[1];
    assert_eq!(catv.channels.len(), 10 + 41);
    assert_eq!(catv.channels[0].name, "C13ch");
    assert_eq!(catv.channels[9].name, "C22ch");
    assert_eq!(catv.channels[9].tuning, ter(165_143), "C22 at 165 MHz");
    assert_eq!(catv.channels[50].name, "C63ch");
    assert_eq!(catv.channels[50].tuning, ter(465_143));
}

#[test]
fn generated_satellite_channels() {
    let s = spaces(
        "[Tuner]\nDefaultNetwork=\"BS/CS110\"\n\
         [TuningSpace00]\nTuningSpaceName=BS\nChannelSettingsAuto=BS\n\
         [TuningSpace01]\nTuningSpaceName=CS110\nChannelSettingsAuto=CS110\n\
         [TuningSpace02]\nChannelSettingsAuto=BS\nChannelSettingsAutoOptions=RelativeTS:2\n",
    );
    let bs = &s[0];
    assert_eq!(bs.channels.len(), 12 * 4, "4 TSs each");
    assert_eq!(bs.channels[0].name, "BS01/TS0");
    assert_eq!(bs.channels[0].tuning, sat(1_049_480, Ts::Number(0)));
    assert_eq!(bs.channels[29].name, "BS15/TS1");
    assert_eq!(bs.channels[29].tuning, sat(1_318_000, Ts::Number(1)));
    assert_eq!(bs.channels[47].name, "BS23/TS3");
    let cs = &s[1];
    assert_eq!(cs.channels.len(), 12);
    assert_eq!(cs.channels[0].name, "ND02/TS0");
    assert_eq!(cs.channels[0].tuning, sat(1_613_000, Ts::First));
    assert_eq!(cs.channels[11].name, "ND24/TS0");
    assert_eq!(s[2].name, "NoName");
    assert_eq!(s[2].channels.len(), 12 * 2);
}

#[test]
fn numbering() {
    // ReserveUnusedCh: by number, gaps listed; a space number without a section is "-"
    let s = spaces(
        "[Tuner]\nDefaultNetwork=\"UHF/CATV\"\n\
         [TuningSpace00]\nReserveUnusedCh=YES\n\
         CH000=0,473.143, ,0,13ch\nCH002=0,485.143, ,0,15ch\n\
         [TuningSpace02]\nTuningSpaceName=two\nCH005=0,557.143, ,0,27ch\n",
    );
    assert_eq!(s.len(), 3);
    assert_eq!(s[0].name, "スカパー", "space 0's default name");
    assert_eq!(names(&s[0]), ["13ch", "----", "15ch"]);
    assert_eq!((s[1].name.as_str(), s[1].channels.len()), ("-", 0));
    assert_eq!(names(&s[2]), ["27ch"]);
    // ReserveUnusedCh in [Channel] is every space's default
    let s = spaces(
        "[Tuner]\nDefaultNetwork=UHF\n[Channel]\nReserveUnusedCh=YES\n\
         [TuningSpace01]\nCH001=0,473.143, ,0,13ch\n",
    );
    assert_eq!(names(&s[1]), ["----", "13ch"]);
    assert_eq!(s[0].channels.len(), 0, "space 0 from an empty [Channel]");
}

#[test]
fn channel_section_is_space_0() {
    let s = spaces(
        "[Tuner]\nDefaultNetwork=UHF\nTuningSpaceName=地デジ\n\
         [Channel]\nCH000=0,473.143, ,0,13ch\n",
    );
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].name, "地デジ");
    assert_eq!(names(&s[0]), ["13ch"]);
}

#[test]
fn sphd_default() {
    // No DefaultNetwork: SPHD. Modulation 0 is DVB-S, and space 0 without channels gets
    // three DVB-S transponders.
    let s = spaces("[TuningSpace01]\nCH000=1,12658,V,0,x\n");
    assert_eq!(s[0].channels.len(), 3);
    assert!(
        s.iter()
            .flat_map(|s| &s.channels)
            .all(|c| matches!(c.tuning, Tuning::Untunable(_)))
    );
}

#[test]
fn modulations_lnbs_and_offsets() {
    let s = spaces(
        "[Tuner]\nDefaultNetwork=NONE\n\
         [Modulation]\nModulationType0Modulation=BDA_MOD_ISDB_S_TMCC\n\
         ModulationType1SettingsAuto=ISDB-T\nModulationType2SettingsAuto=J.83C-64QAM\n\
         [Satellite]\nSatellite2Oscillator=10678000\nSatellite3SettingsAuto=BS/CS110\n\
         [TuningSpace00]\n\
         CH000=2,11727.480,R,0,LO given\n\
         CH001=0,1049.480, ,0,IF given\n\
         CH002=3,11727.480,L,0,left-hand\n\
         CH003=0,557.143, ,1,27ch\n\
         CH004=0,557.143, ,2,QAM\n\
         CH005=0,11727.480, ,0,no LNB\n\
         CH006=0,11727.480,R,0,x,0x0,0x4010,4\n\
         [TuningSpace01]\nFrequencyOffset=-10678000\n\
         CH000=0,11727.480, ,0,offset\n",
    );
    let t: Vec<_> = s[0].channels.iter().map(|c| c.tuning.clone()).collect();
    assert_eq!(t[0], sat(1_049_480, Ts::First));
    assert_eq!(t[1], sat(1_049_480, Ts::First));
    assert!(matches!(&t[2], Tuning::Untunable(w) if w.contains("2222.480")));
    assert_eq!(t[3], ter(557_143));
    assert!(matches!(&t[4], Tuning::Untunable(w) if w.contains("64QAM")));
    assert_eq!(t[5], sat(1_049_480, Ts::First), "BS / CS110's LNB");
    assert_eq!(t[6], sat(1_049_480, Ts::Id(0x4010)), "the TSID field");
    assert_eq!(s[1].channels[0].tuning, sat(1_049_480, Ts::First));
}

#[test]
fn channel_lines() {
    let s = spaces(
        "[Tuner]\nDefaultNetwork=\"BS/CS110\"\n[TuningSpace00]\n\
         CH000=1,11727.480,R,0\n\
         CH=1,11996,R,0,bare CH\n\
         CH00=1,11996,R,0,not a channel key\n\
         CH001=\n\
         ch002=1,12149.44,R,0,lower case\n",
    );
    // by key: CH before CH000
    assert_eq!(
        names(&s[0]),
        ["bare CH", "BS/CS110/11727.480R/ISDB-S", "lower case"]
    );
    assert_eq!(s[0].channels[0].tuning, sat(1_318_000, Ts::First));
    assert_eq!(s[0].channels[2].tuning, sat(1_471_440, Ts::First));
}

#[test]
fn channel_lines_by_key() {
    // packed in the order of their keys, a repeated key in file order; quotes removed
    let s = spaces(
        "[Tuner]\nDefaultNetwork=\"BS/CS110\"\n[TuningSpace00]\n\
         CH010=1,12149.44,R,0,ten\n\
         CH002=1,11727.48,R,0,two\n\
         ch010=1,11996,R,0,ten again\n\
         CH005=\"1,11996,R,0,quoted\"\n\
         CH006=''\n",
    );
    assert_eq!(names(&s[0]), ["two", "quoted", "ten", "ten again"]);
    assert_eq!(s[0].channels[1].tuning, sat(1_318_000, Ts::First));
    assert_eq!(section_value(" \"a\"b\" "), "a\"b");
    assert_eq!(section_value("'a"), "a");
}

#[test]
fn reserve_unused_ch_values() {
    let s = |global: &str, local: &str| {
        let s = spaces(&format!(
            "[Tuner]\nDefaultNetwork=UHF\n[Channel]\nReserveUnusedCh={global}\n\
             [TuningSpace00]\nReserveUnusedCh={local}\nCH001=0,473.143, ,0,13ch\n"
        ));
        s[0].channels.len()
    };
    // YES / TRUE / ON or a number other than 0; other text is 0 (not the default)
    assert_eq!(s("NO", "on"), 2);
    assert_eq!(s("NO", "2"), 2);
    assert_eq!(s("YES", "x"), 1);
    assert_eq!(s("YES", "Y"), 1);
    assert_eq!(s("YES", ""), 2, "empty: the [Channel] one");
    assert_eq!(s("Y", ""), 1);
}

#[test]
fn channel_numbers_bounded() {
    // a start or a relative TS count far too high lists no more than MAX_CHANNELS
    let s =
        spaces("[Tuner]\nDefaultNetwork=UHF\n[TuningSpace00]\nChannelGenerate0=UHF,0,,400000000\n");
    assert!(s[0].channels.is_empty());
    let s = spaces(
        "[Tuner]\nDefaultNetwork=UHF\n[TuningSpace00]\nChannelGenerate0=UHF:0:40:100000,0\n",
    );
    assert_eq!(s[0].channels.len(), MAX_CHANNELS as usize);
}

#[test]
fn channel_generate_lines() {
    let s = spaces(
        "[Tuner]\nDefaultNetwork=UHF\n[TuningSpace00]\nChannelSettingsAuto=UHF\n\
         ChannelGenerate0=UHF:2:3,0,,,143\n\
         ChannelGenerate1=CATV-L:0:2,0,,10,0,CATV %d,13\n\
         ChannelGenerate3=UHF,0,,,143\n",
    );
    // ChannelGenerate0 replaces the UHF line ChannelSettingsAuto stands for; the list
    // ends at the first missing number
    assert_eq!(
        names(&s[0]),
        [
            "15ch", "16ch", "17ch", "----", "----", "----", "----", "----", "----", "----",
            "CATV 13", "CATV 14"
        ]
    );
    assert_eq!(s[0].channels[0].tuning, ter(485_143));
    assert_eq!(s[0].channels[10].tuning, ter(111_000));
}

#[test]
fn name_formats() {
    assert_eq!(printf("BS%02d/TS%d", &[1, 0]), "BS01/TS0");
    assert_eq!(printf("H%03d+", &[11]), "H011+");
    assert_eq!(printf("%dch", &[13]), "13ch");
    assert_eq!(printf("100%% %ld", &[5]), "100% 5");
    assert_eq!(printf("%d/%d/%d", &[1, 2]), "1/2/0", "missing values are 0");
    assert_eq!(printf("%s", &[1]), "%s");
    // C's unsigned values: %d prints them signed, %u not
    assert_eq!(
        printf("%dch %u", &[-12i32 as u32, -1i32 as u32]),
        "-12ch 4294967295"
    );
    // a width past MAX_WIDTH pads to it (Rust's formatting takes 16 bits at most)
    assert_eq!(printf("%70000d", &[1]).len(), MAX_WIDTH);
}

#[test]
fn numbers_past_the_tables() {
    // BonDriver_BDA does not check a ChannelGenerate line's modulation and satellite
    // numbers: such a channel is listed (the numbering holds) but does not tune
    let untunable = |s: &Space, i: usize, what: &str| matches!(&s.channels[i].tuning, Tuning::Untunable(w) if w.contains(what));
    let s = spaces("[Tuner]\nDefaultNetwork=UHF\n[TuningSpace00]\nChannelGenerate0=UHF:0:2,12\n");
    assert_eq!(names(&s[0]), ["13ch", "14ch"]);
    assert!(untunable(&s[0], 0, "modulation number 12"));
    let s = spaces("[Tuner]\nDefaultNetwork=UHF\n[TuningSpace00]\nChannelGenerate0=UHF:0:1,-1\n");
    assert!(untunable(&s[0], 0, "modulation number -1"));
    let s = spaces(
        "[Tuner]\nDefaultNetwork=BS/CS110\n[TuningSpace00]\nChannelGenerate0=BS1:0:1,0,15\n",
    );
    assert!(untunable(&s[0], 0, "satellite number 15"));
    // a CHnnn line without a name over such a channel: the automatic name, without it
    let s = spaces(
        "[Tuner]\nDefaultNetwork=UHF\n[TuningSpace00]\nChannelGenerate0=UHF:0:1,0,15\n\
         CH000=99,473.143, ,0\n",
    );
    assert_eq!(names(&s[0]), ["/00473.143 /ISDB-T"]);
    assert_eq!(
        s[0].channels[0].tuning,
        ter(473_143),
        "ISDB-T has no satellite"
    );
}

#[test]
fn wrapping_numbers() {
    // C's unsigned arithmetic wraps where Rust's would panic (in a debug build)
    let s = spaces(
        "[Tuner]\nDefaultNetwork=BS/CS110\n[TuningSpace00]\n\
         ChannelGenerate0=BS1:0:1:2,0,1,,,,,,-1\n",
    );
    assert_eq!(names(&s[0]), ["BS01/TS-1", "BS01/TS0"]);
    let s = spaces(
        "[Tuner]\nDefaultNetwork=UHF\n[TuningSpace00]\nChannelSettingsAuto=TRANSMODULATION\n\
         ChannelSettingsAutoOptions=RELATIVETS-64QAM:99999999\n",
    );
    assert_eq!(s[0].channels.len(), MAX_CHANNELS as usize);
}

#[test]
fn values_as_section_data() {
    // a leading quote is removed with everything from the last one on, so a comment
    // after a quoted value goes too (BonDriver_BDA's section lines, not the profile API)
    let s = spaces(
        "[Tuner]\nDefaultNetwork=\"UHF\" ; terrestrial\n[TuningSpace00]\n\
         TuningSpaceName=\"UHF\" ; the name\nChannelSettingsAuto=\"UHF\" ; auto\n\
         [TuningSpace01]\nTuningSpaceName=\"CATV\n",
    );
    assert_eq!(s[0].name, "UHF");
    assert_eq!(s[0].channels.len(), 40);
    assert_eq!(s[0].channels[0].tuning, ter(473_143));
    assert_eq!(s[1].name, "CATV", "one quote: removed");
}

#[test]
fn modulation_and_network_values() {
    let first = |text: &str| spaces(text)[0].channels[0].tuning.clone();
    let bs1 = "[TuningSpace00]\nCH000=1,11727.480,R,0,BS01/TS0\n";
    // an empty ModulationTypeNModulation keeps the kind DefaultNetwork set up
    let t = first(&format!(
        "[Tuner]\nDefaultNetwork=BS/CS110\n[Modulation]\nModulationType0Modulation=\n{bs1}"
    ));
    assert_eq!(t, sat(1_049_480, Ts::First));
    // a number, in the C syntax: 0x23 = 35 = BDA_MOD_ISDB_S_TMCC
    let t = first(&format!(
        "[Tuner]\nDefaultNetwork=NONE\n[Modulation]\nModulationType0Modulation=0x23\n\
         [Satellite]\nSatellite1SettingsAuto=BS/CS110\n{bs1}"
    ));
    assert_eq!(t, sat(1_049_480, Ts::First));
    // DefaultNetwork as a number; text that is none is 0, NONE
    let t = first(&format!("[Tuner]\nDefaultNetwork=0x2\n{bs1}"));
    assert_eq!(t, sat(1_049_480, Ts::First));
    let s = spaces("[Tuner]\nDefaultNetwork=ISDB-S\n[TuningSpace01]\nCH000=1,11727.480,R,0\n");
    assert!(s[0].channels.is_empty(), "no SPHD channels for NONE");
}

#[test]
fn frequencies() {
    assert_eq!(mhz_as_khz("11727.480"), Some(11_727_480));
    assert_eq!(mhz_as_khz(" 713.143"), Some(713_143));
    assert_eq!(mhz_as_khz("12658"), Some(12_658_000));
    assert_eq!(mhz_as_khz("12149.44x"), Some(12_149_440));
    assert_eq!(mhz_as_khz("x"), None);
}
