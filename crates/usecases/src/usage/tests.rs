use super::pricing::{BUILT_IN, ModelPrice};
use super::*;

/// 2026-10-08 (a Thursday) 00:00 UTC.
const OCT_8: i64 = 1_791_417_600_000;

fn usd(amount: f64) -> Cost {
    Cost {
        amount,
        currency: "USD".into(),
    }
}

fn tokens(input: u64, output: u64) -> TokenCount {
    TokenCount {
        input,
        output,
        ..TokenCount::default()
    }
}

fn row(
    session: SessionId,
    at: i64,
    model: &str,
    cost: Option<f64>,
    t: Option<TokenCount>,
) -> UsageEntry {
    UsageEntry {
        session_id: session,
        at: Timestamp(at),
        model: Some(model.into()),
        local: is_local(model),
        cost: cost.map(usd),
        tokens: t,
    }
}

#[test]
fn civil_dates_round_trip() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(days_from_civil(2026, 10, 8) * DAY_MS, OCT_8);
    assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    assert_eq!(weekday(days_from_civil(2026, 10, 8)), 3);
}

#[test]
fn one_model_takes_the_whole_turn() {
    let s = SessionId::new();
    let spend = TurnSpend {
        cost: Some(usd(0.3)),
        tokens: Some(tokens(10, 20)),
        models: Vec::new(),
    };
    let rows = book_turn(s, Timestamp(5), spend, Some("ollama/qwen3"));
    assert_eq!(
        rows,
        [row(s, 5, "ollama/qwen3", Some(0.3), Some(tokens(10, 20)))]
    );
    assert!(book_turn(s, Timestamp(5), TurnSpend::default(), Some("m")).is_empty());
    // No model known at all.
    let rows = book_turn(
        s,
        Timestamp(5),
        TurnSpend {
            cost: Some(usd(0.1)),
            ..TurnSpend::default()
        },
        None,
    );
    assert_eq!(rows[0].model, None);
}

#[test]
fn several_models_share_cost_and_tokens_by_their_tokens() {
    let s = SessionId::new();
    let spend = TurnSpend {
        cost: Some(usd(1.0)),
        tokens: Some(tokens(101, 7)),
        models: vec![("opus".into(), 750), ("haiku".into(), 250)],
    };
    let rows = book_turn(s, Timestamp(1), spend, Some("ignored"));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].model.as_deref(), Some("opus"));
    assert!((rows[0].cost.as_ref().unwrap().amount - 0.75).abs() < 1e-9);
    assert!((rows[1].cost.as_ref().unwrap().amount - 0.25).abs() < 1e-9);
    let mut sum = TokenCount::default();
    for r in &rows {
        sum += r.tokens.unwrap();
    }
    assert_eq!(sum, tokens(101, 7));
    assert_eq!(rows[0].tokens.unwrap().input, 76);
}

#[test]
fn rounding_never_gives_out_more_tokens_than_the_turn_has() {
    let spend = TurnSpend {
        cost: None,
        tokens: Some(tokens(2, 2)),
        models: (0..4).map(|i| (format!("m{i}"), 1)).collect(),
    };
    let rows = book_turn(SessionId::new(), Timestamp(1), spend, None);
    let mut sum = TokenCount::default();
    for r in &rows {
        sum += r.tokens.unwrap();
    }
    assert_eq!(sum, tokens(2, 2));
}

#[test]
fn huge_model_weights_do_not_overflow() {
    let spend = TurnSpend {
        cost: Some(usd(1.0)),
        tokens: Some(tokens(10, 0)),
        models: vec![("a".into(), u64::MAX), ("b".into(), u64::MAX)],
    };
    let rows = book_turn(SessionId::new(), Timestamp(1), spend, None);
    assert_eq!(rows.len(), 2);
}

#[test]
fn ranges_cover_local_periods() {
    let now = Timestamp(OCT_8 + 10 * HOUR_MS);
    // UTC+2: local midnight is 22:00 UTC the day before.
    let day = range(Period::Day, now, 7200, 0);
    assert_eq!(day.title, "Today");
    assert_eq!(day.start, Timestamp(OCT_8 - 2 * HOUR_MS));
    assert_eq!(day.end.0 - day.start.0, DAY_MS);
    assert_eq!(day.buckets.len(), 24);
    assert_eq!(range(Period::Day, now, 0, 3).title, "Oct 5, 2026");

    let week = range(Period::Week, now, 0, 0);
    assert_eq!(week.start, Timestamp(OCT_8 - 3 * DAY_MS));
    assert_eq!(week.buckets[0].label, "Mon");
    assert_eq!(range(Period::Week, now, 0, 2).title, "Week of Sep 21, 2026");

    let month = range(Period::Month, now, 0, 0);
    assert_eq!(month.title, "October 2026");
    assert_eq!(month.buckets.len(), 31);
    let feb = range(Period::Month, now, 0, 8);
    assert_eq!(feb.title, "February 2026");
    assert_eq!(feb.buckets.len(), 28);
    assert_eq!(range(Period::Month, now, 0, 10).title, "December 2025");

    let year = range(Period::Year, now, 0, 1);
    assert_eq!(year.title, "2025");
    assert_eq!(year.buckets.len(), 12);
    assert_eq!(year.end, Timestamp(days_from_civil(2026, 1, 1) * DAY_MS));
}

#[test]
fn summary_per_model_with_local_share_and_saving() {
    let (a, b) = (SessionId::new(), SessionId::new());
    let day = range(Period::Day, Timestamp(OCT_8 + HOUR_MS), 0, 0);
    let h = |hours: i64| OCT_8 + hours * HOUR_MS;
    let entries = [
        row(a, h(-1), "claude-opus-5-5", Some(9.0), Some(tokens(1, 1))), // yesterday
        row(
            a,
            h(1),
            "claude-opus-5-5",
            Some(0.75),
            Some(tokens(1_000, 500)),
        ),
        row(
            a,
            h(1),
            "claude-haiku-5-5",
            Some(0.25),
            Some(tokens(500, 0)),
        ),
        row(
            b,
            h(3),
            "ollama/qwen3",
            None,
            Some(tokens(1_000_000, 1_000_000)),
        ),
        row(b, h(4), "ollama/qwen3", None, None),
        UsageEntry {
            cost: Some(Cost {
                amount: 2.0,
                currency: "EUR".into(),
            }),
            ..row(b, h(5), "gpt-5", None, None)
        },
    ];
    let summary = summarize(&entries, &day, &UsageSettings::default());
    assert_eq!(summary.turns, 4);
    assert_eq!(summary.totals.len(), 2);
    let names: Vec<&str> = summary.models.iter().map(ModelRow::name).collect();
    // Sorted by the USD/EUR main currency (EUR here: 2.0 > 1.0), then tokens.
    assert_eq!(names[0], "gpt-5");
    assert_eq!(summary.chart.currency.as_deref(), Some("EUR"));
    assert_eq!(summary.chart.bars[5].1, vec![2.0]);

    let qwen = summary.models.iter().find(|m| m.local).unwrap();
    assert_eq!(qwen.turns, 2);
    assert!(summary.local.local_tokens_unknown);
    assert_eq!(summary.local.local_turns, 2);
    let ratio = summary.local.ratio().unwrap();
    assert!((ratio - 2_000_000.0 / 2_002_000.0).abs() < 1e-9);

    // The reference defaults to the priced cloud model with the most tokens.
    assert_eq!(summary.reference.as_deref(), Some("claude-opus-5-5"));
    let saving = summary.saving.unwrap();
    assert!((saving.cost.amount - 24.0).abs() < 1e-9);

    let custom = UsageSettings {
        reference_model: Some("my-model".into()),
        prices: vec![pricing::CustomPrice {
            model: "my-model".into(),
            price: ModelPrice::from_io(1.0, 1.0),
        }],
        ..UsageSettings::default()
    };
    let saving = summarize(&entries, &day, &custom).saving.unwrap();
    assert!((saving.cost.amount - 2.0).abs() < 1e-9);
}

#[test]
fn chart_keeps_the_top_models_and_sums_the_rest() {
    let s = SessionId::new();
    let day = range(Period::Day, Timestamp(OCT_8), 0, 0);
    let entries: Vec<UsageEntry> = (0..7)
        .map(|i| row(s, OCT_8 + i, &format!("m{i}"), Some(1.0 + i as f64), None))
        .collect();
    let summary = summarize(&entries, &day, &UsageSettings::default());
    assert_eq!(
        summary.chart.series,
        ["m6", "m5", "m4", "m3", "m2", "Other"]
    );
    assert_eq!(summary.chart.bars[0].1, vec![7.0, 6.0, 5.0, 4.0, 3.0, 3.0]);
    assert!(summary.saving.is_none());
    assert_eq!(summary.local.ratio(), Some(0.0));
}

#[test]
fn prices_match_the_longest_id() {
    let opus55 = price_of("claude-opus-5-5-20260901", &[]).unwrap();
    assert_eq!(opus55.input, 4.0);
    assert_eq!(opus55.cache_read, 0.2);
    assert_eq!(price_of("anthropic/claude-opus-5", &[]).unwrap().input, 5.0);
    assert_eq!(price_of("gpt-5", &[]), None);
    assert!(BUILT_IN.iter().all(|(_, p)| p.output >= p.input));
    let million = TokenCount {
        input: 1_000_000,
        output: 1_000_000,
        cache_read: 1_000_000,
        cache_write: 1_000_000,
    };
    assert!(
        (ModelPrice::from_io(2.0, 10.0).cost(&million) - (2.0 + 10.0 + 0.2 + 2.5)).abs() < 1e-9
    );
}

#[test]
fn custom_prices_match_like_built_in_ones() {
    let custom = |model: &str, input| CustomPrice {
        model: model.into(),
        price: ModelPrice::from_io(input, 1.0),
    };
    let prices = [custom("gpt-5", 1.25), custom("claude-opus-5", 9.0)];
    assert_eq!(price_of("gpt-5", &prices).unwrap().input, 1.25);
    assert_eq!(
        price_of("openai/gpt-5-2025-08-07", &prices).unwrap().input,
        1.25
    );
    // The user's price wins over the built-in one for dated and prefixed ids too.
    assert_eq!(
        price_of("anthropic/claude-opus-5-20260101", &prices)
            .unwrap()
            .input,
        9.0
    );
    assert_eq!(price_of("gpt-4o", &prices), None);
}
