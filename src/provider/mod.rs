//! Completion providers: the pluggable source of values during generation.
//!
//! Whenever the generator reaches a position where a *value* is needed — the
//! subject, predicate or object of a triple pattern, an IRI inside a property
//! path, a constant in an expression, a cell of a `VALUES` block, a graph
//! name, ... — it builds a [`CompletionRequest`] and asks its
//! [`CompletionProvider`] for [`Suggestion`]s. The request contains the
//! partial query text written so far (e.g. `SELECT * WHERE { ?s ?p`) together
//! with structured context, so that providers can be as simple or as smart as
//! needed:
//!
//! * [`EngineProvider`] asks the SPARQL engine under test (context-sensitive
//!   autocompletion, as in the `QLever` UI);
//! * [`GraphProvider`] answers from an in-memory RDF dataset, deterministically;
//! * [`FixedProvider`] returns a fixed vocabulary, for tests;
//! * [`VariableProvider`] proposes variables that are in scope or fresh;
//! * [`MixProvider`] combines several providers with per-slot weights;
//! * [`RecordingProvider`] and [`ReplayProvider`] record and replay the
//!   answers of any provider, which makes engine-guided runs reproducible.
//!
//! Only the generator decides which suggestion is taken, so a provider never
//! has to know about grammar rules or randomness. Suggestions that are not
//! valid at the requested position are discarded by the generator, which
//! falls back to a built-in vocabulary if nothing usable remains.

mod engine;
mod fixed;
mod graph;
mod mix;
mod replay;
mod variables;

use std::fmt;

use bitflags::bitflags;
use oxrdf::{NamedNode, Term};
use serde::{Deserialize, Serialize};

use crate::ast::Variable;

pub use engine::{EngineProvider, EngineProviderConfig};
pub use fixed::FixedProvider;
pub use graph::GraphProvider;
pub use mix::MixProvider;
pub use replay::{RecordedCall, RecordingProvider, ReplayProvider};
pub use variables::VariableProvider;

/// The grammar position a value is requested for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Slot {
    /// The subject of a triple pattern or template.
    Subject,
    /// The predicate of a triple pattern or template.
    Predicate,
    /// The object of a triple pattern or template.
    Object,
    /// An IRI inside a property path, e.g. `ex:q` in `ex:p/ex:q`.
    PathIri,
    /// The graph of `GRAPH`, `FROM`, `FROM NAMED`, `USING`, `WITH` or a graph
    /// management operation.
    GraphName,
    /// The endpoint of a `SERVICE` pattern.
    ServiceEndpoint,
    /// A resource described by `DESCRIBE`.
    DescribeTarget,
    /// An operand of an expression, e.g. the right-hand side of `?o < _`.
    ExpressionOperand,
    /// A cell of a `VALUES` block.
    ValuesCell,
}

impl Slot {
    /// All slots.
    pub const ALL: [Self; 9] = [
        Self::Subject,
        Self::Predicate,
        Self::Object,
        Self::PathIri,
        Self::GraphName,
        Self::ServiceEndpoint,
        Self::DescribeTarget,
        Self::ExpressionOperand,
        Self::ValuesCell,
    ];
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

bitflags! {
    /// The kinds of values that are syntactically allowed at a position.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct TermKinds: u8 {
        /// A variable.
        const VARIABLE = 1;
        /// An IRI.
        const IRI = 1 << 1;
        /// A literal.
        const LITERAL = 1 << 2;
        /// A blank node.
        const BLANK_NODE = 1 << 3;
    }
}

/// A coarse value type used as a hint for expression operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ValueType {
    /// No constraint.
    #[default]
    Any,
    /// `xsd:boolean`
    Boolean,
    /// A numeric type.
    Numeric,
    /// A string (`xsd:string` or language-tagged string).
    String,
    /// A simple literal / `xsd:string`.
    SimpleString,
    /// `xsd:dateTime`
    DateTime,
    /// An IRI.
    Iri,
    /// Any literal.
    Literal,
}

impl ValueType {
    /// Whether a value of type `produced` satisfies a requirement of type `self`.
    #[must_use]
    pub fn accepts(self, produced: Self) -> bool {
        match self {
            Self::Any => true,
            Self::Literal => !matches!(produced, Self::Iri | Self::Any),
            Self::String => matches!(produced, Self::String | Self::SimpleString),
            _ => self == produced,
        }
    }

    /// The value type of an RDF term.
    #[must_use]
    pub fn of_term(term: &Term) -> Self {
        use oxrdf::vocab::{rdf, xsd};
        match term {
            Term::NamedNode(_) => Self::Iri,
            Term::BlankNode(_) => Self::Any,
            Term::Literal(l) => {
                let dt = l.datatype();
                if dt == xsd::STRING {
                    Self::SimpleString
                } else if dt == rdf::LANG_STRING {
                    Self::String
                } else if dt == xsd::BOOLEAN {
                    Self::Boolean
                } else if dt == xsd::DATE_TIME {
                    Self::DateTime
                } else if is_numeric_datatype(dt.as_str()) {
                    Self::Numeric
                } else {
                    Self::Literal
                }
            }
        }
    }
}

/// Whether `datatype` is one of the XSD numeric types.
pub(crate) fn is_numeric_datatype(datatype: &str) -> bool {
    datatype
        .strip_prefix("http://www.w3.org/2001/XMLSchema#")
        .is_some_and(|local| {
            matches!(
                local,
                "integer"
                    | "decimal"
                    | "float"
                    | "double"
                    | "nonPositiveInteger"
                    | "negativeInteger"
                    | "long"
                    | "int"
                    | "short"
                    | "byte"
                    | "nonNegativeInteger"
                    | "unsignedLong"
                    | "unsignedInt"
                    | "unsignedShort"
                    | "unsignedByte"
                    | "positiveInteger"
            )
        })
}

/// A term of a triple pattern in the context of a request.
///
/// IRIs are fully resolved; blank node labels of the query are given as
/// blank nodes (they behave like variables in a pattern).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContextTerm {
    /// A variable.
    Variable(Variable),
    /// A concrete term.
    Term(Term),
}

/// The predicate of a triple pattern in the context of a request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContextPredicate {
    /// A variable.
    Variable(Variable),
    /// A plain IRI.
    Iri(NamedNode),
    /// A property path in SPARQL syntax; prefixed names in it are declared
    /// in [`CompletionRequest::prologue`].
    Path(String),
}

/// A complete triple pattern of the partial query.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContextTriple {
    /// The subject.
    pub subject: ContextTerm,
    /// The predicate.
    pub predicate: ContextPredicate,
    /// The object.
    pub object: ContextTerm,
}

/// The triple pattern that is currently being written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PartialTriple {
    /// The subject, once known.
    pub subject: Option<ContextTerm>,
    /// The predicate, once known.
    pub predicate: Option<ContextPredicate>,
}

/// Everything a provider may use to propose values.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompletionRequest<'a> {
    /// The query text written so far; the requested value comes next.
    pub partial_query: &'a str,
    /// The `BASE`/`PREFIX` declarations of the query, as SPARQL text.
    pub prologue: &'a str,
    /// The position the value is for.
    pub slot: Slot,
    /// The kinds of values allowed at this position.
    pub accepts: TermKinds,
    /// The type the generator would like the value to have.
    pub expected: ValueType,
    /// For [`Slot::ExpressionOperand`] and [`Slot::ValuesCell`]: the variable
    /// whose values are relevant (e.g. `?o` in `?o < _`).
    pub related_variable: Option<&'a Variable>,
    /// The triple being written, for the triple slots.
    pub partial_triple: Option<&'a PartialTriple>,
    /// Complete triple patterns of the enclosing groups, innermost last.
    pub context: &'a [ContextTriple],
    /// Variables bound in the current group so far.
    pub variables: &'a [Variable],
    /// Variables of enclosing groups.
    pub outer_variables: &'a [Variable],
    /// A variable name that is unused so far, for providers that want to
    /// introduce a new variable.
    pub fresh_variable: &'a Variable,
}

/// A proposed value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Candidate {
    /// A variable (possibly [`CompletionRequest::fresh_variable`]).
    Variable(Variable),
    /// An RDF term.
    Term(Term),
}

impl Candidate {
    /// The kind of this candidate.
    #[must_use]
    pub fn kind(&self) -> TermKinds {
        match self {
            Self::Variable(_) => TermKinds::VARIABLE,
            Self::Term(Term::NamedNode(_)) => TermKinds::IRI,
            Self::Term(Term::Literal(_)) => TermKinds::LITERAL,
            Self::Term(Term::BlankNode(_)) => TermKinds::BLANK_NODE,
        }
    }
}

/// A candidate value with a non-negative weight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    /// The value.
    pub candidate: Candidate,
    /// The relative weight; non-positive weights are ignored.
    pub weight: f64,
}

impl Suggestion {
    /// A suggestion with weight 1.
    #[must_use]
    pub fn new(candidate: Candidate) -> Self {
        Self {
            candidate,
            weight: 1.0,
        }
    }

    /// A term suggestion with weight 1.
    #[must_use]
    pub fn term(term: impl Into<Term>) -> Self {
        Self::new(Candidate::Term(term.into()))
    }
}

/// A namespace that is worth declaring with `PREFIX`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Namespace {
    /// The preferred prefix (without colon); may be empty.
    pub prefix: String,
    /// The namespace IRI.
    pub iri: String,
}

impl Namespace {
    /// Creates a namespace.
    #[must_use]
    pub fn new(prefix: impl Into<String>, iri: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            iri: iri.into(),
        }
    }

    /// The namespaces of the W3C vocabularies used by SPARQL itself.
    #[must_use]
    pub fn standard() -> Vec<Self> {
        vec![
            Self::new("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            Self::new("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
            Self::new("xsd", "http://www.w3.org/2001/XMLSchema#"),
            Self::new("owl", "http://www.w3.org/2002/07/owl#"),
        ]
    }
}

/// Errors reported by providers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// The SPARQL engine could not be queried.
    #[error("engine request failed: {0}")]
    Engine(#[from] crate::engine::EngineError),
    /// A replay log does not match the requests of the generator.
    #[error("replay diverged: {0}")]
    ReplayDiverged(String),
    /// Any other failure.
    #[error("{0}")]
    Other(String),
}

/// A source of values for the generator.
///
/// Implementations may keep state (e.g. caches) and are therefore called
/// through `&mut self`. The trait is object-safe.
pub trait CompletionProvider {
    /// Proposes values for the position described by `request`.
    ///
    /// Returning no suggestions is fine; the generator then uses other means
    /// to fill the position.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider failed; the generator treats this
    /// like an empty answer but counts it in its statistics.
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError>;

    /// Namespaces worth declaring in the prologue of generated queries.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider failed.
    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        Ok(Vec::new())
    }
}

impl<P: CompletionProvider + ?Sized> CompletionProvider for &mut P {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        (**self).suggest(request)
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        (**self).namespaces()
    }
}

impl<P: CompletionProvider + ?Sized> CompletionProvider for Box<P> {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        (**self).suggest(request)
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        (**self).namespaces()
    }
}
