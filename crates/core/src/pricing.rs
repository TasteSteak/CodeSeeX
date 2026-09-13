//! Data-driven pricing and peak/valley billing.
//!
//! The pricing table is pure data: it is embedded with the built-in catalog,
//! can be replaced by a remote catalog document, and can be overridden per
//! model by the user. This module owns the single implementation of the
//! peak/valley window rule so the store, the manager API and the UI cannot
//! drift apart.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Upper bound accepted for a peak multiplier. Anything above this is treated
/// as a malformed document instead of a real price.
pub const MAX_PEAK_MULTIPLIER: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelRates {
    pub cached_input: f64,
    pub cache_miss_input: f64,
    pub output: f64,
}

impl ModelRates {
    pub fn new(cached_input: f64, cache_miss_input: f64, output: f64) -> Self {
        Self {
            cached_input,
            cache_miss_input,
            output,
        }
    }

    pub fn is_all_zero(self) -> bool {
        self.cached_input == 0.0 && self.cache_miss_input == 0.0 && self.output == 0.0
    }

    pub fn is_finite_and_non_negative(self) -> bool {
        [self.cached_input, self.cache_miss_input, self.output]
            .into_iter()
            .all(|value| value.is_finite() && value >= 0.0)
    }

    pub fn to_value(self) -> Value {
        json!({
            "cached_input": self.cached_input,
            "cache_miss_input": self.cache_miss_input,
            "output": self.output,
        })
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let read = |key: &str| -> Option<f64> {
            match object.get(key) {
                Some(value) => value.as_f64(),
                None => Some(0.0),
            }
        };
        let rates = Self {
            cached_input: read("cached_input")?,
            cache_miss_input: read("cache_miss_input")?,
            output: read("output")?,
        };
        rates.is_finite_and_non_negative().then_some(rates)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakWindow {
    pub from_minute: u32,
    pub to_minute: u32,
}

impl PeakWindow {
    pub fn contains(self, minute_of_day: u32) -> bool {
        minute_of_day >= self.from_minute && minute_of_day < self.to_minute
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeakValley {
    pub enabled: bool,
    pub timezone: String,
    pub utc_offset_minutes: i32,
    pub multiplier: f64,
    pub windows: Vec<PeakWindow>,
}

impl Default for PeakValley {
    fn default() -> Self {
        Self {
            enabled: true,
            timezone: "Asia/Shanghai".to_owned(),
            utc_offset_minutes: 8 * 60,
            multiplier: 2.0,
            windows: vec![
                PeakWindow {
                    from_minute: 9 * 60,
                    to_minute: 12 * 60,
                },
                PeakWindow {
                    from_minute: 14 * 60,
                    to_minute: 18 * 60,
                },
            ],
        }
    }
}

impl PeakValley {
    pub fn is_active(&self) -> bool {
        self.enabled && !self.windows.is_empty() && self.multiplier > 1.0
    }

    /// Peak/off-peak classification for a completion timestamp.
    ///
    /// `completed_at` is an RFC 3339 timestamp. A timestamp that cannot be
    /// parsed is treated as off-peak, matching the previous behaviour.
    pub fn period_for(&self, completed_at: &str) -> BillingPeriod {
        if !self.is_active() {
            return BillingPeriod::off_peak();
        }
        let Some(minute) = local_minute_of_day(completed_at, self.utc_offset_minutes) else {
            return BillingPeriod::off_peak();
        };
        if self.windows.iter().any(|window| window.contains(minute)) {
            BillingPeriod {
                name: "peak".to_owned(),
                multiplier: self.multiplier,
            }
        } else {
            BillingPeriod::off_peak()
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "timezone": self.timezone,
            "multiplier": self.multiplier,
            "windows": self
                .windows
                .iter()
                .map(|window| json!({
                    "from": minute_to_hhmm(window.from_minute),
                    "to": minute_to_hhmm(window.to_minute),
                }))
                .collect::<Vec<_>>(),
        })
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "peak_valley must be an object".to_owned())?;
        let timezone = object
            .get("timezone")
            .and_then(Value::as_str)
            .unwrap_or("Asia/Shanghai")
            .trim()
            .to_owned();
        let utc_offset_minutes = timezone_utc_offset_minutes(&timezone)
            .ok_or_else(|| format!("unsupported peak_valley timezone: {timezone}"))?;
        let multiplier = object
            .get("multiplier")
            .map(|value| {
                value
                    .as_f64()
                    .ok_or_else(|| "peak_valley.multiplier must be a number".to_owned())
            })
            .transpose()?
            .unwrap_or(2.0);
        if !multiplier.is_finite() || multiplier < 1.0 || multiplier > MAX_PEAK_MULTIPLIER {
            return Err(format!(
                "peak_valley.multiplier must be between 1 and {MAX_PEAK_MULTIPLIER}"
            ));
        }
        let enabled = match object.get("enabled") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "peak_valley.enabled must be a boolean".to_owned())?,
            None => true,
        };
        let windows = match object.get("windows") {
            Some(value) => value
                .as_array()
                .ok_or_else(|| "peak_valley.windows must be an array".to_owned())?
                .iter()
                .map(parse_peak_window)
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        for window in &windows {
            if window.from_minute >= window.to_minute {
                return Err("peak_valley window must start before it ends".to_owned());
            }
        }
        Ok(Self {
            enabled,
            timezone,
            utc_offset_minutes,
            multiplier,
            windows,
        })
    }
}

fn parse_peak_window(value: &Value) -> Result<PeakWindow, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "peak_valley window must be an object".to_owned())?;
    let from = object
        .get("from")
        .and_then(Value::as_str)
        .ok_or_else(|| "peak_valley window needs a from time".to_owned())?;
    let to = object
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| "peak_valley window needs a to time".to_owned())?;
    Ok(PeakWindow {
        from_minute: parse_hhmm(from)
            .ok_or_else(|| format!("peak_valley window from is not HH:MM: {from}"))?,
        to_minute: parse_hhmm(to)
            .ok_or_else(|| format!("peak_valley window to is not HH:MM: {to}"))?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateSource {
    Exact,
    Group,
}

impl RateSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exact => "model",
            Self::Group => "group",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedRate {
    pub rates: ModelRates,
    pub source: RateSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BillingPeriod {
    pub name: String,
    pub multiplier: f64,
}

impl BillingPeriod {
    fn off_peak() -> Self {
        Self {
            name: "off_peak".to_owned(),
            multiplier: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostEstimate {
    pub amount: f64,
    pub currency: String,
    pub unit: String,
    pub revision: String,
    pub billing_period: String,
    pub billing_multiplier: f64,
    pub rate_source: String,
    pub rates: ModelRates,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PricingTable {
    pub revision: String,
    pub currency: String,
    pub unit: String,
    pub peak_valley: PeakValley,
    pub groups: BTreeMap<String, ModelRates>,
    pub rates: BTreeMap<String, ModelRates>,
}

impl Default for PricingTable {
    fn default() -> Self {
        Self {
            revision: "0.0.0".to_owned(),
            currency: "CNY".to_owned(),
            unit: "per_1m_tokens".to_owned(),
            peak_valley: PeakValley::default(),
            groups: BTreeMap::new(),
            rates: BTreeMap::new(),
        }
    }
}

impl PricingTable {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "pricing must be an object".to_owned())?;
        let revision = object
            .get("revision")
            .and_then(Value::as_str)
            .unwrap_or("0.0.0")
            .trim()
            .to_owned();
        if revision.is_empty() {
            return Err("pricing.revision must not be empty".to_owned());
        }
        let currency = object
            .get("currency")
            .and_then(Value::as_str)
            .unwrap_or("CNY")
            .trim()
            .to_owned();
        let unit = object
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("per_1m_tokens")
            .trim()
            .to_owned();
        let peak_valley = match object.get("peak_valley") {
            Some(value) => PeakValley::from_value(value)?,
            None => PeakValley::default(),
        };
        let groups = parse_rate_map(object.get("groups"), "pricing.groups")?;
        let rates = parse_rate_map(object.get("rates"), "pricing.rates")?;
        Ok(Self {
            revision,
            currency,
            unit,
            peak_valley,
            groups,
            rates,
        })
    }

    pub fn to_value(&self) -> Value {
        json!({
            "revision": self.revision,
            "currency": self.currency,
            "unit": self.unit,
            "peak_valley": self.peak_valley.to_value(),
            "groups": self
                .groups
                .iter()
                .map(|(key, rates)| (key.clone(), rates.to_value()))
                .collect::<serde_json::Map<_, _>>(),
            "rates": self
                .rates
                .iter()
                .map(|(key, rates)| (key.clone(), rates.to_value()))
                .collect::<serde_json::Map<_, _>>(),
        })
    }

    pub fn rate_for(&self, model: &str, group: Option<&str>) -> Option<ResolvedRate> {
        let model = model.trim();
        if let Some(rates) = self.rates.get(model) {
            return Some(ResolvedRate {
                rates: *rates,
                source: RateSource::Exact,
            });
        }
        let group = group.map(str::trim).filter(|value| !value.is_empty())?;
        self.groups.get(group).map(|rates| ResolvedRate {
            rates: *rates,
            source: RateSource::Group,
        })
    }

    pub fn period_for(&self, completed_at: &str) -> BillingPeriod {
        self.peak_valley.period_for(completed_at)
    }

    pub fn estimate(
        &self,
        model: &str,
        group: Option<&str>,
        cached_input_tokens: u64,
        cache_miss_input_tokens: u64,
        output_tokens: u64,
        completed_at: &str,
    ) -> Option<CostEstimate> {
        let resolved = self.rate_for(model, group)?;
        let period = self.period_for(completed_at);
        let amount = ((cached_input_tokens as f64 * resolved.rates.cached_input
            + cache_miss_input_tokens as f64 * resolved.rates.cache_miss_input
            + output_tokens as f64 * resolved.rates.output)
            / 1_000_000.0)
            * period.multiplier;
        Some(CostEstimate {
            amount,
            currency: self.currency.clone(),
            unit: self.unit.clone(),
            revision: self.revision.clone(),
            billing_period: period.name,
            billing_multiplier: period.multiplier,
            rate_source: resolved.source.label().to_owned(),
            rates: resolved.rates,
        })
    }

    /// Applies a sparse override document on top of this table.
    pub fn with_override_value(&self, value: &Value) -> Result<Self, String> {
        let Some(object) = value.as_object() else {
            return Err("pricing override must be an object".to_owned());
        };
        let mut next = self.clone();
        if let Some(revision) = object.get("revision").and_then(Value::as_str) {
            if !revision.trim().is_empty() {
                next.revision = revision.trim().to_owned();
            }
        }
        if let Some(currency) = object.get("currency").and_then(Value::as_str) {
            if !currency.trim().is_empty() {
                next.currency = currency.trim().to_owned();
            }
        }
        if let Some(unit) = object.get("unit").and_then(Value::as_str) {
            if !unit.trim().is_empty() {
                next.unit = unit.trim().to_owned();
            }
        }
        if let Some(enabled) = object.get("peak_valley_enabled").and_then(Value::as_bool) {
            next.peak_valley.enabled = enabled;
        }
        if let Some(multiplier) = object.get("peak_multiplier").and_then(Value::as_f64) {
            if !multiplier.is_finite() || multiplier < 1.0 || multiplier > MAX_PEAK_MULTIPLIER {
                return Err(format!(
                    "peak_multiplier must be between 1 and {MAX_PEAK_MULTIPLIER}"
                ));
            }
            next.peak_valley.multiplier = multiplier;
        }
        if let Some(timezone) = object.get("timezone").and_then(Value::as_str) {
            let timezone = timezone.trim();
            next.peak_valley.utc_offset_minutes = timezone_utc_offset_minutes(timezone)
                .ok_or_else(|| format!("unsupported timezone: {timezone}"))?;
            next.peak_valley.timezone = timezone.to_owned();
        }
        if let Some(windows) = object.get("peak_windows") {
            let windows = windows
                .as_array()
                .ok_or_else(|| "peak_windows must be an array".to_owned())?
                .iter()
                .map(parse_peak_window)
                .collect::<Result<Vec<_>, _>>()?;
            for window in &windows {
                if window.from_minute >= window.to_minute {
                    return Err("peak window must start before it ends".to_owned());
                }
            }
            next.peak_valley.windows = windows;
        }
        if let Some(rates) = object.get("rates") {
            for (slug, value) in parse_rate_map(Some(rates), "pricing.rates")? {
                next.rates.insert(slug, value);
            }
        }
        if let Some(groups) = object.get("groups") {
            for (key, value) in parse_rate_map(Some(groups), "pricing.groups")? {
                next.groups.insert(key, value);
            }
        }
        Ok(next)
    }
}

fn parse_rate_map(
    value: Option<&Value>,
    label: &str,
) -> Result<BTreeMap<String, ModelRates>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))?;
    let mut map = BTreeMap::new();
    for (key, value) in object {
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("{label} contains an empty key"));
        }
        let rates = ModelRates::from_value(value)
            .ok_or_else(|| format!("{label}.{key} must contain non-negative finite numbers"))?;
        map.insert(key.to_owned(), rates);
    }
    Ok(map)
}

pub fn minute_to_hhmm(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

pub fn parse_hhmm(value: &str) -> Option<u32> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

pub fn timezone_utc_offset_minutes(timezone: &str) -> Option<i32> {
    let normalized = timezone.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "asia/shanghai" | "asia/chongqing" | "prc" | "cst" => return Some(8 * 60),
        "utc" | "etc/utc" | "z" | "gmt" => return Some(0),
        _ => {}
    }
    let (sign, rest) = match normalized.strip_prefix('+') {
        Some(rest) => (1_i32, rest),
        None => match normalized.strip_prefix('-') {
            Some(rest) => (-1_i32, rest),
            None => return None,
        },
    };
    let (hour, minute) = rest.split_once(':').unwrap_or((rest, "0"));
    let hour: i32 = hour.parse().ok()?;
    let minute: i32 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(sign * (hour * 60 + minute))
}

fn local_minute_of_day(timestamp: &str, utc_offset_minutes: i32) -> Option<u32> {
    let utc_minutes = parse_rfc3339_utc_minutes(timestamp)?;
    Some((utc_minutes + utc_offset_minutes as i64).rem_euclid(24 * 60) as u32)
}

/// Minimal RFC 3339 minute parser. Only the parts CodeSeeX needs are
/// supported: date, time, optional fractional seconds and a numeric offset.
fn parse_rfc3339_utc_minutes(value: &str) -> Option<i64> {
    let text = value.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 16 {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: i64 = text.get(5..7)?.parse().ok()?;
    let day: i64 = text.get(8..10)?.parse().ok()?;
    if !matches!(bytes[4], b'-') || !matches!(bytes[7], b'-') {
        return None;
    }
    let separator = bytes[10];
    if separator != b'T' && separator != b't' && separator != b' ' {
        return None;
    }
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    if !matches!(bytes[13], b':') || hour > 23 || minute > 59 {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let rest = &text[16..];
    let mut offset_minutes = 0_i64;
    let mut rest = rest;
    if let Some(second_rest) = rest.strip_prefix(':') {
        let seconds_len = second_rest
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(second_rest.len());
        let seconds: i64 = second_rest.get(0..seconds_len)?.parse().ok()?;
        if seconds > 60 {
            return None;
        }
        rest = &second_rest[seconds_len..];
    }
    if let Some(fraction_rest) = rest.strip_prefix('.') {
        let digits_len = fraction_rest
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(fraction_rest.len());
        rest = &fraction_rest[digits_len..];
    }
    match rest.chars().next() {
        None => {}
        Some('Z') | Some('z') => {}
        Some(sign @ ('+' | '-')) => {
            let offset_text = &rest[1..];
            let (hour_text, minute_text) =
                offset_text.split_once(':').unwrap_or((offset_text, "0"));
            let offset_hour: i64 = hour_text.parse().ok()?;
            let offset_minute: i64 = minute_text.parse().ok()?;
            let magnitude = offset_hour * 60 + offset_minute;
            offset_minutes = if sign == '-' { -magnitude } else { magnitude };
        }
        Some(_) => return None,
    }
    let days = days_from_civil(year, month, day);
    Some(days * 24 * 60 + hour * 60 + minute - offset_minutes)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_position = (month + 9) % 12;
    let day_of_year = (153 * month_position + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PricingTable {
        PricingTable::from_value(&json!({
            "revision": "test.1",
            "currency": "CNY",
            "unit": "per_1m_tokens",
            "peak_valley": {
                "enabled": true,
                "timezone": "Asia/Shanghai",
                "multiplier": 2.0,
                "windows": [
                    { "from": "09:00", "to": "12:00" },
                    { "from": "14:00", "to": "18:00" }
                ]
            },
            "groups": { "default": { "cached_input": 0.1, "cache_miss_input": 1.0, "output": 2.0 } },
            "rates": { "deepseek-v4-pro": { "cached_input": 0.025, "cache_miss_input": 3.0, "output": 6.0 } }
        }))
        .expect("valid pricing table")
    }

    #[test]
    fn peak_window_boundaries_follow_beijing_time() {
        let table = table();
        let peak = |timestamp: &str| table.period_for(timestamp).name;
        assert_eq!(peak("2026-09-10T00:59:00Z"), "off_peak");
        assert_eq!(peak("2026-09-10T01:00:00Z"), "peak");
        assert_eq!(peak("2026-09-10T03:59:00Z"), "peak");
        assert_eq!(peak("2026-09-10T04:00:00Z"), "off_peak");
        assert_eq!(peak("2026-09-10T05:59:00Z"), "off_peak");
        assert_eq!(peak("2026-09-10T06:00:00Z"), "peak");
        assert_eq!(peak("2026-09-10T09:59:00Z"), "peak");
        assert_eq!(peak("2026-09-10T10:00:00Z"), "off_peak");
        assert_eq!(peak("2026-09-10T15:59:00Z"), "off_peak");
    }

    #[test]
    fn explicit_offsets_and_fractions_are_supported() {
        let table = table();
        assert_eq!(table.period_for("2026-09-10T09:30:00+08:00").name, "peak");
        assert_eq!(
            table.period_for("2026-09-10T09:30:00.482+08:00").name,
            "peak"
        );
        assert_eq!(table.period_for("not-a-timestamp").name, "off_peak");
    }

    #[test]
    fn estimate_applies_multiplier_and_reports_source() {
        let table = table();
        let off_peak = table
            .estimate(
                "deepseek-v4-pro",
                None,
                1_000_000,
                1_000_000,
                1_000_000,
                "2026-09-10T20:00:00+08:00",
            )
            .expect("priced model");
        assert_eq!(off_peak.billing_period, "off_peak");
        assert_eq!(off_peak.billing_multiplier, 1.0);
        assert!((off_peak.amount - 9.025).abs() < 1e-9);
        assert_eq!(off_peak.rate_source, "model");

        let peak = table
            .estimate(
                "deepseek-v4-pro",
                None,
                1_000_000,
                1_000_000,
                1_000_000,
                "2026-09-10T10:00:00+08:00",
            )
            .expect("priced model");
        assert_eq!(peak.billing_period, "peak");
        assert!((peak.amount - 18.05).abs() < 1e-9);
    }

    #[test]
    fn unpriced_models_are_not_silently_priced() {
        let table = table();
        assert!(table
            .estimate("unknown-model", None, 1, 1, 1, "2026-09-10T20:00:00+08:00")
            .is_none());
        let grouped = table
            .estimate(
                "unknown-model",
                Some("default"),
                1,
                1,
                1,
                "2026-09-10T20:00:00+08:00",
            )
            .expect("group fallback");
        assert_eq!(grouped.rate_source, "group");
    }

    #[test]
    fn invalid_documents_are_rejected() {
        assert!(
            PricingTable::from_value(&json!({ "rates": { "m": { "output": -1.0 } } })).is_err()
        );
        assert!(PricingTable::from_value(&json!({
            "peak_valley": { "windows": [ { "from": "12:00", "to": "09:00" } ] }
        }))
        .is_err());
        assert!(PricingTable::from_value(&json!({
            "peak_valley": { "multiplier": 1000.0 }
        }))
        .is_err());
        assert!(PricingTable::from_value(&json!({
            "peak_valley": { "timezone": "Mars/Olympus" }
        }))
        .is_err());
        assert!(PricingTable::from_value(&json!({ "revision": "  " })).is_err());
    }

    #[test]
    fn sparse_override_keeps_unmentioned_fields() {
        let table = table();
        let next = table
            .with_override_value(&json!({
                "peak_multiplier": 1.5,
                "rates": { "deepseek-v4-flash": { "cached_input": 0.02, "cache_miss_input": 1.0, "output": 2.0 } }
            }))
            .expect("override applies");
        assert_eq!(next.peak_valley.multiplier, 1.5);
        assert_eq!(
            next.rate_for("deepseek-v4-pro", None).unwrap().rates.output,
            6.0
        );
        assert_eq!(
            next.rate_for("deepseek-v4-flash", None)
                .unwrap()
                .rates
                .output,
            2.0
        );
        assert_eq!(next.peak_valley.windows.len(), 2);
        assert_eq!(next.revision, "test.1");
    }
}
