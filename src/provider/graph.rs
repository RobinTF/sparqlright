//! A deterministic provider backed by an in-memory RDF dataset.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use oxrdf::{GraphName, NamedNode, Quad, Term};
use oxttl::{NQuadsParser, TriGParser};

use super::{
    Candidate, CompletionProvider, CompletionRequest, ContextPredicate, ContextTerm, Namespace,
    ProviderError, Slot, Suggestion, TermKinds, ValueType,
};

/// Answers requests from an in-memory dataset, looking only at the triple
/// being written (plus, for expression operands, the triple patterns that
/// bind the related variable).
///
/// Answers depend only on the request and the data, so generation with this
/// provider is reproducible from the seed alone. Suggestions are weighted by
/// frequency and listed in first-occurrence order.
#[derive(Debug, Clone)]
pub struct GraphProvider {
    quads: Vec<Quad>,
    namespaces: Vec<Namespace>,
    limit: usize,
}

/// An error while loading a dataset.
#[derive(Debug, thiserror::Error)]
#[error("cannot load {path}: {message}")]
pub struct LoadError {
    path: String,
    message: String,
}

impl GraphProvider {
    /// Creates a provider over `quads`.
    #[must_use]
    pub fn new(quads: Vec<Quad>) -> Self {
        let namespaces = guess_namespaces(&quads);
        Self {
            quads,
            namespaces,
            limit: 100,
        }
    }

    /// Loads a Turtle, `TriG`, N-Triples or N-Quads file (by extension).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = path.as_ref();
        let error = |message: String| LoadError {
            path: path.display().to_string(),
            message,
        };
        let file = File::open(path).map_err(|e| error(e.to_string()))?;
        let is_nquads = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("nq"));
        Self::from_reader(BufReader::new(file), is_nquads).map_err(error)
    }

    /// Parses Turtle/TriG/N-Triples, or N-Quads if `nquads` is set.
    ///
    /// # Errors
    ///
    /// Returns the parse error message.
    pub fn from_reader(reader: impl Read, nquads: bool) -> Result<Self, String> {
        if nquads {
            let quads = NQuadsParser::new()
                .for_reader(reader)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            return Ok(Self::new(quads));
        }
        let mut parser = TriGParser::new().for_reader(reader);
        let mut quads = Vec::new();
        for quad in parser.by_ref() {
            quads.push(quad.map_err(|e| e.to_string())?);
        }
        let mut declared: Vec<Namespace> = parser
            .prefixes()
            .map(|(prefix, iri)| Namespace::new(prefix, iri))
            .collect();
        // The parser does not keep declaration order; sort for determinism.
        declared.sort_by(|a, b| a.prefix.cmp(&b.prefix));
        let mut provider = Self::new(quads);
        if !declared.is_empty() {
            provider.namespaces = declared;
        }
        Ok(provider)
    }

    /// Sets the maximum number of suggestions per request (default 100).
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Counts matching values in first-occurrence order.
    fn collect<'a>(
        &self,
        values: impl Iterator<Item = &'a Term>,
        accepts: TermKinds,
        expected: ValueType,
    ) -> Vec<Suggestion> {
        let mut order: Vec<&Term> = Vec::new();
        let mut counts: HashMap<&Term, usize> = HashMap::new();
        for v in values {
            let kind = Candidate::Term(v.clone()).kind();
            if !accepts.contains(kind) || !expected.accepts(ValueType::of_term(v)) {
                continue;
            }
            let c = counts.entry(v).or_insert(0);
            if *c == 0 {
                order.push(v);
            }
            *c += 1;
        }
        #[allow(clippy::cast_precision_loss)]
        order
            .into_iter()
            .take(self.limit)
            .map(|t| Suggestion {
                candidate: Candidate::Term(t.clone()),
                weight: (1.0 + counts[t] as f64).ln(),
            })
            .collect()
    }
}

fn constant(term: Option<&ContextTerm>) -> Option<&Term> {
    match term {
        Some(ContextTerm::Term(t @ (Term::NamedNode(_) | Term::Literal(_)))) => Some(t),
        _ => None,
    }
}

fn predicate_iri(p: Option<&ContextPredicate>) -> Option<&NamedNode> {
    match p {
        Some(ContextPredicate::Iri(i)) => Some(i),
        _ => None,
    }
}

fn subject_term(q: &Quad) -> Term {
    q.subject.clone().into()
}

impl CompletionProvider for GraphProvider {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        let accepts = request.accepts;
        let partial = request.partial_triple.cloned().unwrap_or_default();
        let s = constant(partial.subject.as_ref()).cloned();
        let p = predicate_iri(partial.predicate.as_ref()).cloned();
        let matches_s = |q: &&Quad| s.as_ref().is_none_or(|s| subject_term(q) == *s);
        let matches_p = |q: &&Quad| p.as_ref().is_none_or(|p| q.predicate == *p);
        let any = ValueType::Any;
        let result = match request.slot {
            Slot::Subject | Slot::DescribeTarget => {
                let subjects: Vec<Term> = self.quads.iter().map(subject_term).collect();
                self.collect(subjects.iter(), accepts, any)
            }
            Slot::Predicate | Slot::PathIri => {
                let predicates: Vec<Term> = self
                    .quads
                    .iter()
                    .filter(matches_s)
                    .map(|q| q.predicate.clone().into())
                    .collect();
                self.collect(predicates.iter(), accepts, any)
            }
            Slot::Object => self.collect(
                self.quads
                    .iter()
                    .filter(matches_s)
                    .filter(matches_p)
                    .map(|q| &q.object),
                accepts,
                any,
            ),
            Slot::GraphName => {
                let graphs: Vec<Term> = self
                    .quads
                    .iter()
                    .filter_map(|q| match &q.graph_name {
                        GraphName::NamedNode(n) => Some(n.clone().into()),
                        _ => None,
                    })
                    .collect();
                self.collect(graphs.iter(), accepts, any)
            }
            Slot::ServiceEndpoint => Vec::new(),
            Slot::ExpressionOperand | Slot::ValuesCell => {
                // Values of the related variable, found through the triple
                // patterns that bind it to a constant predicate.
                let bound_by: Vec<(&NamedNode, bool)> = request
                    .related_variable
                    .map(|v| {
                        request
                            .context
                            .iter()
                            .filter_map(|t| {
                                let p = predicate_iri(Some(&t.predicate))?;
                                match (&t.subject, &t.object) {
                                    (_, ContextTerm::Variable(o)) if o == v => Some((p, false)),
                                    (ContextTerm::Variable(s), _) if s == v => Some((p, true)),
                                    _ => None,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let values: Vec<Term> = if bound_by.is_empty() {
                    self.quads.iter().map(|q| q.object.clone()).collect()
                } else {
                    self.quads
                        .iter()
                        .filter_map(|q| {
                            bound_by.iter().find(|(p, _)| q.predicate == **p).map(
                                |(_, is_subject)| {
                                    if *is_subject {
                                        subject_term(q)
                                    } else {
                                        q.object.clone()
                                    }
                                },
                            )
                        })
                        .collect()
                };
                self.collect(values.iter(), accepts, request.expected)
            }
        };
        Ok(result)
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        Ok(self.namespaces.clone())
    }
}

fn guess_namespaces(quads: &[Quad]) -> Vec<Namespace> {
    let iris = quads.iter().flat_map(|q| {
        let s = match &q.subject {
            oxrdf::NamedOrBlankNode::NamedNode(s) => Some(s.as_str()),
            oxrdf::NamedOrBlankNode::BlankNode(_) => None,
        };
        let o = match &q.object {
            Term::NamedNode(o) => Some(o.as_str()),
            _ => None,
        };
        s.into_iter().chain(Some(q.predicate.as_str())).chain(o)
    });
    namespaces_of(iris)
}

/// Guesses the most frequent namespaces of `iris` by cutting each IRI after
/// its last `#` or `/`.
pub(crate) fn namespaces_of<'a>(iris: impl Iterator<Item = &'a str>) -> Vec<Namespace> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut order = Vec::new();
    for iri in iris {
        if let Some(i) = iri.rfind(['#', '/']) {
            let ns = &iri[..=i];
            if ns.len() > "http://".len() {
                let c = counts.entry(ns.to_owned()).or_insert(0);
                if *c == 0 {
                    order.push(ns.to_owned());
                }
                *c += 1;
            }
        }
    }
    order.sort_by_key(|ns| std::cmp::Reverse(counts[ns]));
    order
        .into_iter()
        .take(8)
        .enumerate()
        .map(|(i, iri)| Namespace::new(format!("ns{i}"), iri))
        .collect()
}
