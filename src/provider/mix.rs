//! Weighted combination of providers.

use std::collections::BTreeMap;
use std::fmt;

use super::{CompletionProvider, CompletionRequest, Namespace, ProviderError, Slot, Suggestion};

struct Entry {
    provider: Box<dyn CompletionProvider>,
    default_weight: f64,
    slot_weights: BTreeMap<Slot, f64>,
}

/// Combines providers: each provider's suggestions are normalized to sum up
/// to the provider's weight for the requested slot, then all are merged.
///
/// A failing provider is skipped as long as another one succeeds, so an
/// engine provider can be backed by a deterministic fallback.
#[derive(Default)]
pub struct MixProvider {
    entries: Vec<Entry>,
    namespaces_from: Option<usize>,
}

impl fmt::Debug for MixProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MixProvider")
            .field("providers", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl MixProvider {
    /// Creates an empty mix.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a provider with the same weight for every slot.
    #[must_use]
    pub fn with(self, provider: impl CompletionProvider + 'static, weight: f64) -> Self {
        self.with_slot_weights(provider, weight, [])
    }

    /// Adds a provider with a default weight and per-slot overrides.
    #[must_use]
    pub fn with_slot_weights(
        mut self,
        provider: impl CompletionProvider + 'static,
        default_weight: f64,
        slot_weights: impl IntoIterator<Item = (Slot, f64)>,
    ) -> Self {
        self.entries.push(Entry {
            provider: Box::new(provider),
            default_weight,
            slot_weights: slot_weights.into_iter().collect(),
        });
        self
    }

    /// The default combination of [`VariableProvider`](super::VariableProvider)
    /// with a provider of terms.
    ///
    /// The share of variables depends on the slot: subjects are usually
    /// variables, predicates usually IRIs (so that patterns match data), and
    /// expression operands and `VALUES` cells never come from the variable
    /// provider (the generator chooses variables in expressions itself, and
    /// `VALUES` cells cannot be variables).
    #[must_use]
    pub fn variables_and(terms: impl CompletionProvider + 'static) -> Self {
        let variable_share = [
            (Slot::Subject, 0.6),
            (Slot::Predicate, 0.2),
            (Slot::Object, 0.5),
            (Slot::PathIri, 0.0),
            (Slot::GraphName, 0.5),
            (Slot::ServiceEndpoint, 0.0),
            (Slot::DescribeTarget, 0.5),
            (Slot::ExpressionOperand, 0.0),
            (Slot::ValuesCell, 0.0),
        ];
        Self::new()
            .with_slot_weights(super::VariableProvider::default(), 0.5, variable_share)
            .with_slot_weights(
                terms,
                0.5,
                variable_share.map(|(slot, share)| (slot, 1.0 - share)),
            )
    }

    /// Takes the namespaces from the most recently added provider only
    /// (by default, the namespaces of all providers are merged).
    #[must_use]
    pub fn namespaces_from_last(mut self) -> Self {
        self.namespaces_from = self.entries.len().checked_sub(1);
        self
    }
}

impl CompletionProvider for MixProvider {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        let mut out = Vec::new();
        let mut first_error = None;
        let mut any_ok = false;
        for entry in &mut self.entries {
            let weight = entry
                .slot_weights
                .get(&request.slot)
                .copied()
                .unwrap_or(entry.default_weight);
            if weight <= 0.0 {
                continue;
            }
            match entry.provider.suggest(request) {
                Ok(suggestions) => {
                    any_ok = true;
                    let total: f64 = suggestions.iter().map(|s| s.weight.max(0.0)).sum();
                    if total > 0.0 {
                        out.extend(suggestions.into_iter().filter(|s| s.weight > 0.0).map(|s| {
                            Suggestion {
                                weight: s.weight / total * weight,
                                ..s
                            }
                        }));
                    }
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) if !any_ok => Err(e),
            _ => Ok(out),
        }
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        if let Some(i) = self.namespaces_from {
            return self.entries[i].provider.namespaces();
        }
        let mut all: Vec<Namespace> = Vec::new();
        for entry in &mut self.entries {
            for ns in entry.provider.namespaces()? {
                if !all.contains(&ns) {
                    all.push(ns);
                }
            }
        }
        Ok(all)
    }
}
