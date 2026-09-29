//! # sparqlright
//!
//! A random SPARQL 1.1 query generator for testing SPARQL engines, in the
//! spirit of [Csmith](https://github.com/csmith-project/csmith) and
//! [SQLsmith](https://github.com/anse1/sqlsmith).
//!
//! The crate is organised as follows:
//!
//! * [`ast`] — a syntax tree covering the complete SPARQL 1.1 grammar
//!   (queries and updates), with a canonical serializer;
//! * [`generator`] — the [`Generator`], which produces queries that are valid
//!   by construction, in text order;
//! * [`provider`] — pluggable sources of the *values* used during generation
//!   ([`CompletionProvider`]): the engine under test, an in-memory dataset,
//!   a fixed vocabulary, record/replay, or anything else such as an LLM;
//! * [`config`] — weights, limits and swarm testing;
//! * [`engine`] — a SPARQL protocol client;
//! * [`fuzz`] — the loop that runs generated queries against an engine and
//!   classifies the outcomes;
//! * [`validate`] — validation of generated requests with an independent parser;
//! * [`extension`] — hooks for engine-specific extensions of the standard.
//!
//! ```
//! use sparqlright::{Generator, GeneratorConfig};
//! use sparqlright::provider::FixedProvider;
//!
//! let generator = Generator::new(GeneratorConfig::default());
//! let mut provider = FixedProvider::example();
//! for seed in 0..10 {
//!     let q = generator.query(seed, &mut provider);
//!     assert!(sparqlright::validate::validate_query(&q.text).error().is_none());
//! }
//! ```

pub mod ast;
pub mod config;
pub mod engine;
pub mod extension;
pub mod fuzz;
pub mod generator;
pub mod provider;
pub mod validate;

pub use config::GeneratorConfig;
pub use generator::{GeneratedQuery, GeneratedUpdate, Generator};
pub use provider::CompletionProvider;
