//! Numbers as the C runtime reads them (`wcstoul` / `wcstol` with base 0): the syntax of
//! the numbers in the channel lines of the BonDriver ini.

/// C `wcstoul(s, &end, 0)` with a 32-bit `unsigned long`: leading whitespace, an
/// optional sign (`-` negates modulo 2^32), `0x` = hex, leading `0` = octal, else
/// decimal; saturates at `u32::MAX`. Returns the value and the unparsed rest (the
/// whole input when no digits were found).
pub fn strtoul(s: &str) -> (u32, &str) {
    match parse_c_integer(s) {
        Some((neg, mag, rest)) => {
            let v = match u32::try_from(mag) {
                Ok(v) if neg => v.wrapping_neg(),
                Ok(v) => v,
                Err(_) => u32::MAX, // ERANGE, either sign
            };
            (v, rest)
        }
        None => (0, s),
    }
}

/// C `wcstol(s, &end, 0)` with a 32-bit `long` (saturating at `i32::MIN/MAX`).
pub fn strtol(s: &str) -> (i32, &str) {
    match parse_c_integer(s) {
        Some((neg, mag, rest)) => {
            let v = if neg {
                i32::try_from(-i128::from(mag)).unwrap_or(i32::MIN)
            } else {
                i32::try_from(mag).unwrap_or(i32::MAX)
            };
            (v, rest)
        }
        None => (0, s),
    }
}

/// Sign, saturated magnitude and rest of a base-0 C integer; `None` without digits.
fn parse_c_integer(s: &str) -> Option<(bool, u64, &str)> {
    let t = s.trim_start_matches(|c: char| c.is_whitespace());
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let b = t.as_bytes();
    let (radix, digits) =
        if b.len() > 2 && b[0] == b'0' && (b[1] | 0x20) == b'x' && b[2].is_ascii_hexdigit() {
            (16, &t[2..])
        } else if b.first() == Some(&b'0') {
            (8, t)
        } else {
            (10, t)
        };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let mag = digits[..end].chars().fold(0u64, |acc, c| {
        let d = u64::from(c.to_digit(radix).unwrap_or(0));
        acc.saturating_mul(u64::from(radix)).saturating_add(d)
    });
    Some((neg, mag, &digits[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_number_parsing() {
        assert_eq!(strtoul("123abc"), (123, "abc"));
        assert_eq!(strtoul("  0x1F,"), (0x1F, ","));
        assert_eq!(strtoul("0X1f"), (0x1F, ""));
        assert_eq!(strtoul("017"), (15, ""));
        assert_eq!(strtoul("0"), (0, ""));
        assert_eq!(strtoul("0x"), (0, "x"));
        assert_eq!(strtoul("08"), (0, "8"));
        assert_eq!(strtoul("-1"), (u32::MAX, ""));
        assert_eq!(strtoul("99999999999"), (u32::MAX, ""));
        assert_eq!(strtoul("-99999999999"), (u32::MAX, ""));
        assert_eq!(strtoul(",5"), (0, ",5"));
        assert_eq!(strtoul("  x"), (0, "  x"));
        assert_eq!(strtol("-1"), (-1, ""));
        assert_eq!(strtol("+42 "), (42, " "));
        assert_eq!(strtol("99999999999"), (i32::MAX, ""));
        assert_eq!(strtol("-99999999999"), (i32::MIN, ""));
        assert_eq!(strtol("z"), (0, "z"));
    }
}
