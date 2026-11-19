//! The text of a setting file as Windows programs read it: UTF-8 (with or without BOM),
//! UTF-16LE with BOM, or else the ANSI code page (Shift_JIS off Windows).

/// The text of a file: UTF-8 (with or without BOM), UTF-16LE with BOM, or else the ANSI
/// code page (what `GetPrivateProfileStringW` assumes for a BOM-less file; off Windows,
/// Shift_JIS, the code page of a Japanese Windows).
pub fn decode(bytes: &[u8]) -> String {
    decode_or(bytes, decode_ansi)
}

/// [`decode`], but bytes that are not UTF-8 are CP932 whatever the ANSI code page, as
/// the PT drivers read a ChSet.txt.
pub fn decode_cp932(bytes: &[u8]) -> String {
    decode_or(bytes, decode_shift_jis)
}

fn decode_or(bytes: &[u8], other: fn(&[u8]) -> String) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return String::from_utf16le_lossy(rest);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => other(bytes),
    }
}

/// Bytes in the system ANSI code page (CP932 on a Japanese Windows).
#[cfg(windows)]
fn decode_ansi(bytes: &[u8]) -> String {
    decode_code_page(bytes, windows_sys::Win32::Globalization::CP_ACP)
}

#[cfg(windows)]
fn decode_shift_jis(bytes: &[u8]) -> String {
    decode_code_page(bytes, 932)
}

#[cfg(windows)]
fn decode_code_page(bytes: &[u8], code_page: u32) -> String {
    use std::ptr;
    use windows_sys::Win32::Globalization::MultiByteToWideChar;

    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    if len == 0 {
        return String::new();
    }
    // SAFETY: the input is `len` readable bytes; a null output with size 0
    // only asks for the required length.
    let need =
        unsafe { MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, ptr::null_mut(), 0) };
    let Ok(cap) = usize::try_from(need) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    let mut w = vec![0u16; cap];
    // SAFETY: as above, and `w` is writable for `need` UTF-16 units.
    let got =
        unsafe { MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, w.as_mut_ptr(), need) };
    match usize::try_from(got) {
        Ok(n) if n > 0 => String::from_utf16_lossy(&w[..n]),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// No ANSI code page off Windows: Shift_JIS as CP932 has it (the files come from a
/// Japanese Windows), invalid bytes replaced.
#[cfg(not(windows))]
fn decode_ansi(bytes: &[u8]) -> String {
    decode_shift_jis(bytes)
}

#[cfg(not(windows))]
fn decode_shift_jis(bytes: &[u8]) -> String {
    encoding_rs::SHIFT_JIS
        .decode_without_bom_handling(bytes)
        .0
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding() {
        assert_eq!(decode(b"\xEF\xBB\xBF[Tuner]"), "[Tuner]");
        assert_eq!(decode(b"\xFF\xFE[\0T\0]\0"), "[T]");
        assert_eq!(decode("[BS]\nあ=1".as_bytes()), "[BS]\nあ=1");
        // Not UTF-8: decoded with the ANSI code page (never fails).
        assert!(decode(b"[BS]\n\x82\xa0=1").starts_with("[BS]\n"));
        assert_eq!(decode_ansi(b"[Tuner]\r\nUHF=1"), "[Tuner]\r\nUHF=1");
        assert_eq!(decode_ansi(b""), "");
        // CP932 whatever the ANSI code page; Unicode as decode reads it.
        assert_eq!(decode_cp932(b"$\x82\xa0\t0"), "$あ\t0");
        assert_eq!(decode_cp932("$あ\t0".as_bytes()), "$あ\t0");
        assert_eq!(decode_cp932(b"\xFF\xFE$\0"), "$");
    }

    #[cfg(not(windows))]
    #[test]
    fn shift_jis_off_windows() {
        // "あ.state", and a CP932 extension (NEC row 13: a circled 1)
        assert_eq!(decode(b"StateFile=\x82\xA0.state"), "StateFile=あ.state");
        assert_eq!(decode(b"\x87\x40"), "\u{2460}");
        assert_eq!(decode(b"\xFF"), "\u{FFFD}", "not Shift_JIS");
    }
}
