//! Properties of the generator that must hold for every seed.

use sparqlright::Generator;
use sparqlright::config::GeneratorConfig;
use sparqlright::provider::{
    CompletionProvider, CompletionRequest, FixedProvider, GraphProvider, MixProvider, Namespace,
    ProviderError, RecordingProvider, ReplayProvider, Slot, Suggestion, VariableProvider,
};
use sparqlright::validate::{validate_query, validate_update};

const SEEDS: u64 = 3000;

fn dataset_provider() -> GraphProvider {
    GraphProvider::from_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/small.ttl"))
        .expect("test data parses")
}

fn mixed(provider: impl CompletionProvider + 'static) -> MixProvider {
    MixProvider::new()
        .with_slot_weights(
            VariableProvider::default(),
            0.5,
            [
                (Slot::Predicate, 0.2),
                (Slot::ExpressionOperand, 0.0),
                (Slot::ValuesCell, 0.0),
            ],
        )
        .with(provider, 1.0)
}

/// Checks that every partial query is a prefix of the final text.
struct PrefixChecker<P> {
    inner: P,
    partials: Vec<String>,
}

impl<P: CompletionProvider> CompletionProvider for PrefixChecker<P> {
    fn suggest(
        &mut self,
        request: &CompletionRequest<'_>,
    ) -> Result<Vec<Suggestion>, ProviderError> {
        assert!(
            request.partial_query.starts_with(request.prologue),
            "prologue is not a prefix"
        );
        self.partials.push(request.partial_query.to_owned());
        self.inner.suggest(request)
    }

    fn namespaces(&mut self) -> Result<Vec<Namespace>, ProviderError> {
        self.inner.namespaces()
    }
}

fn check_queries(
    config: &GeneratorConfig,
    provider: impl CompletionProvider,
    seeds: std::ops::Range<u64>,
) {
    let generator = Generator::new(config.clone());
    let mut checker = PrefixChecker {
        inner: provider,
        partials: Vec::new(),
    };
    let mut failures = Vec::new();
    for seed in seeds {
        checker.partials.clear();
        let q = generator.query(seed, &mut checker);
        assert_eq!(q.text, q.query.to_string(), "seed {seed}");
        for partial in &checker.partials {
            assert!(
                q.text.starts_with(partial.as_str()),
                "seed {seed}: {partial:?} is not a prefix of {:?}",
                q.text
            );
        }
        if let Some(e) = validate_query(&q.text).error() {
            failures.push(format!("seed {seed}: {e}\n  {}", q.text));
        }
    }
    assert!(
        failures.is_empty(),
        "{} invalid queries:\n{}",
        failures.len(),
        failures[..failures.len().min(15)].join("\n")
    );
}

#[test]
fn queries_are_valid_with_fixed_vocabulary() {
    check_queries(
        &GeneratorConfig::default(),
        mixed(FixedProvider::example()),
        0..SEEDS,
    );
}

#[test]
fn queries_are_valid_with_dataset_provider() {
    check_queries(
        &GeneratorConfig::default(),
        mixed(dataset_provider()),
        0..SEEDS,
    );
}

#[test]
fn queries_are_valid_with_swarm_testing_and_deep_nesting() {
    let mut config = GeneratorConfig::default();
    config.swarm.enabled = true;
    config.limits.max_depth = 6;
    config.limits.max_expression_depth = 5;
    config.limits.budget = 1500;
    check_queries(&config, mixed(dataset_provider()), 0..SEEDS);
}

#[test]
fn queries_are_valid_with_every_feature_forced() {
    // Crank up rarely used alternatives.
    let mut config = GeneratorConfig::default();
    let w = &mut config.weights;
    w.form = sparqlright::config::FormWeights {
        select: 1.0,
        construct: 1.0,
        construct_where: 1.0,
        describe: 1.0,
        ask: 1.0,
    };
    w.pattern.service = 1.0;
    w.pattern.p_dot_after_pattern = 0.5;
    w.pattern.p_trailing_dot = 0.5;
    w.triple.p_path = 0.5;
    w.triple.p_blank_node_label = 0.3;
    w.triple.p_empty_entry = 0.3;
    w.triple.subject_collection = 3.0;
    w.triple.object_collection = 3.0;
    w.lexical.p_dollar = 0.3;
    w.lexical.p_exotic_name = 0.3;
    w.prologue.p_base = 0.5;
    w.data.p_from = 0.3;
    w.data.p_from_named = 0.3;
    w.data.p_trailing_values = 0.3;
    w.expression.signed_tail = 2.0;
    w.expression.p_unbound_variable = 0.2;
    w.select.aggregate = 5.0;
    w.modifier.p_order_by = 0.8;
    w.modifier.p_having = 0.8;
    check_queries(&config, mixed(dataset_provider()), 0..SEEDS);
}

#[test]
fn updates_are_valid() {
    let generator = Generator::default();
    let mut provider = mixed(dataset_provider());
    let mut failures = Vec::new();
    for seed in 0..SEEDS {
        let u = generator.update(seed, &mut provider);
        assert_eq!(u.text, u.update.to_string());
        if let Some(e) = validate_update(&u.text).error() {
            failures.push(format!("seed {seed}: {e}\n  {}", u.text));
        }
    }
    assert!(
        failures.is_empty(),
        "{} invalid updates:\n{}",
        failures.len(),
        failures[..failures.len().min(15)].join("\n")
    );
}

#[test]
fn generation_is_deterministic() {
    let generator = Generator::default();
    for seed in 0..200 {
        let a = generator.query(seed, &mut mixed(dataset_provider()));
        let b = generator.query(seed, &mut mixed(dataset_provider()));
        assert_eq!(a.text, b.text);
    }
}

#[test]
fn recorded_answers_replay_to_the_same_queries() {
    let generator = Generator::default();
    for seed in 0..100 {
        let mut recording = RecordingProvider::new(mixed(dataset_provider()));
        let original = generator.query(seed, &mut recording);
        let namespaces = recording.recorded_namespaces().unwrap_or_default().to_vec();
        let mut replay = ReplayProvider::new(recording.take_calls(), namespaces);
        let replayed = generator.query(seed, &mut replay);
        assert_eq!(original.text, replayed.text);
        assert_eq!(replay.remaining(), 0);
    }
}
