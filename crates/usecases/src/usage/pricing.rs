//! What cloud models charge per token, to price what local models did for free.

use hangar_core::changes::TokenCount;
use serde::{Deserialize, Serialize};

/// USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl ModelPrice {
    /// Cache reads and writes at their usual share of the input price (0.1× and 1.25×).
    pub const fn from_io(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_read: input * 0.1,
            cache_write: input * 1.25,
        }
    }

    /// USD for `tokens` at this price.
    pub fn cost(&self, tokens: &TokenCount) -> f64 {
        (tokens.input as f64 * self.input
            + tokens.output as f64 * self.output
            + tokens.cache_read as f64 * self.cache_read
            + tokens.cache_write as f64 * self.cache_write)
            / 1_000_000.0
    }
}

/// A price the user set for a model id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomPrice {
    pub model: String,
    pub price: ModelPrice,
}

/// Anthropic API list prices (first-party, as of 2026-10-06). Ids match as prefixes, so dated
/// snapshots (`claude-sonnet-4-6-20260101`) find their model.
pub const BUILT_IN: &[(&str, ModelPrice)] = &[
    (
        "claude-fable-5-1",
        ModelPrice {
            cache_read: 0.25,
            ..ModelPrice::from_io(10.0, 50.0)
        },
    ),
    ("claude-fable-5", ModelPrice::from_io(10.0, 50.0)),
    (
        "claude-opus-5-5",
        ModelPrice {
            cache_read: 0.2,
            ..ModelPrice::from_io(4.0, 20.0)
        },
    ),
    ("claude-opus-5", ModelPrice::from_io(5.0, 25.0)),
    ("claude-opus-4-8", ModelPrice::from_io(5.0, 25.0)),
    ("claude-opus-4-7", ModelPrice::from_io(5.0, 25.0)),
    ("claude-opus-4-6", ModelPrice::from_io(5.0, 25.0)),
    ("claude-sonnet-5-5", ModelPrice::from_io(2.0, 10.0)),
    ("claude-sonnet-5", ModelPrice::from_io(2.0, 10.0)),
    ("claude-sonnet-4-6", ModelPrice::from_io(3.0, 15.0)),
    // Prompts up to 100K tokens; longer ones cost 5× more.
    ("claude-haiku-5-5", ModelPrice::from_io(0.1, 0.5)),
    ("claude-haiku-4-5", ModelPrice::from_io(1.0, 5.0)),
];

/// The price of `model`: the user's own first, else the longest built-in id it starts with.
/// A provider prefix (`anthropic/claude-…`) is ignored.
pub fn price_of(model: &str, custom: &[CustomPrice]) -> Option<ModelPrice> {
    if let Some(own) = custom.iter().find(|c| c.model == model) {
        return Some(own.price);
    }
    let bare = model.rsplit_once('/').map_or(model, |(_, m)| m);
    BUILT_IN
        .iter()
        .filter(|(id, _)| bare.starts_with(id))
        .max_by_key(|(id, _)| id.len())
        .map(|(_, price)| *price)
}
