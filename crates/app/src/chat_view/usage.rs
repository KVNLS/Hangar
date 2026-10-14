//! Short labels for a structured agent's context use and cost.

use hangar_agents::acp::protocol::Cost;
use hangar_agents::acp::usage::SessionUsage;

/// `ctx 42% · $0.31`, with whichever half is known; `None` when neither is.
pub fn usage_label(usage: &SessionUsage) -> Option<String> {
    let parts: Vec<String> = usage
        .context_fraction()
        .map(context_label)
        .into_iter()
        .chain(usage.cost().as_ref().map(cost_label))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `ctx 42%` for a context window `fraction` in use; `ctx <1%` for a sliver.
pub fn context_label(fraction: f64) -> String {
    let percent = (fraction.max(0.0) * 100.0).round();
    if percent == 0.0 && fraction > 0.0 {
        "ctx <1%".to_owned()
    } else {
        format!("ctx {percent:.0}%")
    }
}

/// `$0.31`, `€2.40`, `$128`, `<$0.01`, or `3.10 CHF` for currencies without a common sign.
pub fn cost_label(cost: &Cost) -> String {
    let amount = cost.amount.max(0.0);
    let sign = match cost.currency.as_str() {
        "USD" => Some("$"),
        "EUR" => Some("€"),
        "GBP" => Some("£"),
        _ => None,
    };
    let (prefix, number) = if amount > 0.0 && amount < 0.005 {
        ("<", "0.01".to_owned())
    } else if amount >= 100.0 {
        ("", format!("{amount:.0}"))
    } else {
        ("", format!("{amount:.2}"))
    };
    match sign {
        Some(sign) => format!("{prefix}{sign}{number}"),
        None => format!("{prefix}{number} {}", cost.currency),
    }
}

/// The costs of several sessions, one label per currency: `$1.24`, `$1.24 · €0.40`.
pub fn costs_label(costs: &[Cost]) -> Option<String> {
    (!costs.is_empty()).then(|| costs.iter().map(cost_label).collect::<Vec<_>>().join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(amount: f64, currency: &str) -> Cost {
        Cost {
            amount,
            currency: currency.into(),
        }
    }

    #[test]
    fn context_percent() {
        assert_eq!(context_label(0.42), "ctx 42%");
        assert_eq!(context_label(0.4249), "ctx 42%");
        assert_eq!(context_label(0.996), "ctx 100%");
        assert_eq!(context_label(0.0), "ctx 0%");
        assert_eq!(context_label(0.001), "ctx <1%");
        assert_eq!(context_label(1.2), "ctx 120%");
    }

    #[test]
    fn currency() {
        assert_eq!(cost_label(&cost(0.3114, "USD")), "$0.31");
        assert_eq!(cost_label(&cost(0.0, "USD")), "$0.00");
        assert_eq!(cost_label(&cost(0.004, "USD")), "<$0.01");
        assert_eq!(cost_label(&cost(2.4, "EUR")), "€2.40");
        assert_eq!(cost_label(&cost(127.6, "USD")), "$128");
        assert_eq!(cost_label(&cost(3.1, "CHF")), "3.10 CHF");
        assert_eq!(cost_label(&cost(0.001, "CHF")), "<0.01 CHF");
    }

    #[test]
    fn combined_label() {
        use hangar_agents::acp::protocol::Usage;
        let mut usage = SessionUsage::default();
        assert_eq!(usage_label(&usage), None);
        usage.apply(&Usage {
            used_tokens: Some(84_000),
            context_size: Some(200_000),
            cost: None,
        });
        assert_eq!(usage_label(&usage).as_deref(), Some("ctx 42%"));
        usage.apply(&Usage {
            cost: Some(cost(0.31, "USD")),
            ..Usage::default()
        });
        assert_eq!(usage_label(&usage).as_deref(), Some("ctx 42% · $0.31"));
    }

    #[test]
    fn several_currencies() {
        assert_eq!(costs_label(&[]), None);
        assert_eq!(
            costs_label(&[cost(1.24, "USD"), cost(0.4, "EUR")]).as_deref(),
            Some("$1.24 · €0.40")
        );
    }
}
