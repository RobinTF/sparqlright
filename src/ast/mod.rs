//! A syntax tree for the complete SPARQL 1.1 grammar.
//!
//! The tree follows the productions of the
//! [SPARQL 1.1 grammar](https://www.w3.org/TR/sparql11-query/#sparqlGrammar)
//! closely enough that every alternative of the grammar, including purely
//! syntactic ones (prefixed names vs. IRI references, `;`/`,` abbreviations,
//! optional dots, quote styles, signed numbers, ...), can be represented and
//! is serialized faithfully. Expressions are the one exception: they are an
//! operator tree whose serialization adds the parentheses the grammar needs.
//!
//! Every node implements [`WriteSparql`] and [`std::fmt::Display`].
//!
//! ```
//! use sparqlright::ast::*;
//!
//! let x = Variable::new("x")?;
//! let pattern = GroupGraphPattern::Group(vec![GroupElement::Pattern {
//!     pattern: GraphPatternNotTriples::Bind(
//!         Expression::Literal(Literal::Numeric(NumericLiteral::new("42")?)),
//!         x.clone(),
//!     ),
//!     dot: false,
//! }]);
//! let query = Query {
//!     prologue: Prologue::default(),
//!     form: QueryForm::Select(SelectQuery {
//!         select: SelectClause {
//!             modifier: None,
//!             projection: Projection::Items(vec![ProjectionItem::Var(x)]),
//!         },
//!         dataset: vec![],
//!         where_clause: WhereClause { keyword: true, pattern },
//!         modifiers: SolutionModifier::default(),
//!     }),
//!     values: None,
//! };
//! assert_eq!(query.to_string(), "SELECT ?x WHERE { BIND(42 AS ?x) }");
//! # Ok::<_, sparqlright::ast::InvalidTerminal>(())
//! ```

mod expr;
mod pattern;
mod query;
mod term;
mod update;
mod writer;

pub use expr::{
    Aggregate, Arity, BuiltInCall, BuiltInFunction, CompareOp, Expression, FunctionCall,
    MultiplicativeOp, SetFunction,
};
pub use pattern::{
    Constraint, DataBlock, DataBlockValue, GraphNode, GraphNodePath, GraphPatternNotTriples,
    GroupElement, GroupGraphPattern, NegatedSetStyle, Path, PathElt, PathEltOrInverse, PathMod,
    PathOneInPropertySet, PathPrimary, PathSequence, PropertyList, PropertyListPath, TriplesBlock,
    TriplesNode, TriplesNodePath, TriplesSameSubject, TriplesSameSubjectPath, TriplesTemplate,
    Verb, VerbPath,
};
pub use query::{
    AskQuery, ConstructQuery, DatasetClause, DescribeQuery, GroupCondition, LimitOffset,
    OrderCondition, Projection, ProjectionItem, Prologue, PrologueDecl, Query, QueryForm,
    SelectClause, SelectModifier, SelectQuery, SolutionModifier, SubSelect, WhereClause,
};
pub use term::{
    BlankNode, BlankNodeLabel, GraphTerm, InvalidTerminal, Iri, IriRef, LanguageTag, Literal,
    LiteralAnnotation, NumericKind, NumericLiteral, PrefixedName, QuoteStyle, RdfLiteral,
    StringLiteral, VarOrIri, VarOrTerm, VarSigil, Variable,
};
pub use update::{
    GraphOrDefault, GraphRefAll, Quads, QuadsElement, TransferKind, Update, UpdateOperation,
    UsingClause,
};
pub use writer::{Mark, TokenWriter, WriteSparql};
