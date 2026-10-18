//! Fixed-point monetary amount with four decimal places.
//!
//! Amounts are stored as a signed integer number of ten-thousandths (`1.5`
//! is stored as `15000`), so arithmetic is exact and never suffers from
//! floating point rounding. An amount can be negative: the available funds
//! of a client go below zero when a deposit already withdrawn is disputed.
//! All arithmetic is checked: an overflow yields `None` instead of wrapping.

use std::fmt;
use std::str::FromStr;

/// Number of decimal places kept.
const DECIMALS: usize = 4;
/// `10^DECIMALS`: the integer value of one unit.
const SCALE: i64 = 10_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount(i64);

/// Reason an amount string was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseAmountError(pub String);

impl fmt::Display for ParseAmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid amount {:?}", self.0)
    }
}

impl std::error::Error for ParseAmountError {}

impl Amount {
    pub const ZERO: Amount = Amount(0);

    /// Builds an amount from a raw number of ten-thousandths.
    pub const fn from_raw(raw: i64) -> Self {
        Amount(raw)
    }

    pub fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub fn checked_add(self, other: Amount) -> Option<Amount> {
        self.0.checked_add(other.0).map(Amount)
    }

    pub fn checked_sub(self, other: Amount) -> Option<Amount> {
        self.0.checked_sub(other.0).map(Amount)
    }
}

impl FromStr for Amount {
    type Err = ParseAmountError;

    /// Parses a non-negative decimal with at most four fractional digits,
    /// e.g. `"2"`, `"2."`, `"0.1234"` or `".5"`. Transactions always carry
    /// non-negative amounts, so a sign is rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseAmountError(s.to_owned());
        let (int_part, frac_part) = s.split_once('.').unwrap_or((s, ""));
        let all_digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
        if (int_part.is_empty() && frac_part.is_empty())
            || frac_part.len() > DECIMALS
            || !all_digits(int_part)
            || !all_digits(frac_part)
        {
            return Err(err());
        }
        // Both parts are now plain digit strings (possibly empty).
        let int: i64 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().map_err(|_| err())?
        };
        let mut frac: i64 = 0;
        for i in 0..DECIMALS {
            let digit = frac_part.as_bytes().get(i).map_or(0, |b| b - b'0');
            frac = frac * 10 + i64::from(digit);
        }
        int.checked_mul(SCALE)
            .and_then(|v| v.checked_add(frac))
            .map(Amount)
            .ok_or_else(err)
    }
}

impl fmt::Display for Amount {
    /// Writes the amount with trailing fractional zeros removed:
    /// `1.5000` is written `1.5`, `2.0000` is written `2` and `-0.5000` is
    /// written `-0.5`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The absolute value always fits in a u64, even for i64::MIN.
        let sign = if self.is_negative() { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let scale = SCALE.unsigned_abs();
        let int = abs / scale;
        let frac = abs % scale;
        if frac == 0 {
            return write!(f, "{sign}{int}");
        }
        let digits = format!("{frac:0DECIMALS$}");
        write!(f, "{sign}{int}.{}", digits.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<i64, ParseAmountError> {
        s.parse::<Amount>().map(|a| a.0)
    }

    #[test]
    fn parses_valid_amounts() {
        assert_eq!(parse("0"), Ok(0));
        assert_eq!(parse("1"), Ok(10_000));
        assert_eq!(parse("1."), Ok(10_000));
        assert_eq!(parse("1.0"), Ok(10_000));
        assert_eq!(parse(".5"), Ok(5_000));
        assert_eq!(parse("2.75"), Ok(27_500));
        assert_eq!(parse("0.0001"), Ok(1));
        assert_eq!(parse("123.4567"), Ok(1_234_567));
        assert_eq!(parse("0001.10"), Ok(11_000));
    }

    #[test]
    fn parses_largest_amount() {
        let max = format!("{}.{}", i64::MAX / SCALE, i64::MAX % SCALE);
        assert_eq!(parse(&max), Ok(i64::MAX));
    }

    #[test]
    fn rejects_invalid_amounts() {
        for s in [
            "", ".", "-1", "+1", "1.23456", "1,5", "abc", "1.2.3", "1e3", " 1", "1.a",
        ] {
            assert_eq!(parse(s), Err(ParseAmountError(s.to_owned())), "{s:?}");
        }
    }

    #[test]
    fn rejects_overflowing_amounts() {
        // Integer part does not fit in i64 at all.
        assert!(parse("99999999999999999999999").is_err());
        // Integer part fits in i64 but not once scaled.
        assert!(parse(&format!("{}", i64::MAX / SCALE + 1)).is_err());
        // Scaled integer part fits but adding the fraction overflows.
        let just_over = format!("{}.9999", i64::MAX / SCALE);
        assert!(parse(&just_over).is_err());
    }

    #[test]
    fn displays_without_trailing_zeros() {
        assert_eq!(Amount(0).to_string(), "0");
        assert_eq!(Amount(10_000).to_string(), "1");
        assert_eq!(Amount(15_000).to_string(), "1.5");
        assert_eq!(Amount(10_100).to_string(), "1.01");
        assert_eq!(Amount(1).to_string(), "0.0001");
        assert_eq!(Amount(1_234_567).to_string(), "123.4567");
    }

    #[test]
    fn displays_negative_amounts() {
        assert_eq!(Amount(-10_000).to_string(), "-1");
        assert_eq!(Amount(-15_000).to_string(), "-1.5");
        assert_eq!(Amount(-5_000).to_string(), "-0.5");
        assert_eq!(Amount(-1).to_string(), "-0.0001");
        assert_eq!(Amount(i64::MIN).to_string(), "-922337203685477.5808");
    }

    #[test]
    fn display_round_trips() {
        for raw in [0, 1, 9_999, 10_000, 10_010, 1_234_567, i64::MAX] {
            let a = Amount(raw);
            assert_eq!(a.to_string().parse::<Amount>(), Ok(a));
        }
    }

    #[test]
    fn checked_arithmetic() {
        let one = Amount::from_raw(1);
        assert_eq!(one.checked_add(one), Some(Amount(2)));
        assert_eq!(Amount(i64::MAX).checked_add(one), None);
        assert_eq!(one.checked_sub(one), Some(Amount::ZERO));
        assert_eq!(Amount::ZERO.checked_sub(one), Some(Amount(-1)));
        assert_eq!(Amount(i64::MIN).checked_sub(one), None);
    }

    #[test]
    fn sign() {
        assert!(Amount(-1).is_negative());
        assert!(!Amount::ZERO.is_negative());
        assert!(!Amount(1).is_negative());
    }

    #[test]
    fn error_message() {
        let e = ParseAmountError("x".into());
        assert_eq!(e.to_string(), "invalid amount \"x\"");
    }
}
