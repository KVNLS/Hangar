//! A session's context use and cost, folded from its `usage_update`s.

use crate::acp::protocol::{Cost, Usage};

/// The latest usage of one session. Agents send the running cost total (claude-agent-acp: the
/// SDK's `total_cost_usd`, OpenCode: the sum of its messages), so the latest amount is the
/// session's cost. When the total drops in the same currency the agent restarted its count, and
/// what it had reported so far is kept.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionUsage {
    pub used_tokens: Option<u64>,
    pub context_size: Option<u64>,
    reported: Option<Cost>,
    /// Totals from before the agent restarted its count, in `reported`'s currency.
    banked: f64,
}

impl SessionUsage {
    /// Fields an update leaves out keep their last value (claude-agent-acp sends cost only at
    /// the end of a turn).
    pub fn apply(&mut self, usage: &Usage) {
        if usage.used_tokens.is_some() {
            self.used_tokens = usage.used_tokens;
        }
        if usage.context_size.is_some() {
            self.context_size = usage.context_size;
        }
        let Some(cost) = &usage.cost else {
            return;
        };
        match &self.reported {
            Some(prev) if prev.currency == cost.currency => {
                if cost.amount < prev.amount {
                    self.banked += prev.amount;
                }
            }
            _ => self.banked = 0.0,
        }
        self.reported = Some(cost.clone());
    }

    /// Share of the context window in use, `0.0..` (above 1 if the agent says so).
    pub fn context_fraction(&self) -> Option<f64> {
        let size = self.context_size.filter(|s| *s > 0)?;
        Some(self.used_tokens? as f64 / size as f64)
    }

    /// What the session has cost so far, when the agent reports it.
    pub fn cost(&self) -> Option<Cost> {
        self.reported.as_ref().map(|c| Cost {
            amount: self.banked + c.amount,
            currency: c.currency.clone(),
        })
    }
}

/// Costs summed per currency, in first-seen order.
pub fn total_costs<'a>(costs: impl IntoIterator<Item = &'a Cost>) -> Vec<Cost> {
    let mut totals: Vec<Cost> = Vec::new();
    for cost in costs {
        match totals.iter_mut().find(|t| t.currency == cost.currency) {
            Some(total) => total.amount += cost.amount,
            None => totals.push(cost.clone()),
        }
    }
    totals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(amount: f64) -> Option<Cost> {
        Some(Cost {
            amount,
            currency: "USD".into(),
        })
    }

    fn update(used: Option<u64>, size: Option<u64>, cost: Option<Cost>) -> Usage {
        Usage {
            used_tokens: used,
            context_size: size,
            cost,
        }
    }

    #[test]
    fn keeps_the_latest_fields() {
        let mut usage = SessionUsage::default();
        assert_eq!(usage.context_fraction(), None);
        assert_eq!(usage.cost(), None);
        usage.apply(&update(Some(50_000), Some(200_000), usd(0.25)));
        // A mid-stream update without cost keeps the cost.
        usage.apply(&update(Some(100_000), None, None));
        assert_eq!(usage.context_fraction(), Some(0.5));
        assert_eq!(usage.cost(), usd(0.25));
        usage.apply(&update(None, Some(0), usd(0.4)));
        assert_eq!(usage.context_fraction(), None);
        assert_eq!(usage.cost(), usd(0.4));
    }

    #[test]
    fn a_restarted_total_adds_to_what_was_reported() {
        let mut usage = SessionUsage::default();
        usage.apply(&update(None, None, usd(1.0)));
        usage.apply(&update(None, None, usd(1.5)));
        usage.apply(&update(None, None, usd(0.25)));
        assert_eq!(usage.cost(), usd(1.75));
        usage.apply(&update(None, None, usd(0.5)));
        assert_eq!(usage.cost(), usd(2.0));
        // Another currency starts over rather than mixing amounts.
        let eur = Cost {
            amount: 0.1,
            currency: "EUR".into(),
        };
        usage.apply(&update(None, None, Some(eur.clone())));
        assert_eq!(usage.cost(), Some(eur));
    }

    #[test]
    fn totals_per_currency() {
        let costs = [
            Cost {
                amount: 0.5,
                currency: "USD".into(),
            },
            Cost {
                amount: 2.0,
                currency: "EUR".into(),
            },
            Cost {
                amount: 0.75,
                currency: "USD".into(),
            },
        ];
        let totals = total_costs(&costs);
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0].currency, "USD");
        assert!((totals[0].amount - 1.25).abs() < 1e-9);
        assert!(total_costs(&[]).is_empty());
    }
}
