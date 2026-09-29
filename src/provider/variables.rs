//! Suggests variables: those in scope and a fresh one.

use super::{
    Candidate, CompletionProvider, CompletionRequest, ProviderError, Suggestion, TermKinds,
};

/// Proposes the variables of the current group, those of enclosing groups
/// and a fresh variable, with configurable relative weights.
///
/// Reusing variables creates joins; fresh variables widen patterns. This
/// provider is usually combined with a term provider in a
/// [`MixProvider`](super::MixProvider).
#[derive(Debug, Clone, PartialEq)]
pub struct VariableProvider {
    /// Total weight of the variables bound in the current group.
    pub current_group: f64,
    /// Total weight of the variables of enclosing groups.
    pub outer_groups: f64,
    /// Weight of the fresh variable.
    pub fresh: f64,
}

impl Default for VariableProvider {
    fn default() -> Self {
        Self {
            current_group: 3.0,
            outer_groups: 1.0,
            fresh: 2.0,
        }
    }
}

impl CompletionProvider for VariableProvider {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        if !request.accepts.contains(TermKinds::VARIABLE) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        #[allow(clippy::cast_precision_loss)] // variable counts are small
        let mut spread = |vars: &[crate::ast::Variable], total: f64| {
            if vars.is_empty() || total <= 0.0 {
                return;
            }
            let each = total / vars.len() as f64;
            out.extend(vars.iter().map(|v| Suggestion {
                candidate: Candidate::Variable(v.clone()),
                weight: each,
            }));
        };
        spread(request.variables, self.current_group);
        spread(request.outer_variables, self.outer_groups);
        if self.fresh > 0.0 {
            out.push(Suggestion {
                candidate: Candidate::Variable(request.fresh_variable.clone()),
                weight: self.fresh,
            });
        }
        Ok(out)
    }
}
