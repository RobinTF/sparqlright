//! Recording and replaying provider answers.
//!
//! Generation guided by a live engine depends on the engine's data and
//! answers. Recording those answers makes a run reproducible: replaying the
//! log with the same seed and configuration regenerates the same queries
//! without the engine.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::{CompletionProvider, CompletionRequest, Namespace, ProviderError, Slot, Suggestion};

/// One answered request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedCall {
    /// The requested slot.
    pub slot: Slot,
    /// The partial query at the time of the request.
    pub partial_query: String,
    /// The answer.
    pub suggestions: Vec<Suggestion>,
}

/// Wraps a provider and records every answer.
#[derive(Debug)]
pub struct RecordingProvider<P> {
    inner: P,
    namespaces: Option<Vec<Namespace>>,
    calls: Vec<RecordedCall>,
}

impl<P: CompletionProvider> RecordingProvider<P> {
    /// Wraps `inner`.
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            namespaces: None,
            calls: Vec::new(),
        }
    }

    /// The calls recorded so far.
    #[must_use]
    pub fn calls(&self) -> &[RecordedCall] {
        &self.calls
    }

    /// Takes the recorded calls, leaving the log empty.
    pub fn take_calls(&mut self) -> Vec<RecordedCall> {
        std::mem::take(&mut self.calls)
    }

    /// The namespaces answered by the wrapped provider, if requested.
    #[must_use]
    pub fn recorded_namespaces(&self) -> Option<&[Namespace]> {
        self.namespaces.as_deref()
    }

    /// Returns the wrapped provider.
    pub fn into_inner(self) -> P {
        self.inner
    }
}

impl<P: CompletionProvider> CompletionProvider for RecordingProvider<P> {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        // Errors are recorded as empty answers, which is how the generator
        // treats them as well.
        let suggestions = self.inner.suggest(request).unwrap_or_default();
        self.calls.push(RecordedCall {
            slot: request.slot,
            partial_query: request.partial_query.to_owned(),
            suggestions: suggestions.clone(),
        });
        Ok(suggestions)
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        let namespaces = self.inner.namespaces()?;
        self.namespaces = Some(namespaces.clone());
        Ok(namespaces)
    }
}

/// Replays recorded answers in order.
///
/// Each request must match the next recorded call (same slot and partial
/// query); otherwise [`ProviderError::ReplayDiverged`] is returned.
#[derive(Debug, Clone, Default)]
pub struct ReplayProvider {
    calls: VecDeque<RecordedCall>,
    namespaces: Vec<Namespace>,
}

impl ReplayProvider {
    /// Creates a replay of `calls`, answering `namespaces` for the prologue.
    #[must_use]
    pub fn new(calls: Vec<RecordedCall>, namespaces: Vec<Namespace>) -> Self {
        Self {
            calls: calls.into(),
            namespaces,
        }
    }

    /// The number of calls not yet replayed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.calls.len()
    }
}

impl CompletionProvider for ReplayProvider {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        let Some(call) = self.calls.pop_front() else {
            return Err(ProviderError::ReplayDiverged("log exhausted".to_owned()));
        };
        if call.slot != request.slot || call.partial_query != request.partial_query {
            return Err(ProviderError::ReplayDiverged(format!(
                "expected {} at {:?}, got {} at {:?}",
                call.slot, call.partial_query, request.slot, request.partial_query
            )));
        }
        Ok(call.suggestions)
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        Ok(self.namespaces.clone())
    }
}
