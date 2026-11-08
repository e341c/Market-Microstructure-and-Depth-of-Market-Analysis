//! Fixed-point ("lamport-like") helpers.
//! Every price / quantity is a u128 scaled by 1e8. No floats anywhere.

pub const SCALE: u128 = 100_000_000;
const DECIMALS: usize = 8;

/// Parses a decimal string ("75886.89", "0.00000001", "5") into a u128 scaled by 1e8.
/// Digits beyond the 8th decimal are truncated. Returns None on any invalid input.
pub fn parse_fixed(s: &str) -> Option<u128> {
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }

    let mut int: u128 = 0;
    for c in int_part.bytes() {
        if !c.is_ascii_digit() {
            return None;
        }
        int = int.checked_mul(10)?.checked_add((c - b'0') as u128)?;
    }

    let mut frac: u128 = 0;
    let mut digits = 0;
    for c in frac_part.bytes() {
        if !c.is_ascii_digit() {
            return None;
        }
        if digits < DECIMALS {
            frac = frac * 10 + (c - b'0') as u128;
            digits += 1;
        }
    }
    while digits < DECIMALS {
        frac *= 10;
        digits += 1;
    }

    int.checked_mul(SCALE)?.checked_add(frac)
}

/// Formats a scaled u128 back to a decimal string, trimming trailing zeros.
pub fn fmt_fixed(v: u128) -> String {
    let int = v / SCALE;
    let frac = v % SCALE;
    if frac == 0 {
        return int.to_string();
    }
    let mut f = format!("{:08}", frac);
    while f.ends_with('0') {
        f.pop();
    }
    format!("{int}.{f}")
}

/// Formats OBI (scaled by 10_000) as e.g. "-0.4290".
pub fn fmt_obi(v: i32) -> String {
    let sign = if v < 0 { '-' } else { '+' };
    let a = v.unsigned_abs();
    format!("{}{}.{:04}", sign, a / 10_000, a % 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse_fixed("75886.89"), Some(7_588_689_000_000));
        assert_eq!(parse_fixed("0.00000001"), Some(1));
        assert_eq!(parse_fixed("5"), Some(500_000_000));
        assert_eq!(parse_fixed("1.123456789"), Some(112_345_678));
        assert_eq!(parse_fixed(""), None);
        assert_eq!(parse_fixed("abc"), None);
        assert_eq!(parse_fixed("-1"), None);
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_fixed(7_588_689_000_000), "75886.89");
        assert_eq!(fmt_fixed(1), "0.00000001");
        assert_eq!(fmt_fixed(500_000_000), "5");
        assert_eq!(fmt_obi(-4290), "-0.4290");
        assert_eq!(fmt_obi(10_000), "+1.0000");
    }
}

#[test]
fn roundtrip_from_log() {
    for s in ["76022.73", "3.24988", "4.20347"] {
        assert_eq!(fmt_fixed(parse_fixed(s).unwrap()), s);
    }
}

/// Formats a signed scaled value, e.g. -12345678 -> "-0.12345678".
pub fn fmt_fixed_signed(v: i128) -> String {
    let s = fmt_fixed(v.unsigned_abs());
    if v < 0 { format!("-{s}") } else { s }
}
