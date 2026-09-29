//! The random query generator.
//!
//! Generation proceeds strictly in text order: every token is appended to a
//! [`TokenWriter`](crate::ast::TokenWriter) as soon as it is decided, using
//! the same serialization as the syntax tree. Whenever a value is needed, the
//! [`CompletionProvider`] receives the text written so far, which is exactly
//! a prefix of the final query.
//!
//! Queries are valid by construction: the generator tracks which variables
//! are in scope (certainly or possibly bound), keeps `BIND` and `AS` targets
//! fresh, respects the projection rules of aggregate queries, scopes blank
//! node labels to their basic graph pattern, and so on. Clauses that come
//! before the pattern they depend on (the `SELECT` clause, `CONSTRUCT`
//! templates, `DESCRIBE` targets, update templates) use *planned* variables,
//! which the following `WHERE` clause preferentially binds.
//!
//! ```
//! use sparqlright::generator::Generator;
//! use sparqlright::provider::FixedProvider;
//!
//! let generator = Generator::default();
//! let mut provider = FixedProvider::example();
//! let generated = generator.query(42, &mut provider);
//! assert_eq!(generated.text, generated.query.to_string());
//! // The same seed and provider answers give the same query.
//! assert_eq!(generator.query(42, &mut provider).text, generated.text);
//! ```

mod choices;
mod context;
mod expr;
mod pattern;
mod query;
mod terms;
mod update;

use std::collections::BTreeMap;

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

use crate::ast::{Query, Update};
use crate::config::GeneratorConfig;
use crate::extension::Extension;
use crate::provider::CompletionProvider;

use choices::ChoiceSource;
use context::Gen;

/// Generates random SPARQL queries and updates.
#[derive(Debug, Default)]
pub struct Generator {
    config: GeneratorConfig,
    extensions: Vec<Box<dyn Extension>>,
}

/// Counters collected while generating one query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GenerationStats {
    /// How often each grammar production was generated.
    pub productions: BTreeMap<&'static str, u64>,
    /// Requests sent to the provider.
    pub provider_calls: u64,
    /// Requests that failed.
    pub provider_errors: u64,
    /// Requests without a usable suggestion, answered by the fallback vocabulary.
    pub fallbacks: u64,
    /// Weights disabled by swarm testing for this query.
    pub disabled_features: Vec<String>,
}

impl GenerationStats {
    /// Adds the counters of `other` (except the disabled features).
    pub fn merge(&mut self, other: &Self) {
        for (k, v) in &other.productions {
            *self.productions.entry(k).or_insert(0) += v;
        }
        self.provider_calls += other.provider_calls;
        self.provider_errors += other.provider_errors;
        self.fallbacks += other.fallbacks;
    }
}

/// A generated query.
#[derive(Debug, Clone)]
pub struct GeneratedQuery {
    /// The seed it was generated from.
    pub seed: u64,
    /// The syntax tree.
    pub query: Query,
    /// The query text; equal to `query.to_string()`.
    pub text: String,
    /// Statistics of the generation.
    pub stats: GenerationStats,
}

/// A generated update request.
#[derive(Debug, Clone)]
pub struct GeneratedUpdate {
    /// The seed it was generated from.
    pub seed: u64,
    /// The syntax tree.
    pub update: Update,
    /// The update text; equal to `update.to_string()`.
    pub text: String,
    /// Statistics of the generation.
    pub stats: GenerationStats,
}

impl Generator {
    /// Creates a generator with the given configuration.
    #[must_use]
    pub fn new(config: GeneratorConfig) -> Self {
        Self {
            config,
            extensions: Vec::new(),
        }
    }

    /// Registers an engine-specific extension.
    #[must_use]
    pub fn with_extension(mut self, extension: impl Extension + 'static) -> Self {
        self.extensions.push(Box::new(extension));
        self
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &GeneratorConfig {
        &self.config
    }

    /// Generates a query from `seed`, taking values from `provider`.
    ///
    /// The result is fully determined by the seed, the configuration and the
    /// answers of the provider.
    pub fn query(&self, seed: u64, provider: &mut dyn CompletionProvider) -> GeneratedQuery {
        let choices = ChoiceSource::Seeded(ChaCha8Rng::seed_from_u64(seed));
        self.query_with(seed, choices, provider)
    }

    /// Generates an update request from `seed`, taking values from `provider`.
    pub fn update(&self, seed: u64, provider: &mut dyn CompletionProvider) -> GeneratedUpdate {
        let choices = ChoiceSource::Seeded(ChaCha8Rng::seed_from_u64(seed));
        self.update_with(seed, choices, provider)
    }

    /// Generates a query whose random choices are read from `data`.
    ///
    /// This is the entry point for coverage-guided fuzzing: a fuzzer mutates
    /// `data`, and every byte string yields a valid query. Once `data` is
    /// exhausted, all optional constructs are omitted. The `seed` of the
    /// result is 0.
    pub fn query_from_bytes(
        &self,
        data: &[u8],
        provider: &mut dyn CompletionProvider,
    ) -> GeneratedQuery {
        self.query_with(0, ChoiceSource::from_bytes(data), provider)
    }

    /// Generates an update request whose random choices are read from `data`.
    pub fn update_from_bytes(
        &self,
        data: &[u8],
        provider: &mut dyn CompletionProvider,
    ) -> GeneratedUpdate {
        self.update_with(0, ChoiceSource::from_bytes(data), provider)
    }

    fn query_with(
        &self,
        seed: u64,
        choices: ChoiceSource,
        provider: &mut dyn CompletionProvider,
    ) -> GeneratedQuery {
        let mut gen_ = Gen::new(&self.config, &self.extensions, provider, choices);
        let query = gen_.query();
        let (text, stats) = gen_.finish();
        debug_assert_eq!(
            text,
            query.to_string(),
            "generator output diverged from the syntax tree"
        );
        GeneratedQuery {
            seed,
            query,
            text,
            stats,
        }
    }

    fn update_with(
        &self,
        seed: u64,
        choices: ChoiceSource,
        provider: &mut dyn CompletionProvider,
    ) -> GeneratedUpdate {
        let mut gen_ = Gen::new(&self.config, &self.extensions, provider, choices);
        let update = gen_.update();
        let (text, stats) = gen_.finish();
        debug_assert_eq!(
            text,
            update.to_string(),
            "generator output diverged from the syntax tree"
        );
        GeneratedUpdate {
            seed,
            update,
            text,
            stats,
        }
    }
}
