//! The usage ledger: a turn's spend booked per model, and ledger rows summarized for a day,
//! week, month or year, with the share served by local models and what they saved. Pure: the
//! caller passes the clock and the local UTC offset.

use hangar_core::changes::{Cost, TokenCount, UsageEntry};
use hangar_core::{SessionId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::history::sum_costs;

pub mod pricing;

use pricing::{CustomPrice, price_of};

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;
/// Models drawn separately in the chart; the rest are summed as "Other".
const CHART_MODELS: usize = 5;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// What a turn spent, as the agent reported it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnSpend {
    /// The cost added since the previous booking.
    pub cost: Option<Cost>,
    pub tokens: Option<TokenCount>,
    /// Total tokens per model, used as shares of `cost` and `tokens`.
    pub models: Vec<(String, u64)>,
}

/// Served by Ollama (OpenCode names its models `ollama/<tag>`).
pub fn is_local(model: &str) -> bool {
    model.starts_with("ollama/")
}

/// The ledger rows for one turn: one per model the agent names, sharing the cost and tokens by
/// each model's token count; else one row for `current_model`. Empty when nothing was spent.
pub fn book_turn(
    session_id: SessionId,
    at: Timestamp,
    spend: TurnSpend,
    current_model: Option<&str>,
) -> Vec<UsageEntry> {
    let TurnSpend {
        cost,
        tokens,
        models,
    } = spend;
    if cost.is_none() && tokens.is_none() {
        return Vec::new();
    }
    let entry =
        |model: Option<String>, cost: Option<Cost>, tokens: Option<TokenCount>| UsageEntry {
            session_id,
            at,
            local: model.as_deref().is_some_and(is_local),
            model,
            cost,
            tokens,
        };
    let weight: u64 = models.iter().map(|(_, t)| t).sum();
    if models.len() < 2 || weight == 0 {
        let model = models
            .into_iter()
            .next()
            .map(|(m, _)| m)
            .or_else(|| current_model.map(str::to_owned));
        return vec![entry(model, cost, tokens)];
    }
    let mut left = tokens;
    let last = models.len() - 1;
    models
        .into_iter()
        .enumerate()
        .map(|(i, (model, share))| {
            let fraction = share as f64 / weight as f64;
            let cost = cost.as_ref().map(|c| Cost {
                amount: c.amount * fraction,
                currency: c.currency.clone(),
            });
            // The last model takes the rounding remainder so the rows add up to the turn.
            let tokens = match (i == last, tokens.as_ref(), left.as_mut()) {
                (true, _, _) => left,
                (false, Some(all), Some(left)) => {
                    let part = scale(all, fraction);
                    left.input -= part.input;
                    left.output -= part.output;
                    left.cache_read -= part.cache_read;
                    left.cache_write -= part.cache_write;
                    Some(part)
                }
                _ => None,
            };
            entry(Some(model), cost, tokens)
        })
        .collect()
}

fn scale(tokens: &TokenCount, fraction: f64) -> TokenCount {
    let part = |n: u64| ((n as f64 * fraction).round() as u64).min(n);
    TokenCount {
        input: part(tokens.input),
        output: part(tokens.output),
        cache_read: part(tokens.cache_read),
        cache_write: part(tokens.cache_write),
    }
}

/// How much time the dashboard shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Period {
    #[default]
    Day,
    Week,
    Month,
    Year,
}

impl Period {
    pub const ALL: [Period; 4] = [Self::Day, Self::Week, Self::Month, Self::Year];

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "Day",
            Self::Week => "Week",
            Self::Month => "Month",
            Self::Year => "Year",
        }
    }
}

/// The dashboard's settings, saved under `usage`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageSettings {
    pub period: Period,
    /// The cloud model local use is priced at; `None` picks the cloud model used most.
    pub reference_model: Option<String>,
    pub prices: Vec<CustomPrice>,
}

/// One bar of the chart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub start: Timestamp,
    pub label: String,
}

/// A stretch of local time, cut into the chart's bars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: Timestamp,
    /// Exclusive.
    pub end: Timestamp,
    /// `Today`, `This week`, `October 2026`, …
    pub title: String,
    pub buckets: Vec<Bucket>,
}

/// The `period` containing `now`, or the one `back` periods before it: hours of a day, days of a
/// week (Monday first) or month, months of a year. The current UTC offset is used throughout, so
/// a range across a DST change is off by an hour at one end.
pub fn range(period: Period, now: Timestamp, utc_offset_secs: i32, back: u32) -> Range {
    let offset = i64::from(utc_offset_secs) * 1000;
    let to_utc = |day: i64| Timestamp(day * DAY_MS - offset);
    let today = (now.0 + offset).div_euclid(DAY_MS);
    let back = i64::from(back);
    match period {
        Period::Day => {
            let day = today - back;
            let start = to_utc(day);
            let title = match back {
                0 => "Today".to_owned(),
                1 => "Yesterday".to_owned(),
                _ => date_label(day),
            };
            let buckets = (0..24)
                .map(|h| Bucket {
                    start: Timestamp(start.0 + h * HOUR_MS),
                    label: format!("{h:02}"),
                })
                .collect();
            Range {
                start,
                end: to_utc(day + 1),
                title,
                buckets,
            }
        }
        Period::Week => {
            let monday = today - weekday(today) - 7 * back;
            let title = match back {
                0 => "This week".to_owned(),
                1 => "Last week".to_owned(),
                _ => format!("Week of {}", date_label(monday)),
            };
            Range {
                start: to_utc(monday),
                end: to_utc(monday + 7),
                title,
                buckets: (0..7)
                    .map(|d| Bucket {
                        start: to_utc(monday + d),
                        label: WEEKDAYS[d as usize].to_owned(),
                    })
                    .collect(),
            }
        }
        Period::Month => {
            let (y, m, _) = civil_from_days(today);
            let months = y * 12 + i64::from(m - 1) - back;
            let (y, m) = (months.div_euclid(12), months.rem_euclid(12) as u32 + 1);
            let first = days_from_civil(y, m, 1);
            let next = days_from_civil(y + i64::from(m == 12), m % 12 + 1, 1);
            Range {
                start: to_utc(first),
                end: to_utc(next),
                title: format!("{} {y}", MONTH_NAMES[m as usize - 1]),
                buckets: (first..next)
                    .map(|d| Bucket {
                        start: to_utc(d),
                        label: (d - first + 1).to_string(),
                    })
                    .collect(),
            }
        }
        Period::Year => {
            let y = civil_from_days(today).0 - back;
            Range {
                start: to_utc(days_from_civil(y, 1, 1)),
                end: to_utc(days_from_civil(y + 1, 1, 1)),
                title: y.to_string(),
                buckets: (1..=12)
                    .map(|m| Bucket {
                        start: to_utc(days_from_civil(y, m, 1)),
                        label: MONTHS[m as usize - 1].to_owned(),
                    })
                    .collect(),
            }
        }
    }
}

/// Monday = 0 for days since 1970-01-01 (a Thursday).
fn weekday(day: i64) -> i64 {
    (day + 3).rem_euclid(7)
}

fn date_label(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{} {d}, {y}", MONTHS[m as usize - 1])
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` of days since 1970-01-01 (H. Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// One model's spend in the range.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRow {
    /// `None`: rows from before Hangar recorded models, or agents that don't say.
    pub model: Option<String>,
    pub local: bool,
    /// Per currency, in first-seen order.
    pub costs: Vec<Cost>,
    /// Tokens of the rows that report them.
    pub tokens: TokenCount,
    pub turns: usize,
}

impl ModelRow {
    pub fn name(&self) -> &str {
        self.model.as_deref().unwrap_or("Unknown model")
    }

    fn amount(&self, currency: &str) -> f64 {
        self.costs
            .iter()
            .filter(|c| c.currency == currency)
            .map(|c| c.amount)
            .sum()
    }
}

/// Cost per bar, one series per top model plus "Other", in one currency.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chart {
    pub currency: Option<String>,
    pub series: Vec<String>,
    /// Bar label and one amount per series.
    pub bars: Vec<(String, Vec<f64>)>,
}

/// How much local models did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalShare {
    pub local_tokens: u64,
    pub tokens: u64,
    pub local_turns: usize,
    pub turns: usize,
    /// Some local turns reported no tokens, so the saving leaves them out.
    pub local_tokens_unknown: bool,
}

impl LocalShare {
    /// Local share of tokens, else of turns; `None` with no turns.
    pub fn ratio(&self) -> Option<f64> {
        if self.tokens > 0 {
            Some(self.local_tokens as f64 / self.tokens as f64)
        } else if self.turns > 0 {
            Some(self.local_turns as f64 / self.turns as f64)
        } else {
            None
        }
    }
}

/// What local models saved, priced at `reference`.
#[derive(Clone, Debug, PartialEq)]
pub struct Saving {
    pub reference: String,
    pub cost: Cost,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSummary {
    pub totals: Vec<Cost>,
    pub turns: usize,
    pub tokens: TokenCount,
    /// The costliest first (in the chart's currency), then by tokens.
    pub models: Vec<ModelRow>,
    pub chart: Chart,
    pub local: LocalShare,
    /// `None` without local tokens or without a priced reference model.
    pub saving: Option<Saving>,
    /// The reference model used: the setting, else the priced cloud model with the most tokens.
    pub reference: Option<String>,
}

/// Summarizes the ledger rows inside `range`.
pub fn summarize(entries: &[UsageEntry], range: &Range, settings: &UsageSettings) -> UsageSummary {
    let rows: Vec<&UsageEntry> = entries
        .iter()
        .filter(|e| e.at >= range.start && e.at < range.end)
        .collect();
    let turns_of = |rows: &mut dyn Iterator<Item = &&UsageEntry>| {
        let mut seen: Vec<(SessionId, Timestamp)> = rows.map(|e| (e.session_id, e.at)).collect();
        seen.sort_by_key(|(s, at)| (s.0, *at));
        seen.dedup();
        seen.len()
    };
    let mut summary = UsageSummary {
        totals: sum_costs(rows.iter().filter_map(|e| e.cost.as_ref())),
        turns: turns_of(&mut rows.iter()),
        ..UsageSummary::default()
    };
    for e in &rows {
        if let Some(t) = e.tokens {
            summary.tokens += t;
        }
    }

    let mut models: Vec<ModelRow> = Vec::new();
    for e in &rows {
        let i = match models.iter().position(|m| m.model == e.model) {
            Some(i) => i,
            None => {
                models.push(ModelRow {
                    model: e.model.clone(),
                    local: e.local,
                    costs: Vec::new(),
                    tokens: TokenCount::default(),
                    turns: 0,
                });
                models.len() - 1
            }
        };
        let row = &mut models[i];
        if let Some(cost) = &e.cost {
            row.costs = sum_costs(row.costs.iter().chain([cost]));
        }
        if let Some(t) = e.tokens {
            row.tokens += t;
        }
    }
    for row in &mut models {
        row.turns = turns_of(&mut rows.iter().filter(|e| e.model == row.model));
    }
    let currency = summary
        .totals
        .iter()
        .max_by(|a, b| a.amount.total_cmp(&b.amount))
        .map(|c| c.currency.clone());
    let main = currency.as_deref().unwrap_or("USD");
    models.sort_by(|a, b| {
        b.amount(main)
            .total_cmp(&a.amount(main))
            .then(b.tokens.total().cmp(&a.tokens.total()))
    });

    summary.chart = chart(&rows, range, &models, currency);

    let local_rows = || rows.iter().filter(|e| e.local);
    summary.local = LocalShare {
        local_tokens: local_rows()
            .filter_map(|e| e.tokens)
            .map(|t| t.total())
            .sum(),
        tokens: summary.tokens.total(),
        local_turns: turns_of(&mut local_rows()),
        turns: summary.turns,
        local_tokens_unknown: local_rows().any(|e| e.tokens.is_none()),
    };

    summary.reference = settings.reference_model.clone().or_else(|| {
        models
            .iter()
            .filter(|m| !m.local)
            .filter_map(|m| Some((m.model.as_deref()?, m.tokens.total())))
            .filter(|(m, _)| price_of(m, &settings.prices).is_some())
            .max_by_key(|(_, tokens)| *tokens)
            .map(|(m, _)| m.to_owned())
    });
    let price = summary
        .reference
        .as_deref()
        .and_then(|m| price_of(m, &settings.prices));
    summary.saving = price
        .zip(summary.reference.clone())
        .filter(|_| summary.local.local_tokens > 0)
        .map(|(price, reference)| Saving {
            reference,
            cost: Cost {
                amount: local_rows()
                    .filter_map(|e| e.tokens.as_ref())
                    .map(|t| price.cost(t))
                    .sum(),
                currency: "USD".to_owned(),
            },
        });
    summary.models = models;
    summary
}

fn chart(
    rows: &[&UsageEntry],
    range: &Range,
    models: &[ModelRow],
    currency: Option<String>,
) -> Chart {
    let Some(currency) = currency else {
        return Chart::default();
    };
    let top: Vec<&ModelRow> = models
        .iter()
        .filter(|m| m.amount(&currency) > 0.0)
        .take(CHART_MODELS)
        .collect();
    let mut series: Vec<String> = top.iter().map(|m| m.name().to_owned()).collect();
    let has_other = models.iter().filter(|m| m.amount(&currency) > 0.0).count() > top.len();
    if has_other {
        series.push("Other".to_owned());
    }
    let mut bars: Vec<(String, Vec<f64>)> = range
        .buckets
        .iter()
        .map(|b| (b.label.clone(), vec![0.0; series.len()]))
        .collect();
    for e in rows {
        let Some(cost) = e.cost.as_ref().filter(|c| c.currency == currency) else {
            continue;
        };
        let Some(bar) = range.buckets.iter().rposition(|b| b.start <= e.at) else {
            continue;
        };
        let column = top
            .iter()
            .position(|m| m.model == e.model)
            .or(has_other.then_some(series.len().saturating_sub(1)));
        if let Some(column) = column {
            bars[bar].1[column] += cost.amount;
        }
    }
    Chart {
        currency: Some(currency),
        series,
        bars,
    }
}

#[cfg(test)]
mod tests;
