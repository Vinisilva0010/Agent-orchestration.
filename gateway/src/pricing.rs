//! Prices and cost calculation in integer micro-dollars (1 USD = 1_000_000).
//!
//! Integer math avoids floating-point rounding, and every cost is rounded up so
//! the gateway never under-counts spending.

use time::{Date, OffsetDateTime};

use crate::provider::Usage;

pub const MICROS_PER_USD: u64 = 1_000_000;
const TOKENS_PER_PRICE_UNIT: u128 = 1_000_000;

/// Price of one model in micro-dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// A price that applies from a UTC date (as `YYYYMMDD`) onwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceChange {
    pub from_yyyymmdd: u32,
    pub price: Price,
}

fn per_million_rounded_up(total: u128) -> u64 {
    u64::try_from(total.div_ceil(TOKENS_PER_PRICE_UNIT)).unwrap_or(u64::MAX)
}

impl Price {
    /// Actual cost of a completed call.
    pub fn cost_micros(&self, usage: &Usage) -> u64 {
        let total = u128::from(usage.input_tokens) * u128::from(self.input)
            + u128::from(usage.output_tokens) * u128::from(self.output)
            + u128::from(usage.cache_read_tokens) * u128::from(self.cache_read)
            + u128::from(usage.cache_write_tokens) * u128::from(self.cache_write);
        per_million_rounded_up(total)
    }

    /// Highest possible cost of a call, used to reserve budget before it runs.
    /// Every input token is priced at the most expensive input rate.
    pub fn max_cost_micros(&self, max_input_tokens: u64, max_output_tokens: u64) -> u64 {
        let highest_input = self.input.max(self.cache_write).max(self.cache_read);
        let total = u128::from(max_input_tokens) * u128::from(highest_input)
            + u128::from(max_output_tokens) * u128::from(self.output);
        per_million_rounded_up(total)
    }
}

/// Price in effect on `day`: the latest change that started on or before it.
pub fn price_on(changes: &[PriceChange], day: u32) -> Option<Price> {
    changes
        .iter()
        .filter(|change| change.from_yyyymmdd <= day)
        .max_by_key(|change| change.from_yyyymmdd)
        .map(|change| change.price)
}

pub fn yyyymmdd(date: Date) -> u32 {
    let year = u32::try_from(date.year()).unwrap_or(0);
    year * 10_000 + u32::from(u8::from(date.month())) * 100 + u32::from(date.day())
}

pub fn today_yyyymmdd() -> u32 {
    yyyymmdd(OffsetDateTime::now_utc().date())
}

/// Parses a dollar amount such as `50` or `12.5` into micro-dollars.
/// Rejects negative values, signs, more than six decimals and anything else.
pub fn parse_usd_to_micros(value: &str) -> Option<u64> {
    let value = value.trim();
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || fraction.len() > 6 || !digits(fraction) {
        return None;
    }
    let whole: u64 = whole.parse().ok()?;
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<6}").parse().ok()?
    };
    whole.checked_mul(MICROS_PER_USD)?.checked_add(fraction)
}

#[cfg(test)]
mod tests {
    use time::Month;

    use super::*;

    const STANDARD: Price = Price {
        input: 3_000_000,
        output: 15_000_000,
        cache_read: 300_000,
        cache_write: 3_750_000,
    };

    fn usage(input: u64, output: u64, read: u64, write: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
        }
    }

    #[test]
    fn one_million_input_tokens_cost_the_listed_price() {
        assert_eq!(STANDARD.cost_micros(&usage(1_000_000, 0, 0, 0)), 3_000_000);
    }

    #[test]
    fn zero_usage_costs_nothing() {
        assert_eq!(STANDARD.cost_micros(&Usage::default()), 0);
    }

    #[test]
    fn rounds_fractions_up() {
        assert_eq!(STANDARD.cost_micros(&usage(0, 0, 1, 0)), 1);
        assert_eq!(STANDARD.cost_micros(&usage(1, 0, 0, 0)), 3);
    }

    #[test]
    fn combines_all_usage_types() {
        let cost = STANDARD.cost_micros(&usage(1_000, 1_000, 1_000, 1_000));
        assert_eq!(cost, 3_000 + 15_000 + 300 + 3_750);
    }

    #[test]
    fn max_cost_prices_input_at_highest_rate() {
        assert_eq!(STANDARD.max_cost_micros(1_000, 0), 3_750);
        assert_eq!(STANDARD.max_cost_micros(0, 1_000), 15_000);
    }

    #[test]
    fn picks_the_price_in_effect_on_a_day() {
        let cheaper = Price {
            input: 1,
            ..STANDARD
        };
        let changes = [
            PriceChange {
                from_yyyymmdd: 20260101,
                price: cheaper,
            },
            PriceChange {
                from_yyyymmdd: 20261201,
                price: STANDARD,
            },
        ];
        assert_eq!(price_on(&changes, 20251231), None);
        assert_eq!(price_on(&changes, 20260101), Some(cheaper));
        assert_eq!(price_on(&changes, 20261130), Some(cheaper));
        assert_eq!(price_on(&changes, 20261201), Some(STANDARD));
    }

    #[test]
    fn formats_dates_as_yyyymmdd() {
        let date = Date::from_calendar_date(2026, Month::December, 1).unwrap();
        assert_eq!(yyyymmdd(date), 20261201);
    }

    #[test]
    fn parses_dollar_amounts() {
        assert_eq!(parse_usd_to_micros("50"), Some(50_000_000));
        assert_eq!(parse_usd_to_micros("12.5"), Some(12_500_000));
        assert_eq!(parse_usd_to_micros("0.000001"), Some(1));
        assert_eq!(parse_usd_to_micros(" 7 "), Some(7_000_000));
    }

    #[test]
    fn rejects_invalid_dollar_amounts() {
        for bad in ["", "-1", "+1", "1.1234567", "abc", ".5", "1,5", "1e3"] {
            assert_eq!(parse_usd_to_micros(bad), None, "{bad}");
        }
    }
}
