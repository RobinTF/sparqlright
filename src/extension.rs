//! Extension points for engine-specific features.
//!
//! Many engines extend SPARQL 1.1 with functions (e.g. `ql:` or `bif:`
//! functions), "magic" predicates or services. Most of these extensions are
//! written in standard syntax — a function call is an IRI with arguments, a
//! magic predicate is an IRI in a triple pattern — so an [`Extension`] only
//! has to tell the generator *what* to generate:
//!
//! * [`Extension::functions`] adds callable functions with their signatures;
//! * [`Extension::namespaces`] adds prefixes to declare;
//! * [`Extension::generate_pattern`] can produce whole group elements, e.g. a
//!   `SERVICE` call with engine-specific configuration.
//!
//! No extensions ship with the crate yet; generated queries are standard
//! SPARQL 1.1 unless extensions are registered with
//! [`Generator::with_extension`](crate::Generator::with_extension).

use std::fmt::Debug;

use oxrdf::NamedNode;

use crate::ast::{GraphPatternNotTriples, Variable};
use crate::provider::{Namespace, ValueType};

/// The signature of a function callable as `<iri>(args)`.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSpec {
    /// The function IRI.
    pub iri: NamedNode,
    /// The parameter types. With `variadic`, the last type may repeat.
    pub params: Vec<ValueType>,
    /// Whether the last parameter may be repeated (or omitted).
    pub variadic: bool,
    /// The result type.
    pub returns: ValueType,
    /// The relative weight when the generator picks a function.
    pub weight: f64,
}

impl FunctionSpec {
    /// Creates a function with fixed arity and weight 1.
    #[must_use]
    pub fn new(iri: NamedNode, params: Vec<ValueType>, returns: ValueType) -> Self {
        Self {
            iri,
            params,
            variadic: false,
            returns,
            weight: 1.0,
        }
    }
}

/// What an extension needs to know to generate a group element.
#[non_exhaustive]
pub struct PatternContext<'a> {
    /// A source of randomness; use it for all random choices so that
    /// generation stays reproducible.
    pub rng: &'a mut dyn rand::Rng,
    /// Variables bound in the current group so far.
    pub variables: &'a [Variable],
    /// A variable that is unused so far.
    pub fresh_variable: &'a Variable,
}

impl std::fmt::Debug for PatternContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatternContext")
            .field("variables", &self.variables)
            .field("fresh_variable", &self.fresh_variable)
            .finish_non_exhaustive()
    }
}

/// A group element produced by an extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionPattern {
    /// The element, written in standard syntax.
    pub pattern: GraphPatternNotTriples,
    /// Variables bound by the element, which later elements may use.
    pub binds: Vec<Variable>,
}

/// An engine-specific extension of the generator.
///
/// All methods have defaults that add nothing, so an extension only
/// implements what it needs.
pub trait Extension: Debug {
    /// A short name, used in statistics.
    fn name(&self) -> &str;

    /// Functions the generator may call.
    fn functions(&self) -> Vec<FunctionSpec> {
        Vec::new()
    }

    /// Namespaces to declare in the prologue.
    fn namespaces(&self) -> Vec<Namespace> {
        Vec::new()
    }

    /// The relative weight of [`Extension::generate_pattern`] among the other
    /// group elements (see [`PatternWeights`](crate::config::PatternWeights)).
    fn pattern_weight(&self) -> f64 {
        0.0
    }

    /// Generates a group element, or `None` if not possible here.
    fn generate_pattern(&self, _context: &mut PatternContext<'_>) -> Option<ExtensionPattern> {
        None
    }
}
