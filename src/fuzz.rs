//! Running generated queries against an engine.
//!
//! The [`Fuzzer`] generates queries, optionally validates them with an
//! independent parser (spargebra), sends them to the engine and classifies
//! the outcome. The oracle is deliberately simple — crashes, internal
//! errors, and the rejection of valid queries — in the tradition of `SQLsmith`;
//! result-checking oracles build on top of this loop.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ast::QueryForm;
use crate::engine::{
    ACCEPT_GRAPH, ACCEPT_RESULTS, EngineError, QueryResults, Response, SparqlService,
};
use crate::generator::{GenerationStats, Generator};
use crate::provider::CompletionProvider;
use crate::validate::{validate_query, validate_update};

/// Mixed into the seed to decide whether a request is an update, so that
/// the decision is independent of the request's own random choices.
const UPDATE_SALT: u64 = 0x5EED_0F0B_DA7E;

/// The class of an error reported by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ErrorClass {
    /// The engine claims the query is syntactically invalid.
    Syntax,
    /// The engine does not support a feature used by the query.
    Unsupported,
    /// A memory, size or time limit of the engine was exceeded.
    ResourceLimit,
    /// An internal error: HTTP 5xx or a message indicating a bug.
    Internal,
    /// Any other error.
    Other,
}

/// Substring rules (case-insensitive) used to classify error messages.
///
/// Rules are checked in the order internal, resource limit, unsupported,
/// syntax. The defaults cover common messages of several engines and can be
/// extended per engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ErrorRules {
    /// Messages indicating an internal error.
    pub internal: Vec<String>,
    /// Messages indicating a resource limit.
    pub resource_limit: Vec<String>,
    /// Messages indicating an unsupported feature.
    pub unsupported: Vec<String>,
    /// Messages indicating a syntax error.
    pub syntax: Vec<String>,
}

impl Default for ErrorRules {
    fn default() -> Self {
        let v = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
        Self {
            internal: v(&[
                "assertion",
                "assert failed",
                "internal error",
                "nullpointerexception",
                "classcastexception",
                "indexoutofboundsexception",
                "stackoverflowerror",
                "panicked",
                "segmentation fault",
                "unreachable",
                "should not happen",
                "std::",
            ]),
            resource_limit: v(&[
                "out of memory",
                "memory limit",
                "exceeds the limit",
                "too large",
                "timed out",
                "timeout",
                "cancelled",
            ]),
            unsupported: v(&[
                "not supported",
                "unsupported",
                "not yet implemented",
                "not implemented",
            ]),
            syntax: v(&[
                "invalid sparql",
                "syntax error",
                "parse error",
                "lexical error",
                "mismatched input",
                "encountered",
                "unexpected",
            ]),
        }
    }
}

impl ErrorRules {
    /// Classifies an error response.
    #[must_use]
    pub fn classify(&self, status: u16, body: &str) -> ErrorClass {
        let body = body.to_lowercase();
        let any = |rules: &[String]| rules.iter().any(|r| body.contains(&r.to_lowercase()));
        if any(&self.internal) {
            ErrorClass::Internal
        } else if any(&self.resource_limit) {
            ErrorClass::ResourceLimit
        } else if any(&self.unsupported) {
            ErrorClass::Unsupported
        } else if any(&self.syntax) {
            ErrorClass::Syntax
        } else if status >= 500 {
            ErrorClass::Internal
        } else {
            ErrorClass::Other
        }
    }
}

/// What happened when a query was sent to the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Outcome {
    /// The engine answered successfully.
    Success {
        /// The number of solutions (SELECT) or 1/0 (ASK); `None` for graphs.
        results: Option<usize>,
    },
    /// The engine returned an error.
    Error {
        /// The class of the error.
        class: ErrorClass,
        /// The HTTP status.
        status: u16,
        /// The (truncated) message.
        message: String,
    },
    /// The request timed out.
    Timeout {
        /// Whether the engine answered a simple query afterwards. If not, the
        /// timed-out query keeps the engine busy despite the timeout, which
        /// blocks it for everyone else.
        server_responsive: bool,
    },
    /// The connection failed.
    ConnectionLost {
        /// Whether the engine answered a simple query afterwards.
        alive_afterwards: bool,
        /// The error message.
        message: String,
    },
}

impl Outcome {
    /// A short label for statistics, e.g. `error:syntax`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Success { results: Some(0) } => "ok:empty".to_owned(),
            Self::Success { .. } => "ok".to_owned(),
            Self::Error { class, .. } => format!("error:{}", format!("{class:?}").to_lowercase()),
            Self::Timeout {
                server_responsive: true,
            } => "timeout".to_owned(),
            Self::Timeout {
                server_responsive: false,
            } => "timeout:blocked".to_owned(),
            Self::ConnectionLost {
                alive_afterwards: false,
                ..
            } => "crash".to_owned(),
            Self::ConnectionLost { .. } => "connection".to_owned(),
        }
    }

    /// Whether the outcome points to a potential bug of the engine.
    ///
    /// Timeouts count only if `timeouts` is set: on small datasets they
    /// usually indicate hangs or performance bugs, on large ones they are
    /// expected. A timeout after which the engine stays unresponsive always
    /// counts.
    #[must_use]
    pub fn is_finding(&self, timeouts: bool) -> bool {
        match self {
            Self::Error { class, .. } => matches!(class, ErrorClass::Internal | ErrorClass::Syntax),
            Self::ConnectionLost { .. } => true,
            Self::Timeout { server_responsive } => timeouts || !server_responsive,
            Self::Success { .. } => false,
        }
    }

    /// Whether the engine stopped answering after this test (it crashed or
    /// is still busy with a query that ignores the timeout).
    #[must_use]
    pub fn engine_unavailable(&self) -> bool {
        matches!(
            self,
            Self::ConnectionLost {
                alive_afterwards: false,
                ..
            } | Self::Timeout {
                server_responsive: false
            }
        )
    }

    /// A one-character progress symbol, as in `SQLsmith`'s verbose mode.
    #[must_use]
    pub fn symbol(&self) -> char {
        match self {
            Self::Success { results: Some(0) } => '_',
            Self::Success { .. } => '.',
            Self::Error { class, .. } => match class {
                ErrorClass::Syntax => 'S',
                ErrorClass::Unsupported => 'u',
                ErrorClass::ResourceLimit => 'r',
                ErrorClass::Internal => 'E',
                ErrorClass::Other => 'e',
            },
            Self::Timeout {
                server_responsive: true,
            } => 't',
            Self::Timeout {
                server_responsive: false,
            } => 'H',
            Self::ConnectionLost { .. } => 'C',
        }
    }
}

/// One executed test.
#[derive(Debug, Clone, Serialize)]
pub struct TestCase {
    /// The seed of the query.
    pub seed: u64,
    /// The request text.
    pub text: String,
    /// Whether this is an update request.
    pub is_update: bool,
    /// Why the independent parser rejected the request, if it did.
    pub validator_error: Option<String>,
    /// What the engine did.
    pub outcome: Outcome,
    /// How long the engine took.
    pub elapsed: Duration,
}

/// Settings of a fuzzing run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FuzzConfig {
    /// The seed of the first query; query `i` uses `seed + i`.
    pub seed: u64,
    /// The number of requests.
    pub count: u64,
    /// The fraction of requests that are updates.
    pub update_ratio: f64,
    /// Whether to validate requests with spargebra first.
    pub validate: bool,
    /// Where to write findings (as `.rq`/`.ru` plus `.json`), if anywhere.
    pub output: Option<PathBuf>,
    /// Whether to stop after the engine stopped answering (a crash, or a
    /// timed-out query that keeps it busy).
    pub stop_on_crash: bool,
    /// Whether timeouts are reported as findings.
    pub timeouts_are_findings: bool,
    /// Error classification rules.
    pub error_rules: ErrorRules,
}

impl Default for FuzzConfig {
    fn default() -> Self {
        Self {
            seed: 0,
            count: 1000,
            update_ratio: 0.0,
            validate: true,
            output: None,
            stop_on_crash: true,
            timeouts_are_findings: true,
            error_rules: ErrorRules::default(),
        }
    }
}

/// Aggregated results of a run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FuzzReport {
    /// Requests sent.
    pub tests: u64,
    /// Counts per [`Outcome::label`].
    pub outcomes: BTreeMap<String, u64>,
    /// Requests rejected by the independent parser.
    pub validator_rejections: u64,
    /// Seeds of findings.
    pub findings: Vec<u64>,
    /// Merged generation statistics.
    pub generation: GenerationStats,
    /// Total time spent in the engine.
    pub engine_time: Duration,
}

impl fmt::Display for FuzzReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "tests: {}", self.tests)?;
        for (label, n) in &self.outcomes {
            writeln!(f, "  {label:<22} {n}")?;
        }
        writeln!(f, "validator rejections: {}", self.validator_rejections)?;
        writeln!(f, "findings: {}", self.findings.len())?;
        writeln!(
            f,
            "provider calls: {} (errors {}, fallbacks {})",
            self.generation.provider_calls,
            self.generation.provider_errors,
            self.generation.fallbacks
        )?;
        write!(f, "engine time: {:.1?}", self.engine_time)
    }
}

/// Generates requests and runs them against an engine.
#[derive(Debug)]
pub struct Fuzzer<'a, S> {
    generator: &'a Generator,
    service: S,
    config: FuzzConfig,
}

impl<'a, S: SparqlService> Fuzzer<'a, S> {
    /// Creates a fuzzer.
    pub fn new(generator: &'a Generator, service: S, config: FuzzConfig) -> Self {
        Self {
            generator,
            service,
            config,
        }
    }

    /// Runs the configured number of tests, calling `on_test` after each.
    ///
    /// # Errors
    ///
    /// Returns an error if findings cannot be written to the output directory.
    pub fn run(
        &self,
        provider: &mut dyn CompletionProvider,
        mut on_test: impl FnMut(&TestCase),
    ) -> std::io::Result<FuzzReport> {
        if let Some(dir) = &self.config.output {
            fs::create_dir_all(dir)?;
        }
        let mut report = FuzzReport::default();
        for i in 0..self.config.count {
            let seed = self.config.seed.wrapping_add(i);
            let is_update = self.is_update(seed);
            let (test, stats) = self.run_one(seed, is_update, provider);
            report.tests += 1;
            report.generation.merge(&stats);
            report.engine_time += test.elapsed;
            *report.outcomes.entry(test.outcome.label()).or_insert(0) += 1;
            if test.validator_error.is_some() {
                report.validator_rejections += 1;
            }
            // The engine no longer answers: it crashed or is blocked.
            let crashed = test.outcome.engine_unavailable();
            if test.outcome.is_finding(self.config.timeouts_are_findings)
                || test.validator_error.is_some()
            {
                report.findings.push(seed);
                self.save(&test)?;
            }
            on_test(&test);
            if crashed && self.config.stop_on_crash {
                break;
            }
        }
        Ok(report)
    }

    /// Whether the request with this seed is an update (deterministic per seed).
    fn is_update(&self, seed: u64) -> bool {
        use rand::{RngExt, SeedableRng};
        self.config.update_ratio > 0.0
            && rand_chacha::ChaCha8Rng::seed_from_u64(seed ^ UPDATE_SALT)
                .random_bool(self.config.update_ratio.min(1.0))
    }

    /// Generates and runs the request for `seed`.
    pub fn run_one(
        &self,
        seed: u64,
        is_update: bool,
        provider: &mut dyn CompletionProvider,
    ) -> (TestCase, GenerationStats) {
        let (text, stats, accept, validator_error) = if is_update {
            let u = self.generator.update(seed, provider);
            let err = self
                .config
                .validate
                .then(|| validate_update(&u.text).error().map(str::to_owned))
                .flatten();
            (u.text, u.stats, ACCEPT_RESULTS, err)
        } else {
            let q = self.generator.query(seed, provider);
            let accept = match q.query.form {
                QueryForm::Construct(_) | QueryForm::Describe(_) => ACCEPT_GRAPH,
                QueryForm::Select(_) | QueryForm::Ask(_) => ACCEPT_RESULTS,
            };
            let err = self
                .config
                .validate
                .then(|| validate_query(&q.text).error().map(str::to_owned))
                .flatten();
            (q.text, q.stats, accept, err)
        };
        let start = Instant::now();
        let response = if is_update {
            self.service.update(&text)
        } else {
            self.service.query(&text, accept)
        };
        let elapsed = start.elapsed();
        let outcome = self.classify(response, accept == ACCEPT_RESULTS && !is_update);
        (
            TestCase {
                seed,
                text,
                is_update,
                validator_error,
                outcome,
                elapsed,
            },
            stats,
        )
    }

    fn classify(&self, response: Result<Response, EngineError>, parse_results: bool) -> Outcome {
        match response {
            Ok(r) if r.is_success() => {
                let results = if parse_results {
                    match r.results() {
                        Ok(QueryResults::Solutions(s)) => Some(s.rows.len()),
                        Ok(QueryResults::Boolean(b)) => Some(usize::from(b)),
                        Err(e) => {
                            return Outcome::Error {
                                class: ErrorClass::Internal,
                                status: r.status,
                                message: format!("unparsable results: {e}"),
                            };
                        }
                    }
                } else {
                    None
                };
                Outcome::Success { results }
            }
            Ok(r) => {
                let message = error_message(&r.body);
                Outcome::Error {
                    class: self.config.error_rules.classify(r.status, &message),
                    status: r.status,
                    message: truncate(&message, 2000),
                }
            }
            Err(EngineError::Timeout(_)) => Outcome::Timeout {
                server_responsive: self.is_alive(),
            },
            Err(e) => Outcome::ConnectionLost {
                alive_afterwards: self.is_alive(),
                message: e.to_string(),
            },
        }
    }

    /// Whether the engine answers a trivial query (retrying for a while).
    #[must_use]
    pub fn is_alive(&self) -> bool {
        for attempt in 0..5 {
            if self
                .service
                .query("ASK {}", ACCEPT_RESULTS)
                .is_ok_and(|r| r.is_success())
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200 * (attempt + 1)));
        }
        false
    }

    fn save(&self, test: &TestCase) -> std::io::Result<()> {
        let Some(dir) = &self.config.output else {
            return Ok(());
        };
        let label = if test.validator_error.is_some() {
            "invalid".to_owned()
        } else {
            test.outcome.label().replace(':', "-")
        };
        let stem = format!("{}-{label}", test.seed);
        let ext = if test.is_update { "ru" } else { "rq" };
        fs::write(dir.join(format!("{stem}.{ext}")), &test.text)?;
        let meta = serde_json::to_string_pretty(test).map_err(std::io::Error::other)?;
        fs::write(dir.join(format!("{stem}.json")), meta)
    }
}

/// The error message of a response body; engines often wrap it in JSON.
fn error_message(body: &str) -> String {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_str::<serde_json::Value>(body)
    else {
        return body.trim().to_owned();
    };
    ["exception", "message", "error", "detail"]
        .iter()
        .find_map(|k| object.get(*k).and_then(serde_json::Value::as_str))
        .map_or_else(|| body.trim().to_owned(), str::to_owned)
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_engines_are_findings() {
        let blocked = Outcome::Timeout {
            server_responsive: false,
        };
        let slow = Outcome::Timeout {
            server_responsive: true,
        };
        assert!(blocked.is_finding(false) && blocked.engine_unavailable());
        assert!(!slow.is_finding(false) && slow.is_finding(true) && !slow.engine_unavailable());
        assert_eq!(blocked.label(), "timeout:blocked");
        assert_eq!(blocked.symbol(), 'H');
    }

    #[test]
    fn classification_prefers_internal_errors() {
        let rules = ErrorRules::default();
        assert_eq!(
            rules.classify(400, "Invalid SPARQL query: mismatched input"),
            ErrorClass::Syntax
        );
        assert_eq!(
            rules.classify(400, "LATERAL is not supported"),
            ErrorClass::Unsupported
        );
        assert_eq!(
            rules.classify(500, "Assertion `x` failed"),
            ErrorClass::Internal
        );
        assert_eq!(rules.classify(500, "boom"), ErrorClass::Internal);
        assert_eq!(rules.classify(400, "boom"), ErrorClass::Other);
    }
}
