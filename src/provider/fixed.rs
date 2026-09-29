//! A provider with a fixed vocabulary.

use oxrdf::vocab::xsd;
use oxrdf::{Literal, NamedNode, Term};

use super::{
    CompletionProvider, CompletionRequest, Namespace, ProviderError, Slot, Suggestion, TermKinds,
    ValueType,
};

/// Returns the same fixed list of terms for every request, filtered by the
/// kinds allowed at the requested position and, for expression operands and
/// `VALUES` cells, by the expected value type.
///
/// This is fully deterministic and needs no data, which makes it the
/// provider of choice for unit tests.
#[derive(Debug, Clone, PartialEq)]
pub struct FixedProvider {
    terms: Vec<Term>,
    namespaces: Vec<Namespace>,
}

impl FixedProvider {
    /// Creates a provider suggesting `terms`.
    #[must_use]
    pub fn new(terms: Vec<Term>) -> Self {
        Self {
            terms,
            namespaces: Vec::new(),
        }
    }

    /// Also declares `namespaces` via [`CompletionProvider::namespaces`].
    #[must_use]
    pub fn with_namespaces(mut self, namespaces: Vec<Namespace>) -> Self {
        self.namespaces = namespaces;
        self
    }

    /// A small vocabulary under `http://example.org/` with literals of the
    /// common datatypes.
    #[must_use]
    pub fn example() -> Self {
        const EX: &str = "http://example.org/";
        let iri = |local: &str| Term::NamedNode(NamedNode::new_unchecked(format!("{EX}{local}")));
        let typed =
            |v: &str, dt: oxrdf::NamedNodeRef<'_>| Term::Literal(Literal::new_typed_literal(v, dt));
        let mut terms: Vec<Term> = [
            "alice", "bob", "carol", "knows", "name", "age", "Person", "g1",
        ]
        .into_iter()
        .map(iri)
        .collect();
        terms.push(Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned()));
        terms.extend([
            Term::Literal(Literal::new_simple_literal("Alice")),
            Term::Literal(Literal::new_simple_literal("")),
            Term::Literal(Literal::new_language_tagged_literal_unchecked("chat", "fr")),
            typed("42", xsd::INTEGER),
            typed("-7", xsd::INTEGER),
            typed("1.5", xsd::DECIMAL),
            typed("1.0E0", xsd::DOUBLE),
            typed("true", xsd::BOOLEAN),
            typed("2020-02-29T12:00:00Z", xsd::DATE_TIME),
        ]);
        Self::new(terms).with_namespaces(vec![Namespace::new("ex", EX)])
    }
}

impl CompletionProvider for FixedProvider {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        let typed_slot = matches!(request.slot, Slot::ExpressionOperand | Slot::ValuesCell);
        Ok(self
            .terms
            .iter()
            .filter(|t| {
                let kind = match t {
                    Term::NamedNode(_) => TermKinds::IRI,
                    Term::BlankNode(_) => TermKinds::BLANK_NODE,
                    Term::Literal(_) => TermKinds::LITERAL,
                };
                request.accepts.contains(kind)
                    && (!typed_slot
                        || request.expected == ValueType::Any
                        || request.expected.accepts(ValueType::of_term(t)))
            })
            .cloned()
            .map(Suggestion::term)
            .collect())
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        Ok(self.namespaces.clone())
    }
}
