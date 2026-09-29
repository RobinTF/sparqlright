//! Suggestions from the SPARQL engine under test.
//!
//! The approach follows context-sensitive SPARQL autocompletion as in the
//! `QLever` UI (Bast et al., "Efficient and Effective SPARQL Autocompletion on
//! Very Large Knowledge Graphs", CIKM 2022): to suggest the object of
//! `?s ex:p _`, the provider asks the engine for the most frequent values of
//! a hole variable in `{ <context> ?s ex:p ?_hole }`, where the context is the
//! part of the partial query connected to the triple being written. Values
//! chosen this way keep the generated pattern satisfiable, which matters
//! because tests on empty results are nearly worthless.
//!
//! If the context query fails, times out or returns nothing, the provider
//! falls back to the triple alone and then to a generic query for the slot.
//! All answers are cached per query string.

use std::collections::{BTreeSet, HashMap};

use oxrdf::Term;

use super::graph::namespaces_of;
use super::{
    Candidate, CompletionProvider, CompletionRequest, ContextPredicate, ContextTerm, ContextTriple,
    Namespace, ProviderError, Slot, Suggestion, TermKinds, ValueType,
};
use crate::ast::Variable;
use crate::engine::SparqlService;

/// Settings of an [`EngineProvider`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineProviderConfig {
    /// Maximum number of suggestions per query (`LIMIT`).
    pub limit: usize,
    /// Whether to include the connected part of the partial query.
    pub use_context: bool,
    /// Maximum number of context triple patterns.
    pub max_context_triples: usize,
    /// Whether context triple patterns with property paths are included.
    pub include_paths: bool,
    /// Whether to rank by frequency (`GROUP BY ... ORDER BY DESC(COUNT(*))`)
    /// instead of taking `DISTINCT` values.
    pub weigh_by_count: bool,
    /// Number of IRIs sampled to guess namespaces for `PREFIX` declarations.
    pub namespace_sample: usize,
    /// After this many consecutive failed (e.g. timed out) engine queries,
    /// context queries are skipped for a while; see `backoff_requests`.
    pub max_consecutive_failures: u32,
    /// The number of requests for which context queries are skipped after
    /// `max_consecutive_failures` failures. Context-free queries are cheap
    /// and cached, so generation stays fast while the engine struggles.
    pub backoff_requests: u32,
}

impl Default for EngineProviderConfig {
    fn default() -> Self {
        Self {
            limit: 100,
            use_context: true,
            max_context_triples: 3,
            include_paths: false,
            weigh_by_count: true,
            namespace_sample: 1000,
            max_consecutive_failures: 2,
            backoff_requests: 100,
        }
    }
}

/// Counters of an [`EngineProvider`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineProviderStats {
    /// Queries sent to the engine.
    pub queries: u64,
    /// Answers served from the cache.
    pub cache_hits: u64,
    /// Queries that failed.
    pub failures: u64,
    /// How often context queries were suspended after repeated failures.
    pub backoffs: u64,
}

/// Asks the SPARQL engine under test for values.
///
/// The approach follows context-sensitive SPARQL autocompletion as in the
/// `QLever` UI (Bast et al., "Efficient and Effective SPARQL Autocompletion on
/// Very Large Knowledge Graphs", CIKM 2022): to suggest the object of
/// `?s ex:p _`, the provider asks the engine for the most frequent values of
/// a hole variable in `{ <context> ?s ex:p ?_hole }`, where the context is the
/// part of the partial query connected to the triple being written. Values
/// chosen this way keep generated patterns satisfiable.
///
/// If the context query fails, times out or returns nothing, the provider
/// falls back to the triple alone and then to a generic query for the slot.
/// All answers are cached per query string.
#[derive(Debug)]
pub struct EngineProvider<S> {
    service: S,
    config: EngineProviderConfig,
    cache: HashMap<String, Result<Vec<Suggestion>, String>>,
    namespaces: Option<Vec<Namespace>>,
    stats: EngineProviderStats,
    /// Consecutive failed context queries.
    consecutive_failures: u32,
    /// Remaining requests for which context queries are skipped.
    backoff_remaining: u32,
}

const HOLE: &str = "_hole";
const COUNT: &str = "_count";

impl<S: SparqlService> EngineProvider<S> {
    /// Creates a provider querying `service`.
    pub fn new(service: S, config: EngineProviderConfig) -> Self {
        Self {
            service,
            config,
            cache: HashMap::new(),
            namespaces: None,
            stats: EngineProviderStats::default(),
            consecutive_failures: 0,
            backoff_remaining: 0,
        }
    }

    /// The counters so far.
    #[must_use]
    pub fn stats(&self) -> EngineProviderStats {
        self.stats
    }

    /// Runs `query` (cached) and returns the values of `?column`.
    fn run(&mut self, query: String, column: &str) -> Result<Vec<Suggestion>, String> {
        if let Some(cached) = self.cache.get(&query) {
            self.stats.cache_hits += 1;
            return cached.clone();
        }
        self.stats.queries += 1;
        let result = self
            .service
            .select(&query)
            .map(|solutions| {
                let counts: Vec<Option<f64>> = solutions
                    .rows
                    .iter()
                    .map(|row| {
                        let i = solutions
                            .variables
                            .iter()
                            .position(|v| v.as_str() == COUNT)?;
                        match row.get(i)?.as_ref()? {
                            Term::Literal(l) => l.value().parse::<f64>().ok(),
                            _ => None,
                        }
                    })
                    .collect();
                let index = solutions
                    .variables
                    .iter()
                    .position(|v| v.as_str() == column);
                solutions
                    .rows
                    .iter()
                    .zip(counts)
                    .filter_map(|(row, count)| {
                        let term = row.get(index?)?.clone()?;
                        Some(Suggestion {
                            candidate: Candidate::Term(term),
                            weight: count.map_or(1.0, |c| (1.0 + c).ln().max(f64::MIN_POSITIVE)),
                        })
                    })
                    .collect()
            })
            .map_err(|e| e.to_string());
        if result.is_err() {
            self.stats.failures += 1;
        }
        self.cache.insert(query, result.clone());
        result
    }

    /// Suspends context queries after too many consecutive failures.
    fn context_query_failed(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.config.max_consecutive_failures.max(1) {
            self.consecutive_failures = 0;
            self.backoff_remaining = self.config.backoff_requests;
            self.stats.backoffs += 1;
        }
    }

    /// Builds the final query around a `WHERE` body that binds `?column`.
    fn select_values(
        &self,
        prologue: &str,
        body: &str,
        column: &str,
        filter: Option<&str>,
    ) -> String {
        let filter = filter.map(|f| format!(" FILTER({f})")).unwrap_or_default();
        let limit = self.config.limit;
        if self.config.weigh_by_count {
            format!(
                "{prologue}SELECT ?{column} (COUNT(*) AS ?{COUNT}) WHERE {{ {body}{filter} }} \
                 GROUP BY ?{column} ORDER BY DESC(?{COUNT}) LIMIT {limit}"
            )
        } else {
            format!("{prologue}SELECT DISTINCT ?{column} WHERE {{ {body}{filter} }} LIMIT {limit}")
        }
    }

    /// Selects the context triples connected to `seeds`, closest first.
    fn connected_context<'a>(
        &self,
        context: &'a [ContextTriple],
        seeds: BTreeSet<String>,
    ) -> Vec<&'a ContextTriple> {
        let mut vars = seeds;
        let mut chosen: Vec<&ContextTriple> = Vec::new();
        let mut changed = true;
        while changed && chosen.len() < self.config.max_context_triples {
            changed = false;
            for t in context.iter().rev() {
                if chosen.len() >= self.config.max_context_triples {
                    break;
                }
                if chosen.iter().any(|c| std::ptr::eq(*c, t)) {
                    continue;
                }
                if !self.config.include_paths && matches!(t.predicate, ContextPredicate::Path(_)) {
                    continue;
                }
                let tv = triple_variables(t);
                if tv.iter().any(|v| vars.contains(v)) {
                    vars.extend(tv);
                    chosen.push(t);
                    changed = true;
                }
            }
        }
        chosen
    }

    fn context_body(
        &self,
        request: &CompletionRequest<'_>,
        seeds: BTreeSet<String>,
    ) -> Option<String> {
        if !self.config.use_context || seeds.is_empty() {
            return None;
        }
        let chosen = self.connected_context(request.context, seeds);
        if chosen.is_empty() {
            return None;
        }
        let mut body = String::new();
        for t in chosen {
            body.push_str(&render_triple(t));
            body.push_str(" . ");
        }
        Some(body)
    }
}

/// Renders a context term; blank nodes of the query become variables.
fn render_term(term: &ContextTerm) -> String {
    match term {
        ContextTerm::Variable(v) => format!("?{}", v.name()),
        ContextTerm::Term(Term::BlankNode(b)) => format!("?{}", bnode_variable(b.as_str())),
        ContextTerm::Term(t) => t.to_string(),
    }
}

fn bnode_variable(label: &str) -> String {
    let sanitized: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("_b_{sanitized}")
}

fn render_predicate(p: &ContextPredicate) -> String {
    match p {
        ContextPredicate::Variable(v) => format!("?{}", v.name()),
        ContextPredicate::Iri(i) => i.to_string(),
        ContextPredicate::Path(text) => text.clone(),
    }
}

fn render_triple(t: &ContextTriple) -> String {
    format!(
        "{} {} {}",
        render_term(&t.subject),
        render_predicate(&t.predicate),
        render_term(&t.object)
    )
}

fn term_variable(term: &ContextTerm) -> Option<String> {
    match term {
        ContextTerm::Variable(v) => Some(v.name().to_owned()),
        ContextTerm::Term(Term::BlankNode(b)) => Some(bnode_variable(b.as_str())),
        ContextTerm::Term(_) => None,
    }
}

fn triple_variables(t: &ContextTriple) -> Vec<String> {
    let mut vars: Vec<String> = [term_variable(&t.subject), term_variable(&t.object)]
        .into_iter()
        .flatten()
        .collect();
    if let ContextPredicate::Variable(v) = &t.predicate {
        vars.push(v.name().to_owned());
    }
    vars
}

/// A server-side filter restricting `?var` to the expected kind and type.
fn type_filter(var: &str, accepts: TermKinds, expected: ValueType) -> Option<String> {
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    let by_type = match expected {
        ValueType::Any => None,
        ValueType::Boolean => Some(format!("DATATYPE(?{var}) = <{xsd}boolean>")),
        ValueType::Numeric => Some(format!("isNUMERIC(?{var})")),
        ValueType::String => Some(format!(
            "isLITERAL(?{var}) && (DATATYPE(?{var}) = <{xsd}string> || LANG(?{var}) != \"\")"
        )),
        ValueType::SimpleString => Some(format!(
            "isLITERAL(?{var}) && DATATYPE(?{var}) = <{xsd}string>"
        )),
        ValueType::DateTime => Some(format!("DATATYPE(?{var}) = <{xsd}dateTime>")),
        ValueType::Iri => Some(format!("isIRI(?{var})")),
        ValueType::Literal => Some(format!("isLITERAL(?{var})")),
    };
    let by_kind = if accepts.contains(TermKinds::LITERAL) {
        None
    } else if accepts.contains(TermKinds::IRI) {
        Some(format!("isIRI(?{var})"))
    } else {
        None
    };
    match (by_type, by_kind) {
        (Some(a), Some(b)) => Some(format!("{a} && {b}")),
        (a, b) => a.or(b),
    }
}

/// The triple pattern that binds the requested value.
struct Hole {
    /// The pattern with the hole variable; `None` if the value is a
    /// variable of the context itself (expression operands).
    pattern: Option<ContextTriple>,
    /// Variables connecting the pattern to the context.
    seeds: BTreeSet<String>,
    /// The variable whose values are suggested.
    column: String,
}

fn var_term(name: &str) -> ContextTerm {
    ContextTerm::Variable(Variable::new_unchecked(name))
}

fn var_predicate(name: &str) -> ContextPredicate {
    ContextPredicate::Variable(Variable::new_unchecked(name))
}

/// The hole for a triple or expression slot; `None` for other slots.
fn hole(request: &CompletionRequest<'_>) -> Option<Hole> {
    let partial = request.partial_triple.cloned().unwrap_or_default();
    let with_hole = |pattern: ContextTriple, seeds: BTreeSet<String>| Hole {
        pattern: Some(pattern),
        seeds,
        column: HOLE.to_owned(),
    };
    let hole = match request.slot {
        Slot::Subject | Slot::DescribeTarget => with_hole(
            ContextTriple {
                subject: var_term(HOLE),
                predicate: var_predicate("_p"),
                object: var_term("_o"),
            },
            BTreeSet::new(),
        ),
        Slot::Predicate | Slot::PathIri => {
            let subject = partial.subject.unwrap_or_else(|| var_term("_s"));
            let seeds = term_variable(&subject).into_iter().collect();
            with_hole(
                ContextTriple {
                    subject,
                    predicate: var_predicate(HOLE),
                    object: var_term("_o"),
                },
                seeds,
            )
        }
        Slot::Object => {
            let subject = partial.subject.unwrap_or_else(|| var_term("_s"));
            let predicate = partial.predicate.unwrap_or_else(|| var_predicate("_p"));
            let mut seeds: BTreeSet<String> = term_variable(&subject).into_iter().collect();
            if let ContextPredicate::Variable(v) = &predicate {
                seeds.insert(v.name().to_owned());
            }
            with_hole(
                ContextTriple {
                    subject,
                    predicate,
                    object: var_term(HOLE),
                },
                seeds,
            )
        }
        Slot::ExpressionOperand | Slot::ValuesCell => match request.related_variable {
            // Values of a variable of the context: select the variable itself.
            Some(v)
                if request
                    .context
                    .iter()
                    .any(|t| triple_variables(t).iter().any(|n| n == v.name())) =>
            {
                Hole {
                    pattern: None,
                    seeds: BTreeSet::from([v.name().to_owned()]),
                    column: v.name().to_owned(),
                }
            }
            _ => with_hole(
                ContextTriple {
                    subject: var_term("_s"),
                    predicate: var_predicate("_p"),
                    object: var_term(HOLE),
                },
                BTreeSet::new(),
            ),
        },
        Slot::GraphName | Slot::ServiceEndpoint => return None,
    };
    Some(hole)
}

/// The pattern with all variables except the hole renamed apart, or `None`
/// if nothing but variables would remain.
fn without_context(p: &ContextTriple) -> Option<ContextTriple> {
    let has_constant = !matches!(p.subject, ContextTerm::Variable(_))
        || matches!(
            p.predicate,
            ContextPredicate::Iri(_) | ContextPredicate::Path(_)
        );
    if !has_constant {
        return None;
    }
    let rename = |t: &ContextTerm, fresh: &str| match t {
        ContextTerm::Variable(v) if v.name() != HOLE => var_term(fresh),
        other => other.clone(),
    };
    Some(ContextTriple {
        subject: rename(&p.subject, "_s"),
        predicate: p.predicate.clone(),
        object: rename(&p.object, "_o"),
    })
}

impl<S: SparqlService> CompletionProvider for EngineProvider<S> {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        let prologue = if request.prologue.is_empty() {
            String::new()
        } else {
            format!("{} ", request.prologue)
        };
        let Some(Hole {
            pattern,
            seeds,
            column,
        }) = hole(request)
        else {
            if request.slot != Slot::GraphName {
                return Ok(Vec::new());
            }
            let query = format!(
                "{prologue}SELECT DISTINCT ?{HOLE} WHERE {{ GRAPH ?{HOLE} {{ ?_s ?_p ?_o }} }} LIMIT {}",
                self.config.limit
            );
            return Ok(self.run(query, HOLE).unwrap_or_default());
        };
        let filter = type_filter(HOLE, request.accepts, request.expected);
        let pattern_text = pattern
            .as_ref()
            .map(|p| format!("{} .", render_triple(p)))
            .unwrap_or_default();

        // 1. With the connected context, unless suspended after failures.
        let context = if self.backoff_remaining > 0 {
            self.backoff_remaining -= 1;
            None
        } else {
            self.context_body(request, seeds)
        };
        if let Some(context) = context {
            let column_filter = type_filter(&column, request.accepts, request.expected);
            let body = format!("{context}{pattern_text}");
            let query = self.select_values(&prologue, &body, &column, column_filter.as_deref());
            match self.run(query, &column) {
                Ok(found) => {
                    self.consecutive_failures = 0;
                    if !found.is_empty() {
                        return Ok(found);
                    }
                }
                Err(_) => self.context_query_failed(),
            }
        }
        // 2. The triple alone, if it constrains anything.
        if let Some(alone) = pattern.as_ref().and_then(without_context) {
            let body = format!("{} .", render_triple(&alone));
            let query = self.select_values(&prologue, &body, HOLE, filter.as_deref());
            if let Ok(found) = self.run(query, HOLE)
                && !found.is_empty()
            {
                return Ok(found);
            }
        }
        // 3. Generic values for the slot.
        let body = match request.slot {
            Slot::Subject | Slot::DescribeTarget => format!("?{HOLE} ?_p ?_o ."),
            Slot::Predicate | Slot::PathIri => format!("?_s ?{HOLE} ?_o ."),
            _ => format!("?_s ?_p ?{HOLE} ."),
        };
        let query = self.select_values(&prologue, &body, HOLE, filter.as_deref());
        self.run(query, HOLE)
            .map_err(|e| ProviderError::Other(format!("engine provider: {e}")))
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        if let Some(cached) = &self.namespaces {
            return Ok(cached.clone());
        }
        let query = format!(
            "SELECT ?s ?p WHERE {{ ?s ?p ?o }} LIMIT {}",
            self.config.namespace_sample
        );
        let solutions = self.service.select(&query)?;
        let iris: Vec<String> = solutions
            .rows
            .iter()
            .flatten()
            .filter_map(|t| match t {
                Some(Term::NamedNode(n)) => Some(n.as_str().to_owned()),
                _ => None,
            })
            .collect();
        let namespaces = namespaces_of(iris.iter().map(String::as_str));
        self.namespaces = Some(namespaces.clone());
        Ok(namespaces)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::engine::{EngineError, Response};

    /// Records queries and answers with an empty JSON result.
    #[derive(Default)]
    struct Recorder(RefCell<Vec<String>>);

    impl SparqlService for Recorder {
        fn query(&self, query: &str, _accept: &str) -> Result<Response, EngineError> {
            self.0.borrow_mut().push(query.to_owned());
            Ok(Response {
                status: 200,
                content_type: Some("application/sparql-results+json".to_owned()),
                body: r#"{"head":{"vars":["_hole"]},"results":{"bindings":[]}}"#.to_owned(),
                elapsed: std::time::Duration::ZERO,
            })
        }

        fn update(&self, _update: &str) -> Result<Response, EngineError> {
            unreachable!()
        }
    }

    #[test]
    fn object_queries_use_connected_context_then_fall_back() {
        let v = |n: &str| Variable::new(n).unwrap();
        let p = |iri: &str| ContextPredicate::Iri(oxrdf::NamedNode::new(iri).unwrap());
        let context = vec![
            ContextTriple {
                subject: ContextTerm::Variable(v("x")),
                predicate: p("http://ex/q"),
                object: ContextTerm::Variable(v("y")),
            },
            ContextTriple {
                subject: ContextTerm::Variable(v("s")),
                predicate: p("http://ex/type"),
                object: ContextTerm::Variable(v("c")),
            },
        ];
        let partial = super::super::PartialTriple {
            subject: Some(ContextTerm::Variable(v("s"))),
            predicate: Some(p("http://ex/p")),
        };
        let fresh = v("v9");
        let request = CompletionRequest {
            partial_query: "SELECT * WHERE { ?x <http://ex/q> ?y . ?s <http://ex/type> ?c . ?s <http://ex/p>",
            prologue: "",
            slot: Slot::Object,
            accepts: TermKinds::all(),
            expected: ValueType::Any,
            related_variable: None,
            partial_triple: Some(&partial),
            context: &context,
            variables: &[],
            outer_variables: &[],
            fresh_variable: &fresh,
        };
        let recorder = Recorder::default();
        let mut provider = EngineProvider::new(&recorder, EngineProviderConfig::default());
        provider.suggest(&request).unwrap();
        let queries = recorder.0.borrow();
        assert_eq!(queries.len(), 3, "{queries:#?}");
        assert!(queries[0].contains("?s <http://ex/type> ?c . ?s <http://ex/p> ?_hole ."));
        assert!(
            !queries[0].contains("<http://ex/q>"),
            "unconnected triple included"
        );
        assert!(queries[1].contains("{ ?_s <http://ex/p> ?_hole . }"));
        assert!(queries[2].contains("{ ?_s ?_p ?_hole . }"));
    }

    /// Times out on context queries and answers others with no results.
    #[derive(Default)]
    struct SlowContext(RefCell<Vec<String>>);

    impl SparqlService for SlowContext {
        fn query(&self, query: &str, accept: &str) -> Result<Response, EngineError> {
            if query.contains("<http://ex/type>") {
                self.0.borrow_mut().push(query.to_owned());
                return Err(EngineError::Timeout(std::time::Duration::from_secs(1)));
            }
            Recorder::default().query(query, accept)
        }

        fn update(&self, _update: &str) -> Result<Response, EngineError> {
            unreachable!()
        }
    }

    #[test]
    fn context_queries_are_suspended_after_repeated_failures() {
        let v = |n: &str| Variable::new(n).unwrap();
        let context = vec![ContextTriple {
            subject: ContextTerm::Variable(v("s")),
            predicate: ContextPredicate::Iri(oxrdf::NamedNode::new("http://ex/type").unwrap()),
            object: ContextTerm::Variable(v("c")),
        }];
        let fresh = v("v9");
        let service = SlowContext::default();
        let config = EngineProviderConfig {
            max_consecutive_failures: 2,
            backoff_requests: 3,
            ..EngineProviderConfig::default()
        };
        let mut provider = EngineProvider::new(&service, config);
        for i in 0..6 {
            // A different predicate per request, so that nothing is cached.
            let partial = super::super::PartialTriple {
                subject: Some(ContextTerm::Variable(v("s"))),
                predicate: Some(ContextPredicate::Iri(
                    oxrdf::NamedNode::new(format!("http://ex/p{i}")).unwrap(),
                )),
            };
            let request = CompletionRequest {
                partial_query: "",
                prologue: "",
                slot: Slot::Object,
                accepts: TermKinds::all(),
                expected: ValueType::Any,
                related_variable: None,
                partial_triple: Some(&partial),
                context: &context,
                variables: &[],
                outer_variables: &[],
                fresh_variable: &fresh,
            };
            provider.suggest(&request).unwrap();
        }
        // Requests 0 and 1 fail, 2..=4 skip the context, 5 tries again.
        assert_eq!(service.0.borrow().len(), 3);
        assert_eq!(provider.stats().backoffs, 1);
    }
}
