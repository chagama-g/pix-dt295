//! An ini file as the Win32 profile API reads it (`GetPrivateProfileStringW` and
//! friends), with `rust-ini`: the text's encoding ([`decode`]), the lines the API would
//! see, and its integer syntax. The BonDriver reads `BonDriver_DT295.ini` with it, and
//! the `dt295` command reads the same file the same way. Section and key names are
//! case-insensitive; backslashes are literal (escapes off); of a section given twice
//! the first one counts, of a key given twice its first value.

use ini::ParseOption;
pub use ini::{Ini, ParseError};

pub use crate::text::decode;

/// Stand-ins for characters `rust-ini` would treat specially inside a key (`:` is
/// also a key/value separator there; `\` before the separator would swallow it).
const KEY_STANDINS: [(char, char); 2] = [(':', '\u{E03A}'), ('\\', '\u{E05C}')];

fn protect_key(k: &str) -> String {
    KEY_STANDINS
        .iter()
        .fold(k.to_owned(), |s, &(orig, standin)| {
            s.replace(orig, &standin.to_string())
        })
}

fn restore_key(k: &str) -> String {
    KEY_STANDINS
        .iter()
        .fold(k.to_owned(), |s, &(orig, standin)| {
            s.replace(standin, &orig.to_string())
        })
}

/// Parse ini text with `rust-ini` (escapes and quotes off) after normalising it to
/// the lines the Win32 profile API would see: blank, `;` / `#` comment and
/// separator-less lines dropped, `[section]` headers cut after the `]`, keys
/// protected (see `KEY_STANDINS`; [`lines`] gives them back as written), and no
/// backslash line continuation. A leading UTF-8 BOM is stripped.
pub fn parse(text: &str) -> Result<Ini, ParseError> {
    let text = text.trim_prefix('\u{FEFF}');
    let mut norm = String::with_capacity(text.len());
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if line.starts_with('[') {
            match line.find(']') {
                Some(end) => norm.push_str(&line[..=end]),
                None => continue, // not a section header
            }
        } else {
            let Some(eq) = line.find('=').filter(|&eq| eq > 0) else {
                continue; // no key=value
            };
            norm.push_str(&protect_key(&line[..eq]));
            norm.push_str(&line[eq..]);
            if line.ends_with('\\') {
                norm.push(' '); // a trailing '\' would continue the value on the next line
            }
        }
        norm.push('\n');
    }
    Ini::load_from_str_opt(
        &norm,
        ParseOption {
            enabled_quote: false,
            enabled_escape: false,
            enabled_indented_mutiline_value: false,
            enabled_preserve_key_leading_whitespace: false,
        },
    )
}

/// `GetPrivateProfileStringW(section, key, ...)`: the first value, with one pair of
/// enclosing quotes removed. `None` if the key is absent (an empty value is `Some("")`).
pub fn get<'a>(ini: &'a Ini, section: &str, key: &str) -> Option<&'a str> {
    ini.get_from(Some(section), &protect_key(key)).map(unquote)
}

/// `GetPrivateProfileIntW(section, key, default)`: absent or empty = `default`, else
/// [`int`] of the value.
pub fn get_int(ini: &Ini, section: &str, key: &str, default: u32) -> u32 {
    match get(ini, section, key) {
        None | Some("") => default,
        Some(s) => int(s),
    }
}

/// The `key=value` lines of a section, in file order (repeated keys too), keys as
/// written. Empty if the section is absent.
pub fn lines<'a>(ini: &'a Ini, section: &str) -> Vec<(String, &'a str)> {
    ini.section(Some(section))
        .map(|props| props.iter().map(|(k, v)| (restore_key(k), v)).collect())
        .unwrap_or_default()
}

fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// The integer syntax of `GetPrivateProfileIntW` (`RtlUnicodeStringToInteger`, base
/// 0): leading blanks, a sign, `0x` / `0o` / `0b` prefixes, digits up to the first
/// other character (none = 0), modulo 2^32.
pub fn int(s: &str) -> u32 {
    let t = s.trim_start_matches(|c: char| c <= ' ');
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let (radix, digits) = match t.as_bytes() {
        [b'0', b'x' | b'X', ..] => (16, &t[2..]),
        [b'0', b'o' | b'O', ..] => (8, &t[2..]),
        [b'0', b'b' | b'B', ..] => (2, &t[2..]),
        _ => (10, t),
    };
    let v = digits
        .chars()
        .map_while(|c| c.to_digit(radix))
        .fold(0u32, |acc, d| acc.wrapping_mul(radix).wrapping_add(d));
    if neg { v.wrapping_neg() } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        let ini = parse(
            "\u{FEFF}; comment\r\n\
             [tuner]  ; after the header\r\n\
             firmware = D:\\fw\\idle.bin\r\n\
             Empty=\r\n\
             Quoted=\"C:\\a b\"\r\n\
             Twice=1\r\n\
             TWICE=2\r\n\
             Number=0x10\r\n\
             [Tuner]\r\n\
             Later=1\r\n",
        )
        .unwrap();
        assert_eq!(get(&ini, "Tuner", "Firmware"), Some("D:\\fw\\idle.bin"));
        assert_eq!(get(&ini, "TUNER", "empty"), Some(""));
        assert_eq!(get(&ini, "Tuner", "Quoted"), Some("C:\\a b"));
        assert_eq!(get(&ini, "Tuner", "Twice"), Some("1"), "the first value");
        assert_eq!(get(&ini, "Tuner", "Later"), None, "only the first [Tuner]");
        assert_eq!(get(&ini, "Tuner", "Absent"), None);
        assert_eq!(get_int(&ini, "Tuner", "Number", 7), 16);
        assert_eq!(get_int(&ini, "Tuner", "Empty", 7), 7, "empty = the default");
        assert_eq!(get_int(&ini, "Tuner", "Absent", 7), 7);
        assert_eq!(get_int(&ini, "Tuner", "Firmware", 7), 0, "not a number");
    }

    #[test]
    fn section_lines() {
        let ini = parse(
            "[BS]\n\
             a stray line\n\
             =no key\n\
             J:COM BS=1471440,0x4973\n\
             back\\slash=1\n\
             dup=1\n\
             dup=2\n\
             [bs]\n\
             second=1\n",
        )
        .unwrap();
        let got = lines(&ini, "BS");
        let keys: Vec<_> = got.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            keys,
            [
                ("J:COM BS", "1471440,0x4973"),
                ("back\\slash", "1"),
                ("dup", "1"),
                ("dup", "2"),
            ]
        );
        assert!(lines(&ini, "CS110").is_empty());
        assert_eq!(get(&ini, "BS", "J:COM BS"), Some("1471440,0x4973"));
        assert_eq!(get(&ini, "BS", "back\\slash"), Some("1"));
    }

    #[test]
    fn integer_syntax() {
        assert_eq!(int("1"), 1);
        assert_eq!(int("  42abc"), 42);
        assert_eq!(int("0x10"), 16);
        assert_eq!(int("010"), 10);
        assert_eq!(int("-1"), u32::MAX);
        assert_eq!(int("yes"), 0);
        assert_eq!(int("+7"), 7);
    }

    #[test]
    fn unquoting() {
        assert_eq!(unquote("\"a b\""), "a b");
        assert_eq!(unquote("'a'"), "a");
        assert_eq!(unquote("\"a'"), "\"a'");
        assert_eq!(unquote("\""), "\"");
    }
}
