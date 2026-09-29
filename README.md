# SPARQLright

A random SPARQL 1.1 query generator for testing SPARQL engines, in the spirit
of [Csmith](https://github.com/csmith-project/csmith) (C compilers) and
[SQLsmith](https://github.com/anse1/sqlsmith) (SQL databases).

SPARQLright generates queries and updates that are **valid by construction**
over the complete SPARQL 1.1 grammar, sends them to an engine and reports
crashes, internal errors and rejections of valid queries. The values in the
queries (IRIs, literals, variables) come from a pluggable **completion
provider** — by default the engine under test itself, so that generated
patterns match its data.

## The name

SPARQLright is pronounced "sparkle-wright" /ˈspɑːkəl raɪt/. In English,
*right* and *wright* sound the same, so the name can be read two ways:

* **SPARQL-wright**: a *wright* is a craftsman who makes things, as in
  playwright or wheelwright. It means the same as *smith*, so the name
  continues the line of Csmith and SQLsmith.
* **SPARQL, right?**: the generated queries are correct by construction, and
  every test asks the engine whether it gets them right.

The crate and the binary are called `sparqlright`.

## Usage

```sh
# Print 5 queries using a built-in vocabulary
cargo run -- generate -n 5

# Print queries with values taken from an RDF file, validate them with spargebra
cargo run -- generate -n 100 --provider data:tests/data/small.ttl --check

# Fuzz an endpoint; values come from the endpoint itself
cargo run --release -- fuzz -e http://localhost:7001 -n 1000 -o findings

# Print the default configuration (weights, limits, swarm testing)
cargo run -- config > sparqlright.toml
```

`fuzz` writes every finding as `<seed>-<outcome>.rq` plus a `.json` file
with the engine's answer, and a `report.json` with statistics. Query `i` of a
run uses seed `seed + i`, so any query can be regenerated on its own with
`generate --seed S -n 1` (given the same data and configuration).

Progress symbols (as in SQLsmith): `.` results, `_` empty results,
`S` syntax error, `u` unsupported, `r` resource limit, `E` internal error,
`e` other error, `t` timeout, `H` timeout after which the engine stays
busy (the query ignores the timeout and blocks the engine), `C` connection
lost. A run stops when the engine crashed or is blocked, unless
`--keep-going` is given.

The engine provider's own queries use `--provider-timeout` (default 5 s).
After repeated failures of its context-sensitive queries, the provider
suspends them for a while and uses cheap, cached context-free queries, so
generation stays fast even when the engine struggles.

## Architecture

```
src/
  ast/         syntax tree of the complete SPARQL 1.1 grammar + canonical serializer
  generator/   the generator: queries, patterns, paths, expressions, updates
  provider/    completion providers (where values come from)
  config.rs    weights, limits, swarm testing (TOML)
  engine.rs    SPARQL protocol client
  fuzz.rs      the fuzzing loop and outcome classification
  validate.rs  validation with an independent parser (spargebra)
  extension.rs hooks for engine-specific extensions
```

### Syntax tree

`ast` follows the 173 productions of the
[SPARQL 1.1 grammar](https://www.w3.org/TR/sparql11-query/#sparqlGrammar),
including purely syntactic alternatives: prefixed names, relative IRIs,
`BASE`, the `;` and `,` abbreviations, empty `;` entries, `[ ... ]` and
`( ... )` nodes, all four quote styles, signed numbers and the special
`?a -1` form of subtraction, `$` variables, optional dots, `WHERE`-less
queries, `CONSTRUCT WHERE`, all built-in functions and aggregates, property
paths, `VALUES` in both forms, sub-queries, and all update operations.
Expressions are an operator tree whose serializer adds exactly the
parentheses the grammar needs. Lexical variations that parsers normalize away
(keyword case, comments, whitespace, codepoint escapes) are not generated.

### Generation in text order

The generator writes the query token by token into the same writer the
syntax tree uses for serialization. At any point, the text written so far is
a prefix of the final query; this is what a provider receives. Validity is
ensured by tracking variable scope (certainly/possibly bound, per group and
sub-query), keeping `BIND` and `AS` targets fresh, respecting the projection
rules of aggregate queries, scoping blank node labels to their basic graph
pattern, and so on. Clauses that precede the pattern they depend on (the
`SELECT` clause, `CONSTRUCT` templates, `DESCRIBE` targets, update templates)
use *planned* variables, which the following `WHERE` clause binds first.

Every choice is weighted (see `sparqlright config`); a weight of zero
disables a feature. With `--swarm`, a random subset of features is disabled
per query ([swarm testing](https://users.cs.utah.edu/~regehr/papers/swarm12.pdf)).
Generation statistics count every production, so silently dead features are
visible (`generate --stats`).

### Completion providers

Whenever a value is needed, the generator calls a
[`CompletionProvider`](src/provider/mod.rs):

```rust
pub trait CompletionProvider {
    fn suggest(&mut self, request: &CompletionRequest<'_>) -> Result<Vec<Suggestion>, ProviderError>;
    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> { Ok(Vec::new()) }
}
```

For the partial query `SELECT * WHERE { ?s ?o`, the request contains the
text `SELECT * WHERE { ?s ?o`, the slot (`Object`), the allowed kinds
(variable, IRI, literal), the triple being written, the triple patterns
written so far, the variables in scope and a fresh variable name. A provider
returns weighted suggestions such as `?p`, `1`, `"some literal"` or
`<http://www.wikidata.org/entity/Q1234>`; the generator discards invalid ones
and picks one at random. Implementations:

| Provider | Values from |
|---|---|
| `EngineProvider` | the engine under test, via context-sensitive autocompletion queries as in the QLever UI (Bast et al., CIKM 2022): the most frequent values of a hole variable in the part of the partial query connected to the current triple, falling back to the triple alone and to generic queries; cached |
| `GraphProvider` | an in-memory RDF file (Turtle, TriG, N-Triples, N-Quads); deterministic |
| `FixedProvider` | a fixed list of terms; for unit tests |
| `VariableProvider` | variables in scope and a fresh variable |
| `MixProvider` | a weighted combination of providers, per slot (`MixProvider::variables_and(terms)` is the default setup) |
| `RecordingProvider` / `ReplayProvider` | record the answers of any provider and replay them, making engine-guided runs reproducible without the engine |

An LLM-based provider only has to implement `suggest`, e.g. by prompting
with `request.partial_query` and parsing the model's answer into
`Candidate::Term` or `Candidate::Variable` values.

### Engine extensions

The [`Extension`](src/extension.rs) trait lets an engine-specific module add
functions (with signatures), prefixes and whole group elements, all written
in standard syntax (function calls, magic predicates, `SERVICE`). No
extensions ship yet; generated queries are standard SPARQL 1.1.

## Testing

```sh
cargo test                 # unit tests, generator properties, QLever end-to-end
cargo clippy --all-targets # pedantic lints are enabled in Cargo.toml
```

* `tests/generation.rs` checks, for thousands of seeds and several
  configurations (default, swarm testing with deep nesting, all rare features
  forced, updates), that every request parses with spargebra, that the
  generated text equals the serialization of the syntax tree, that every
  partial query handed to a provider is a prefix of the final text, that
  generation is deterministic and that record/replay reproduces queries.
* `tests/qlever.rs` builds a QLever index of `tests/data/small.ttl`, starts
  a server on a free port and fuzzes it with the engine provider. The
  binaries are taken from `$QLEVER_BIN_DIR` or `../qlever/cmake-build-release`;
  the test is skipped if they are missing.

## Findings so far

Against QLever `v0.6.0-165-ga96c6ce5c` with the 233-triple test dataset:

* ``Assertion `variablesAreDisjoint` failed`` in `CartesianProductJoin.cpp`
  (internal error, several seeds);
* ``Assertion `!nextCandidates.empty()` failed`` — "Could not find a single
  candidate join for two optimized graph patterns" in `QueryPlanner.cpp`;
* valid queries rejected: `ASK` with `LIMIT`/`OFFSET`; `HAVING` without
  `GROUP BY` (implicit grouping); the regex flag `x`;
* a ~40 ms stall per request on keep-alive connections, probably because the
  server does not set `TCP_NODELAY` (the client now opens a fresh connection
  per request).

In the Rust SPARQL libraries used here:

* spargebra 0.4.7 rejects `GROUP BY (?x AS ?k)` followed by `SELECT ?k`
  (the alias of a plain variable is dropped), and blank node labels inside an
  `EXISTS` below a unary `!` (the parser backtracks after recording the
  label). Both are valid SPARQL 1.1; `validate.rs` recognizes them.
* sparesults 0.3.4 rejects JSON results with a member after `results`
  (QLever appends `meta`), so `engine.rs` parses the JSON format itself.

## Background

The design is informed by a study of Csmith, C-Reduce, swarm testing,
SQLsmith, SQLancer (PQS, NoREC, TLP) and its successors, graph database
testing, and the formal semantics of SPARQL.

## Next steps

* Result oracles: ternary logic partitioning of `FILTER`s, NoREC,
  equivalence rewrites, containment, and differential testing across engines
  with result canonicalization.
* Test-case reduction on the syntax tree.
* Fewer empty results (e.g. fewer constant `FILTER`s, subject suggestions
  connected to the context).
* Generated datasets, SPARQL 1.2.
