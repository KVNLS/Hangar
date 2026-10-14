use super::*;

/// `1.2M`, `34.5k`, `812`.
pub(super) fn tokens_label(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

pub(super) fn percent(ratio: f64) -> String {
    let p = ratio * 100.0;
    if p > 0.0 && p < 1.0 {
        "<1%".to_owned()
    } else {
        format!("{p:.0}%")
    }
}

pub(super) fn money(amount: f64, currency: &str) -> String {
    crate::chat_view::cost_label(&hangar_agents::acp::protocol::Cost {
        amount,
        currency: currency.to_owned(),
    })
}

/// What the two $/Mtok fields hold.
#[derive(Debug, PartialEq)]
pub(super) enum TypedPrice {
    /// Both blank.
    Empty,
    /// Something that is not a price.
    Invalid,
    /// Input and output $/Mtok; a blank field counts as 0, so an unpriced model can be priced
    /// one field at a time.
    Price(f64, f64),
}

pub(super) fn typed_price(input: &str, output: &str) -> TypedPrice {
    let parse = |text: &str| -> Result<Option<f64>, ()> {
        let text = text.trim().trim_start_matches('$').trim();
        if text.is_empty() {
            return Ok(None);
        }
        text.replace(',', ".")
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(Some)
            .ok_or(())
    };
    match (parse(input), parse(output)) {
        (Ok(None), Ok(None)) => TypedPrice::Empty,
        (Ok(input), Ok(output)) => {
            TypedPrice::Price(input.unwrap_or_default(), output.unwrap_or_default())
        }
        _ => TypedPrice::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::{TypedPrice, percent, tokens_label, typed_price};

    #[test]
    fn a_price_can_be_typed_one_field_at_a_time() {
        assert_eq!(typed_price("1.25", ""), TypedPrice::Price(1.25, 0.0));
        assert_eq!(typed_price("", "$10"), TypedPrice::Price(0.0, 10.0));
        assert_eq!(typed_price("1,50", "3"), TypedPrice::Price(1.5, 3.0));
        assert_eq!(typed_price(" ", ""), TypedPrice::Empty);
        assert_eq!(typed_price("abc", "3"), TypedPrice::Invalid);
        assert_eq!(typed_price("-1", "3"), TypedPrice::Invalid);
    }

    #[test]
    fn labels() {
        assert_eq!(tokens_label(812), "812");
        assert_eq!(tokens_label(34_500), "34.5k");
        assert_eq!(tokens_label(1_234_567), "1.2M");
        assert_eq!(percent(0.004), "<1%");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.426), "43%");
    }
}
