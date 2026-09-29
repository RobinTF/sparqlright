//! Access to SPARQL engines over the SPARQL 1.1 Protocol.
//!
//! [`SparqlService`] abstracts over "something that answers SPARQL requests"
//! so that providers and the fuzzing loop can be tested without a server;
//! [`HttpEndpoint`] is the implementation for real engines.

use std::time::{Duration, Instant};

use oxrdf::{Term, Variable as RdfVariable};
use sparesults::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};

/// The `Accept` header for SELECT and ASK queries.
pub const ACCEPT_RESULTS: &str = "application/sparql-results+json";
/// The `Accept` header for CONSTRUCT and DESCRIBE queries.
pub const ACCEPT_GRAPH: &str = "application/n-triples, text/turtle;q=0.9, */*;q=0.1";

/// Errors when talking to an engine.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The engine could not be reached or closed the connection.
    #[error("connection failed: {0}")]
    Connection(String),
    /// The request did not finish in time.
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    /// The engine answered with a non-success status.
    #[error("HTTP {status}: {}", truncate(body, 300))]
    Status {
        /// The HTTP status code.
        status: u16,
        /// The response body.
        body: String,
    },
    /// The response could not be parsed as SPARQL results.
    #[error("invalid query results: {0}")]
    InvalidResults(String),
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// A raw response of an engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// The `Content-Type` header, if any.
    pub content_type: Option<String>,
    /// The response body.
    pub body: String,
    /// The time until the body was received.
    pub elapsed: Duration,
}

impl Response {
    /// Whether the status is 2xx.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parses the body as SPARQL JSON or XML results.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Status`] for a non-success response and
    /// [`EngineError::InvalidResults`] if the body cannot be parsed.
    pub fn results(&self) -> Result<QueryResults, EngineError> {
        if !self.is_success() {
            return Err(EngineError::Status {
                status: self.status,
                body: self.body.clone(),
            });
        }
        let format = match self.content_type.as_deref() {
            Some(ct) if ct.contains("xml") => QueryResultsFormat::Xml,
            _ => QueryResultsFormat::Json,
        };
        parse_results(self.body.as_bytes(), format)
    }
}

/// Parsed results of a SELECT or ASK query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryResults {
    /// The answer of an ASK query.
    Boolean(bool),
    /// The solutions of a SELECT query.
    Solutions(Solutions),
}

/// A solution sequence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Solutions {
    /// The result variables.
    pub variables: Vec<RdfVariable>,
    /// One entry per solution, aligned with `variables`; `None` is unbound.
    pub rows: Vec<Vec<Option<Term>>>,
}

impl Solutions {
    /// The values of `variable` in all solutions where it is bound.
    pub fn column<'a>(&'a self, variable: &str) -> impl Iterator<Item = &'a Term> + 'a {
        let index = self.variables.iter().position(|v| v.as_str() == variable);
        self.rows
            .iter()
            .filter_map(move |row| index.and_then(|i| row.get(i)?.as_ref()))
    }
}

fn parse_results(body: &[u8], format: QueryResultsFormat) -> Result<QueryResults, EngineError> {
    if format == QueryResultsFormat::Json {
        return parse_json_results(body);
    }
    let invalid = |e: &dyn std::fmt::Display| EngineError::InvalidResults(e.to_string());
    match QueryResultsParser::from_format(format)
        .for_slice(body)
        .map_err(|e| invalid(&e))?
    {
        SliceQueryResultsParserOutput::Boolean(b) => Ok(QueryResults::Boolean(b)),
        SliceQueryResultsParserOutput::Solutions(parser) => {
            let variables = parser.variables().to_vec();
            let mut rows = Vec::new();
            for solution in parser {
                let solution = solution.map_err(|e| invalid(&e))?;
                rows.push(variables.iter().map(|v| solution.get(v).cloned()).collect());
            }
            Ok(QueryResults::Solutions(Solutions { variables, rows }))
        }
    }
}

/// Parses the SPARQL 1.1 Query Results JSON Format.
///
/// This is more lenient than `sparesults` 0.3, which rejects additional
/// top-level members after `results` (`QLever`, for example, appends a
/// `meta` member), and it accepts the legacy `typed-literal` type.
fn parse_json_results(body: &[u8]) -> Result<QueryResults, EngineError> {
    use serde_json::Value;
    let invalid = |m: String| EngineError::InvalidResults(m);
    let json: Value = serde_json::from_slice(body).map_err(|e| invalid(e.to_string()))?;
    if let Some(b) = json.get("boolean") {
        return b
            .as_bool()
            .map(QueryResults::Boolean)
            .ok_or_else(|| invalid("`boolean` is not a boolean".to_owned()));
    }
    let variables: Vec<RdfVariable> = json
        .pointer("/head/vars")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("missing `head.vars`".to_owned()))?
        .iter()
        .map(|v| {
            v.as_str()
                .and_then(|name| RdfVariable::new(name).ok())
                .ok_or_else(|| invalid(format!("invalid variable {v}")))
        })
        .collect::<Result<_, _>>()?;
    let bindings = json
        .pointer("/results/bindings")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("missing `results.bindings`".to_owned()))?;
    let mut rows = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let object = binding
            .as_object()
            .ok_or_else(|| invalid("a binding is not an object".to_owned()))?;
        let mut row = vec![None; variables.len()];
        for (name, value) in object {
            let i = variables
                .iter()
                .position(|v| v.as_str() == name)
                .ok_or_else(|| invalid(format!("undeclared variable {name}")))?;
            row[i] = Some(parse_json_term(value).map_err(invalid)?);
        }
        rows.push(row);
    }
    Ok(QueryResults::Solutions(Solutions { variables, rows }))
}

fn parse_json_term(value: &serde_json::Value) -> Result<Term, String> {
    use oxrdf::{BlankNode, Literal, NamedNode};
    let field = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
    let lexical = field("value").ok_or_else(|| format!("term without value: {value}"))?;
    match field("type") {
        Some("uri") => NamedNode::new(lexical)
            .map(Term::from)
            .map_err(|e| e.to_string()),
        Some("bnode") => BlankNode::new(lexical)
            .map(Term::from)
            .map_err(|e| e.to_string()),
        Some("literal" | "typed-literal") => {
            if let Some(lang) = field("xml:lang") {
                Literal::new_language_tagged_literal(lexical, lang)
                    .map(Term::from)
                    .map_err(|e| e.to_string())
            } else if let Some(datatype) = field("datatype") {
                let datatype = NamedNode::new(datatype).map_err(|e| e.to_string())?;
                Ok(Literal::new_typed_literal(lexical, datatype).into())
            } else {
                Ok(Literal::new_simple_literal(lexical).into())
            }
        }
        other => Err(format!("unsupported term type {other:?}")),
    }
}

/// Something that answers SPARQL requests.
pub trait SparqlService {
    /// Sends a query and returns the raw response, whatever its status.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Connection`] or [`EngineError::Timeout`] if no
    /// response was received.
    fn query(&self, query: &str, accept: &str) -> Result<Response, EngineError>;

    /// Sends an update and returns the raw response, whatever its status.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Connection`] or [`EngineError::Timeout`] if no
    /// response was received.
    fn update(&self, update: &str) -> Result<Response, EngineError>;

    /// Runs a SELECT query and parses its solutions.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, the engine reports an error or
    /// the results are not solutions.
    fn select(&self, query: &str) -> Result<Solutions, EngineError> {
        match self.query(query, ACCEPT_RESULTS)?.results()? {
            QueryResults::Solutions(s) => Ok(s),
            QueryResults::Boolean(_) => Err(EngineError::InvalidResults(
                "expected solutions, got a boolean".to_owned(),
            )),
        }
    }
}

impl<S: SparqlService + ?Sized> SparqlService for &S {
    fn query(&self, query: &str, accept: &str) -> Result<Response, EngineError> {
        (**self).query(query, accept)
    }

    fn update(&self, update: &str) -> Result<Response, EngineError> {
        (**self).update(update)
    }
}

impl<S: SparqlService + ?Sized> SparqlService for std::sync::Arc<S> {
    fn query(&self, query: &str, accept: &str) -> Result<Response, EngineError> {
        (**self).query(query, accept)
    }

    fn update(&self, update: &str) -> Result<Response, EngineError> {
        (**self).update(update)
    }
}

/// A SPARQL endpoint reached over HTTP.
///
/// Requests are sent as `POST` with `application/x-www-form-urlencoded`
/// bodies. Additional form parameters (e.g. an access token or an
/// engine-specific timeout) can be attached with [`HttpEndpoint::with_param`].
#[derive(Debug, Clone)]
pub struct HttpEndpoint {
    agent: ureq::Agent,
    query_url: String,
    update_url: String,
    params: Vec<(String, String)>,
    timeout: Duration,
}

/// The largest response body that is read.
const MAX_BODY: u64 = 256 * 1024 * 1024;

impl HttpEndpoint {
    /// Creates an endpoint that sends queries and updates to `url`.
    #[must_use]
    pub fn new(url: impl Into<String>, timeout: Duration) -> Self {
        let url = url.into();
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            // A fresh connection per request: with keep-alive, servers that
            // write responses in several segments without TCP_NODELAY (such
            // as QLever) stall on delayed ACKs for ~40 ms per request.
            .max_idle_connections(0)
            .build()
            .into();
        Self {
            agent,
            update_url: url.clone(),
            query_url: url,
            params: Vec::new(),
            timeout,
        }
    }

    /// Sends updates to another URL than queries.
    #[must_use]
    pub fn with_update_url(mut self, url: impl Into<String>) -> Self {
        self.update_url = url.into();
        self
    }

    /// Adds a form parameter to every request.
    #[must_use]
    pub fn with_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.push((name.into(), value.into()));
        self
    }

    /// The query URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.query_url
    }

    fn post(
        &self,
        url: &str,
        field: &str,
        text: &str,
        accept: &str,
    ) -> Result<Response, EngineError> {
        let start = Instant::now();
        let mut form: Vec<(&str, &str)> = vec![(field, text)];
        form.extend(self.params.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let result = self
            .agent
            .post(url)
            .header("Accept", accept)
            .send_form(form);
        let mut response = result.map_err(|e| self.convert(e))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()
            .map_err(|e| self.convert(e))?;
        Ok(Response {
            status,
            content_type,
            body,
            elapsed: start.elapsed(),
        })
    }

    fn convert(&self, error: ureq::Error) -> EngineError {
        match error {
            ureq::Error::Timeout(_) => EngineError::Timeout(self.timeout),
            ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                EngineError::Timeout(self.timeout)
            }
            other => EngineError::Connection(other.to_string()),
        }
    }
}

impl SparqlService for HttpEndpoint {
    fn query(&self, query: &str, accept: &str) -> Result<Response, EngineError> {
        self.post(&self.query_url, "query", query, accept)
    }

    fn update(&self, update: &str) -> Result<Response, EngineError> {
        self.post(&self.update_url, "update", update, ACCEPT_RESULTS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qlever_style_json() {
        let body = r#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"http://example.org/a"}},{"s":{"type":"literal","value":"x","xml:lang":"en"}}]},"meta":{"query-time-ms":0,"result-size-total":2}}"#;
        let QueryResults::Solutions(s) = parse_json_results(body.as_bytes()).unwrap() else {
            panic!("expected solutions");
        };
        assert_eq!(s.rows.len(), 2);
        assert_eq!(
            parse_json_results(br#"{"boolean":true,"head":{}}"#).unwrap(),
            QueryResults::Boolean(true)
        );
    }

    #[test]
    fn parses_json_solutions_with_extra_keys() {
        let body = r#"{"head":{"vars":["p","c"]},"results":{"bindings":[
            {"p":{"type":"uri","value":"http://ex/p"},
             "c":{"datatype":"http://www.w3.org/2001/XMLSchema#int","type":"literal","value":"31"}},
            {"c":{"type":"literal","value":"x"}}]},"meta":{"query-time-ms":6}}"#;
        let response = Response {
            status: 200,
            content_type: Some("application/sparql-results+json".to_owned()),
            body: body.to_owned(),
            elapsed: Duration::ZERO,
        };
        let QueryResults::Solutions(s) = response.results().unwrap() else {
            panic!("expected solutions");
        };
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.column("p").count(), 1);
        assert_eq!(s.column("c").count(), 2);
    }
}
