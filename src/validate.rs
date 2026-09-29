//! Validation of generated requests with an independent parser.
//!
//! Generated requests are checked with [spargebra](https://crates.io/crates/spargebra),
//! which implements the SPARQL 1.1 grammar and many of its static
//! constraints (variable scoping of `BIND` and `AS`, projection in aggregate
//! queries, `VALUES` shapes, blank node label scoping, ...). A rejection
//! therefore usually means a bug in the generator — with the exception of
//! the known spargebra 0.4.7 bugs listed in [`KnownValidatorBug`], for which
//! the generator output is valid SPARQL 1.1.

use std::sync::LazyLock;

use regex::Regex;

/// Valid SPARQL 1.1 that spargebra 0.4.7 rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KnownValidatorBug {
    /// `GROUP BY (?var AS ?name)`: when the expression is a plain variable,
    /// the name is dropped and `?name` is then reported as unbound.
    GroupByVariableAlias,
    /// A blank node label inside `EXISTS` below a unary `!`, e.g.
    /// `FILTER(! EXISTS { _:b ?p ?o })`, is reported as already used: the
    /// parser backtracks over the operand of `!` after recording the label.
    BlankNodeInNegatedExists,
}

/// The result of validating a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validation {
    /// Accepted.
    Valid,
    /// Rejected, but the request matches a known validator bug.
    KnownBug(KnownValidatorBug),
    /// Rejected.
    Invalid(String),
}

impl Validation {
    /// The error message, unless the request is valid or matches a known bug.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Invalid(e) => Some(e),
            Self::Valid | Self::KnownBug(_) => None,
        }
    }
}

fn known_bug(text: &str) -> Option<KnownValidatorBug> {
    static GROUP_BY_ALIAS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"GROUP BY [^{}]*?\([?$][^ (),]+ AS ").expect("valid regex"));
    if GROUP_BY_ALIAS.is_match(text) {
        Some(KnownValidatorBug::GroupByVariableAlias)
    } else if text.contains("! ") && text.contains("EXISTS {") && text.contains("_:") {
        Some(KnownValidatorBug::BlankNodeInNegatedExists)
    } else {
        None
    }
}

fn classify(text: &str, result: Result<(), String>) -> Validation {
    match result {
        Ok(()) => Validation::Valid,
        Err(e) => known_bug(text).map_or(Validation::Invalid(e), Validation::KnownBug),
    }
}

/// Validates a query.
#[must_use]
pub fn validate_query(text: &str) -> Validation {
    let result = spargebra::SparqlParser::new()
        .parse_query(text)
        .map(drop)
        .map_err(|e| e.to_string());
    classify(text, result)
}

/// Validates an update request.
#[must_use]
pub fn validate_update(text: &str) -> Validation {
    let result = spargebra::SparqlParser::new()
        .parse_update(text)
        .map(drop)
        .map_err(|e| e.to_string());
    classify(text, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_spargebra_bugs_are_recognized() {
        let alias = "SELECT ?k WHERE { ?s ?p ?o } GROUP BY (?s AS ?k)";
        assert_eq!(
            validate_query(alias),
            Validation::KnownBug(KnownValidatorBug::GroupByVariableAlias)
        );
        let exists = "SELECT * { FILTER(! EXISTS { _:b1 <http://e/p> ?o }) }";
        assert_eq!(
            validate_query(exists),
            Validation::KnownBug(KnownValidatorBug::BlankNodeInNegatedExists)
        );
        assert_eq!(validate_query("SELECT * { ?s ?p ?o }"), Validation::Valid);
        assert!(validate_query("SELECT * { ?s ?p }").error().is_some());
    }
}
