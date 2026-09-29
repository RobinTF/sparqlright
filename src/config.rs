//! Configuration of the generator.
//!
//! Every choice the generator makes is controlled by a weight (relative
//! frequency among alternatives) or a probability (fields prefixed `p_`).
//! Setting a weight or probability to zero disables the corresponding
//! feature. The configuration is (de)serializable, so it can be kept in a
//! TOML file; missing fields take their default values.
//!
//! [`SwarmConfig`] implements *swarm testing* (Groce et al., ISSTA 2012): for
//! each query a random subset of features is disabled, which lets features
//! that suppress each other's bugs appear apart.

use std::collections::BTreeMap;

use rand::RngExt;
use serde::{Deserialize, Serialize};

/// Declares a struct of `f64` weights with defaults and a field visitor.
macro_rules! weights {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            $( $(#[$fmeta:meta])* $field:ident : $default:expr ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: f64, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        impl $name {
            /// Calls `f` with the name and value of every weight.
            pub fn for_each_mut(&mut self, f: &mut dyn FnMut(&'static str, &mut f64)) {
                $( f(stringify!($field), &mut self.$field); )*
            }
        }
    };
}

weights! {
    /// Query forms.
    pub struct FormWeights {
        /// `SELECT`
        select: 10.0,
        /// `CONSTRUCT { template } WHERE { ... }`
        construct: 2.0,
        /// `CONSTRUCT WHERE { triples }`
        construct_where: 1.0,
        /// `DESCRIBE`
        describe: 1.0,
        /// `ASK`
        ask: 2.0,
    }
}

weights! {
    /// Prologue and IRI syntax.
    pub struct PrologueWeights {
        /// Probability to declare a `BASE` IRI.
        p_base: 0.1,
        /// Probability to declare each known namespace as `PREFIX`.
        p_prefix: 0.8,
        /// Probability to write an IRI as a prefixed name when possible.
        p_prefixed_name: 0.7,
        /// Probability to write an IRI relative to `BASE` when possible.
        p_relative_iri: 0.3,
        /// Probability to write `rdf:type` as `a` where allowed.
        p_a_keyword: 0.7,
    }
}

weights! {
    /// The `SELECT` clause.
    pub struct SelectWeights {
        /// `SELECT *`
        star: 3.0,
        /// A list of variables and `(expr AS ?v)`.
        explicit: 5.0,
        /// An aggregate query (`GROUP BY` and/or aggregates).
        aggregate: 2.0,
        /// Probability of `DISTINCT`.
        p_distinct: 0.2,
        /// Probability of `REDUCED` (if not `DISTINCT`).
        p_reduced: 0.05,
        /// Probability that a projected item is `(expr AS ?v)`.
        p_expression: 0.2,
        /// Probability that an aggregate query has no `GROUP BY`.
        p_implicit_group: 0.25,
        /// Probability that a group key is `(expr AS ?v)`.
        p_expression_key: 0.15,
        /// Probability that a group condition is an unnamed expression.
        p_unnamed_key: 0.1,
    }
}

weights! {
    /// Elements of group graph patterns.
    pub struct PatternWeights {
        /// A triples block.
        triples: 10.0,
        /// A nested group `{ ... }`.
        group: 1.0,
        /// `{ ... } UNION { ... }`
        union: 2.0,
        /// `OPTIONAL { ... }`
        optional: 3.0,
        /// `MINUS { ... }`
        minus: 1.0,
        /// `GRAPH ?g { ... }`
        graph: 0.5,
        /// `SERVICE <...> { ... }`; off by default since it contacts other endpoints.
        service: 0.0,
        /// `FILTER`
        filter: 4.0,
        /// `BIND`
        bind: 2.0,
        /// `VALUES`
        values: 1.0,
        /// `{ SELECT ... }`
        subselect: 1.0,
        /// Scale factor for [`Extension::pattern_weight`](crate::extension::Extension::pattern_weight).
        extension: 1.0,
        /// Probability of the `WHERE` keyword.
        p_where_keyword: 0.8,
        /// Probability of the optional `.` after a non-triples element.
        p_dot_after_pattern: 0.15,
        /// Probability of a trailing `.` after a triples block.
        p_trailing_dot: 0.2,
        /// Probability of a third, fourth, ... `UNION` branch.
        p_more_union: 0.3,
        /// Probability that `SERVICE` is `SILENT`.
        p_service_silent: 0.5,
    }
}

weights! {
    /// Triple patterns.
    pub struct TripleWeights {
        /// A variable or term as subject.
        subject_term: 10.0,
        /// `[ ... ]` as subject.
        subject_property_list: 0.7,
        /// `( ... )` as subject.
        subject_collection: 0.3,
        /// A variable or term as object.
        object_term: 10.0,
        /// `[ ... ]` as object.
        object_property_list: 0.7,
        /// `( ... )` as object.
        object_collection: 0.3,
        /// Probability of another `; verb objects` entry.
        p_more_verbs: 0.25,
        /// Probability of another `, object`.
        p_more_objects: 0.15,
        /// Probability of an empty `;` entry.
        p_empty_entry: 0.03,
        /// Probability that a verb is a complex property path.
        p_path: 0.15,
        /// Probability that a subject/object variable is a blank node label instead.
        p_blank_node_label: 0.05,
        /// Probability that `[]` or `()` stand alone without a property list.
        p_node_without_properties: 0.2,
    }
}

weights! {
    /// Property paths.
    pub struct PathWeights {
        /// An IRI as path primary.
        iri: 8.0,
        /// `a` as path primary.
        a: 0.5,
        /// `!(...)` as path primary.
        negated: 1.0,
        /// `( path )` as path primary.
        group: 1.0,
        /// Probability of another `|` alternative.
        p_alternative: 0.25,
        /// Probability of another `/` step.
        p_sequence: 0.35,
        /// Probability of `^`.
        p_inverse: 0.2,
        /// Probability of a modifier `?`, `*` or `+`.
        p_modifier: 0.3,
        /// Probability of another member of a negated property set.
        p_more_negated: 0.3,
        /// Probability of `^` in a negated property set member.
        p_negated_inverse: 0.2,
    }
}

weights! {
    /// Expressions.
    pub struct ExpressionWeights {
        /// A variable.
        variable: 6.0,
        /// A constant.
        constant: 5.0,
        /// `||`
        or: 1.0,
        /// `&&`
        and: 2.0,
        /// `!`
        not: 1.0,
        /// `=`, `!=`, `<`, ...
        compare: 5.0,
        /// `IN`
        r#in: 0.7,
        /// `NOT IN`
        not_in: 0.5,
        /// `+`, `-`, `*`, `/`
        arithmetic: 2.0,
        /// Unary `+` and `-`.
        unary: 0.4,
        /// The `?a -1` form of subtraction.
        signed_tail: 0.1,
        /// Explicit parentheses.
        bracketed: 0.3,
        /// A built-in function.
        builtin: 4.0,
        /// An XSD cast such as `xsd:integer(?x)`.
        cast: 0.8,
        /// A function of an extension.
        extension_function: 1.0,
        /// `EXISTS`
        exists: 0.6,
        /// `NOT EXISTS`
        not_exists: 0.6,
        /// `BOUND`
        bound: 0.8,
        /// Probability to ignore types when choosing an operand.
        p_ignore_types: 0.05,
        /// Probability that a comparison with a variable uses a data value of it.
        p_data_value: 0.7,
        /// Probability of an aggregate (where allowed) instead of another leaf.
        p_aggregate: 0.6,
        /// Probability of `DISTINCT` inside an aggregate.
        p_distinct_aggregate: 0.2,
        /// Probability of an explicit `SEPARATOR` in `GROUP_CONCAT`.
        p_separator: 0.5,
        /// Probability of referencing a variable of an enclosing group.
        p_outer_variable: 0.1,
        /// Probability of referencing a variable that is not in scope.
        p_unbound_variable: 0.02,
    }
}

weights! {
    /// Aggregate functions.
    pub struct AggregateWeights {
        /// `COUNT(*)`
        count_star: 3.0,
        /// `COUNT(expr)`
        count: 2.0,
        /// `SUM`
        sum: 1.0,
        /// `MIN`
        min: 1.0,
        /// `MAX`
        max: 1.0,
        /// `AVG`
        avg: 1.0,
        /// `SAMPLE`
        sample: 0.5,
        /// `GROUP_CONCAT`
        group_concat: 0.7,
    }
}

weights! {
    /// Lexical forms of terms.
    pub struct LexicalWeights {
        /// Probability to write numbers without quotes where possible.
        p_numeric_shorthand: 0.8,
        /// Probability to write `true`/`false` without quotes where possible.
        p_boolean_shorthand: 0.8,
        /// Probability to write `^^xsd:string` for simple literals.
        p_explicit_string_type: 0.1,
        /// `"..."`
        double_quote: 6.0,
        /// `'...'`
        single_quote: 2.0,
        /// `"""..."""`
        long_double_quote: 1.0,
        /// `'''...'''`
        long_single_quote: 1.0,
        /// Probability to write a variable with `$`.
        p_dollar: 0.05,
        /// Probability of an unusual but valid variable name.
        p_exotic_name: 0.03,
    }
}

weights! {
    /// Solution modifiers.
    pub struct ModifierWeights {
        /// Probability of `ORDER BY`.
        p_order_by: 0.3,
        /// Probability of `LIMIT`.
        p_limit: 0.3,
        /// Probability of `OFFSET`.
        p_offset: 0.15,
        /// Probability to write `OFFSET` before `LIMIT`.
        p_offset_first: 0.3,
        /// Probability of `HAVING` in aggregate queries.
        p_having: 0.3,
        /// Probability of another `ORDER BY` condition.
        p_more_order: 0.3,
        /// `ASC(expr)`
        asc: 1.0,
        /// `DESC(expr)`
        desc: 1.0,
        /// A plain variable.
        var: 2.0,
        /// A constraint (function call or bracketed expression).
        constraint: 0.5,
    }
}

weights! {
    /// Dataset clauses and `VALUES`.
    pub struct DataWeights {
        /// Probability of a `FROM` clause.
        p_from: 0.02,
        /// Probability of a `FROM NAMED` clause.
        p_from_named: 0.02,
        /// Probability of a trailing `VALUES` clause.
        p_trailing_values: 0.05,
        /// `VALUES ?x { ... }`
        one_var: 1.0,
        /// `VALUES (?x ?y) { ... }`
        full: 1.0,
        /// Probability of `UNDEF` in a cell.
        p_undef: 0.1,
    }
}

weights! {
    /// SPARQL Update operations.
    pub struct UpdateWeights {
        /// `INSERT DATA`
        insert_data: 3.0,
        /// `DELETE DATA`
        delete_data: 2.0,
        /// `DELETE WHERE`
        delete_where: 2.0,
        /// `DELETE/INSERT ... WHERE`
        modify: 4.0,
        /// `LOAD`; off by default since it fetches documents.
        load: 0.0,
        /// `CLEAR`
        clear: 0.3,
        /// `DROP`
        drop: 0.3,
        /// `CREATE`
        create: 0.3,
        /// `ADD`, `MOVE`, `COPY`
        transfer: 0.3,
        /// Probability of `SILENT`.
        p_silent: 0.3,
        /// Probability of `WITH`.
        p_with: 0.1,
        /// Probability of `USING`.
        p_using: 0.1,
        /// Probability of another operation in the request.
        p_more_operations: 0.3,
        /// Probability of a `GRAPH` block in quads.
        p_graph_quads: 0.2,
    }
}

/// All weights.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(missing_docs)] // see the documentation of the section types
pub struct Weights {
    pub form: FormWeights,
    pub prologue: PrologueWeights,
    pub select: SelectWeights,
    pub pattern: PatternWeights,
    pub triple: TripleWeights,
    pub path: PathWeights,
    pub expression: ExpressionWeights,
    pub aggregate: AggregateWeights,
    pub lexical: LexicalWeights,
    pub modifier: ModifierWeights,
    pub data: DataWeights,
    pub update: UpdateWeights,
}

impl Weights {
    /// Calls `f` with a qualified name (`section.field`) and every weight.
    pub fn for_each_mut(&mut self, f: &mut dyn FnMut(&str, &mut f64)) {
        macro_rules! visit {
            ($($section:ident),*) => {
                $( self.$section.for_each_mut(&mut |name, w| f(&format!("{}.{name}", stringify!($section)), w)); )*
            };
        }
        visit!(
            form, prologue, select, pattern, triple, path, expression, aggregate, lexical,
            modifier, data, update
        );
    }
}

/// Size limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Maximum nesting of group graph patterns (including sub-queries).
    pub max_depth: u32,
    /// Maximum nesting of expressions.
    pub max_expression_depth: u32,
    /// Maximum number of elements in a group.
    pub max_group_elements: u32,
    /// Maximum number of triples in a triples block.
    pub max_triples: u32,
    /// Maximum nesting of blank node property lists and collections.
    pub max_node_depth: u32,
    /// Maximum number of items in lists (collections, `IN`, `CONCAT`, ...).
    pub max_list: u32,
    /// Maximum number of projected items.
    pub max_projection: u32,
    /// Maximum number of `GROUP BY` keys.
    pub max_group_keys: u32,
    /// Maximum number of variables in a `VALUES` block.
    pub max_values_vars: u32,
    /// Maximum number of rows in a `VALUES` block.
    pub max_values_rows: u32,
    /// Maximum nesting of property paths.
    pub max_path_depth: u32,
    /// Maximum value of `LIMIT` and `OFFSET`.
    pub max_limit: u64,
    /// Maximum number of operations in an update request.
    pub max_update_operations: u32,
    /// Total number of grammar productions per query, after which only
    /// leaves are generated.
    pub budget: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_expression_depth: 3,
            max_group_elements: 5,
            max_triples: 4,
            max_node_depth: 2,
            max_list: 3,
            max_projection: 4,
            max_group_keys: 2,
            max_values_vars: 3,
            max_values_rows: 4,
            max_path_depth: 2,
            max_limit: 100,
            max_update_operations: 3,
            budget: 400,
        }
    }
}

/// Swarm testing: per query, disable each feature with some probability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwarmConfig {
    /// Whether swarm testing is enabled.
    pub enabled: bool,
    /// Probability of disabling each weight.
    pub p_disable: f64,
    /// Weights that are never disabled (qualified names like `pattern.triples`).
    pub keep: Vec<String>,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            p_disable: 0.3,
            keep: vec!["form.select".to_owned(), "pattern.triples".to_owned()],
        }
    }
}

impl SwarmConfig {
    /// Returns `weights` with a random subset set to zero, and the names of
    /// the disabled weights.
    pub fn apply(&self, weights: &Weights, rng: &mut impl rand::Rng) -> (Weights, Vec<String>) {
        let mut out = weights.clone();
        let mut disabled = Vec::new();
        if self.enabled {
            out.for_each_mut(&mut |name, w| {
                if *w > 0.0
                    && !self.keep.iter().any(|k| k == name)
                    && rng.random_bool(self.p_disable.clamp(0.0, 1.0))
                {
                    *w = 0.0;
                    disabled.push(name.to_owned());
                }
            });
        }
        (out, disabled)
    }
}

/// The complete generator configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneratorConfig {
    /// Size limits.
    pub limits: Limits,
    /// Weights and probabilities.
    pub weights: Weights,
    /// Relative weights of built-in functions by keyword (e.g. `"REGEX"`);
    /// unlisted functions have weight 1.
    pub functions: BTreeMap<String, f64>,
    /// Whether `RAND`, `NOW`, `UUID`, `STRUUID` and `BNODE` may be used.
    pub nondeterministic_functions: bool,
    /// Swarm testing.
    pub swarm: SwarmConfig,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            weights: Weights::default(),
            functions: BTreeMap::new(),
            nondeterministic_functions: true,
            swarm: SwarmConfig::default(),
        }
    }
}

impl GeneratorConfig {
    /// Parses a TOML configuration; missing fields take their defaults.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid TOML or unknown fields.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serializes the configuration as TOML.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails (it should not).
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact values are compared on purpose
mod tests {
    use rand::SeedableRng;

    use super::*;

    #[test]
    fn default_config_round_trips_through_toml() {
        let config = GeneratorConfig::default();
        let text = config.to_toml().unwrap();
        assert_eq!(GeneratorConfig::from_toml(&text).unwrap(), config);
        let partial = GeneratorConfig::from_toml("[weights.pattern]\noptional = 0.0\n").unwrap();
        assert_eq!(partial.weights.pattern.optional, 0.0);
        assert_eq!(
            partial.weights.pattern.union,
            PatternWeights::default().union
        );
        assert!(GeneratorConfig::from_toml("[weights.pattern]\noptinal = 1.0\n").is_err());
    }

    #[test]
    fn swarm_keeps_protected_weights() {
        let swarm = SwarmConfig {
            enabled: true,
            p_disable: 1.0,
            ..SwarmConfig::default()
        };
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(1);
        let (w, disabled) = swarm.apply(&Weights::default(), &mut rng);
        assert!(w.pattern.triples > 0.0 && w.form.select > 0.0);
        assert_eq!(w.pattern.optional, 0.0);
        assert!(disabled.contains(&"pattern.optional".to_owned()));
    }
}
