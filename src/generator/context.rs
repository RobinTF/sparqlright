//! Generation state shared by all productions.

use std::collections::{BTreeSet, HashMap, HashSet};

use rand::RngExt;
use rand_chacha::ChaCha8Rng;

use super::GenerationStats;
use crate::ast::{TokenWriter, VarSigil, Variable, WriteSparql};
use crate::config::{GeneratorConfig, Limits, Weights};
use crate::extension::{Extension, FunctionSpec};
use crate::provider::{
    Candidate, CompletionProvider, CompletionRequest, ContextTriple, PartialTriple, Slot,
    Suggestion, TermKinds, ValueType,
};

/// Variables bound by a group graph pattern.
#[derive(Debug, Clone, Default)]
pub(super) struct Scope {
    /// Bound in every solution.
    pub certain: BTreeSet<Variable>,
    /// Bound in some solutions (a superset of `certain`).
    pub possible: BTreeSet<Variable>,
    /// Whether enclosing scopes are invisible (the root of a sub-query).
    pub barrier: bool,
}

impl Scope {
    pub fn bind(&mut self, v: &Variable, certain: bool) {
        if certain {
            self.certain.insert(v.clone());
        }
        self.possible.insert(v.clone());
    }

    /// Adds the variables of a pattern joined with this one.
    pub fn join(&mut self, other: &Scope) {
        self.certain.extend(other.certain.iter().cloned());
        self.possible.extend(other.possible.iter().cloned());
    }

    /// Adds the variables of an optional pattern.
    pub fn optional(&mut self, other: &Scope) {
        self.possible.extend(other.possible.iter().cloned());
    }
}

/// Where the variables of an expression may come from.
#[derive(Debug, Clone)]
pub(super) enum BareVars {
    /// The variables in scope in the current group.
    Scope,
    /// Only these variables (e.g. group keys in an aggregate projection).
    Only(Vec<Variable>),
}

/// The expression environment.
#[derive(Debug, Clone)]
pub(super) struct ExprEnv {
    /// Variables usable outside aggregates.
    pub bare: BareVars,
    /// Whether aggregates are allowed.
    pub aggregates: bool,
    /// Variables usable inside aggregates.
    pub aggregate_vars: BareVars,
    /// Whether new planned variables may be introduced (clauses written
    /// before the `WHERE` clause).
    pub planning: bool,
    /// Whether `EXISTS` is allowed.
    pub exists: bool,
}

impl ExprEnv {
    pub fn pattern() -> Self {
        Self {
            bare: BareVars::Scope,
            aggregates: false,
            aggregate_vars: BareVars::Scope,
            planning: false,
            exists: true,
        }
    }
}

/// Variable allocation.
#[derive(Debug, Default)]
pub(super) struct Vars {
    next: usize,
    /// Mentioned anywhere so far.
    pub used: HashSet<Variable>,
    /// Targets of `AS`, never handed out otherwise.
    pub reserved: HashSet<Variable>,
    /// Planned in a clause before the pattern.
    pub planned: HashSet<Variable>,
    /// Per `SELECT` level: planned variables not yet used in a pattern.
    pub pending: Vec<Vec<Variable>>,
    /// Known value types.
    pub types: HashMap<Variable, ValueType>,
}

/// The value chosen for a position.
#[derive(Debug, Clone)]
pub(super) enum Value {
    Var(Variable),
    Term(oxrdf::Term),
}

/// The generator state.
pub(crate) struct Gen<'a> {
    pub rng: ChaCha8Rng,
    pub w: Weights,
    pub limits: &'a Limits,
    pub config: &'a GeneratorConfig,
    pub provider: &'a mut dyn CompletionProvider,
    pub extensions: &'a [Box<dyn Extension>],
    pub functions: Vec<FunctionSpec>,
    pub out: TokenWriter,
    pub prologue_text: String,
    /// Declared prefixes: (prefix, namespace).
    pub prefixes: Vec<(String, String)>,
    pub base: Option<String>,
    pub budget: u32,
    pub depth: u32,
    pub expr_depth: u32,
    pub node_depth: u32,
    pub vars: Vars,
    pub scopes: Vec<Scope>,
    pub context: Vec<ContextTriple>,
    pub env: ExprEnv,
    /// Blank node labels usable in the current basic graph pattern.
    pub block_bnodes: Vec<crate::ast::BlankNodeLabel>,
    pub bnode_counter: usize,
    pub anon_counter: usize,
    /// Whether blank nodes may be generated (not in `DELETE` templates).
    pub allow_bnodes: bool,
    /// Whether variables may be generated (not in `INSERT DATA`).
    pub allow_vars: bool,
    /// The fresh variable offered in the current request, if any.
    pub peeked: Option<Variable>,
    pub stats: GenerationStats,
}

impl<'a> Gen<'a> {
    pub fn new(
        config: &'a GeneratorConfig,
        extensions: &'a [Box<dyn Extension>],
        provider: &'a mut dyn CompletionProvider,
        mut rng: ChaCha8Rng,
    ) -> Self {
        let (w, disabled) = config.swarm.apply(&config.weights, &mut rng);
        let functions = extensions.iter().flat_map(|e| e.functions()).collect();
        Self {
            rng,
            w,
            limits: &config.limits,
            config,
            provider,
            extensions,
            functions,
            out: TokenWriter::new(),
            prologue_text: String::new(),
            prefixes: Vec::new(),
            base: None,
            budget: config.limits.budget,
            depth: 0,
            expr_depth: 0,
            node_depth: 0,
            vars: Vars::default(),
            scopes: vec![Scope::default()],
            context: Vec::new(),
            env: ExprEnv::pattern(),
            block_bnodes: Vec::new(),
            bnode_counter: 0,
            anon_counter: 0,
            allow_bnodes: true,
            allow_vars: true,
            peeked: None,
            stats: GenerationStats {
                disabled_features: disabled,
                ..GenerationStats::default()
            },
        }
    }

    pub fn finish(self) -> (String, GenerationStats) {
        (self.out.into_string(), self.stats)
    }

    // -- bookkeeping --------------------------------------------------------

    /// Records a production and consumes budget.
    pub fn hit(&mut self, production: &'static str) {
        *self.stats.productions.entry(production).or_insert(0) += 1;
        self.budget = self.budget.saturating_sub(1);
    }

    /// Whether there is budget left for optional or recursive constructs.
    pub fn has_budget(&self) -> bool {
        self.budget > 0
    }

    /// Whether nested group patterns may still be generated.
    pub fn can_nest(&self) -> bool {
        self.has_budget() && self.depth < self.limits.max_depth
    }

    pub fn coin(&mut self, p: f64) -> bool {
        p > 0.0 && self.has_budget() && self.rng.random_bool(p.min(1.0))
    }

    /// Like [`Gen::coin`] but independent of the budget.
    pub fn flip(&mut self, p: f64) -> bool {
        p > 0.0 && self.rng.random_bool(p.min(1.0))
    }

    /// Picks an option by weight; `None` if all weights are zero.
    pub fn choose<T: Copy>(&mut self, options: &[(T, f64)]) -> Option<T> {
        let total: f64 = options.iter().map(|(_, w)| w.max(0.0)).sum();
        if total <= 0.0 {
            return None;
        }
        let mut x = self.rng.random_range(0.0..total);
        for (option, weight) in options {
            let weight = weight.max(0.0);
            if x < weight {
                return Some(*option);
            }
            x -= weight;
        }
        options
            .iter()
            .rev()
            .find(|(_, w)| *w > 0.0)
            .map(|(o, _)| *o)
    }

    /// A uniformly random element.
    pub fn pick<'s, T>(&mut self, items: &'s [T]) -> Option<&'s T> {
        if items.is_empty() {
            None
        } else {
            Some(&items[self.rng.random_range(0..items.len())])
        }
    }

    /// A count in `1..=max` with geometrically decreasing probability.
    pub fn count(&mut self, p_more: f64, max: u32) -> u32 {
        let mut n = 1;
        while n < max && self.coin(p_more) {
            n += 1;
        }
        n
    }

    /// Writes a node to the output.
    pub fn emit<N: WriteSparql + ?Sized>(&mut self, node: &N) {
        node.write_sparql(&mut self.out);
    }

    // -- variables ----------------------------------------------------------

    fn new_variable_name(&mut self) -> String {
        let n = self.vars.next;
        self.vars.next += 1;
        if self.flip(self.w.lexical.p_exotic_name) {
            // Unusual but valid VARNAMEs: leading digit or underscore,
            // non-ASCII letters, middle dot and combining characters.
            let forms = [
                format!("{n}v"),
                format!("_{n}"),
                format!("v\u{e9}{n}"),
                format!("v\u{b7}{n}"),
                format!("\u{3b1}{n}"),
                format!("v\u{301}{n}"),
            ];
            let i = self.rng.random_range(0..forms.len());
            forms[i].clone()
        } else {
            format!("v{n}")
        }
    }

    /// The variable that [`Gen::take_fresh`] would return next.
    pub fn peek_fresh(&mut self) -> Variable {
        if let Some(v) = self.pending_unused() {
            return v;
        }
        if let Some(v) = &self.peeked {
            return v.clone();
        }
        loop {
            let v = Variable::new_unchecked(self.new_variable_name());
            if !self.vars.used.contains(&v)
                && !self.vars.reserved.contains(&v)
                && !self.vars.planned.contains(&v)
            {
                self.peeked = Some(v.clone());
                return v;
            }
        }
    }

    /// Returns a variable that has not been used so far and marks it used.
    ///
    /// Outside planning mode, planned variables of the current `SELECT`
    /// level are handed out first, so that the pattern binds them. In
    /// planning mode, the new variable becomes a planned one.
    pub fn take_fresh(&mut self) -> Variable {
        let v = self.peek_fresh();
        if self.env.planning
            && let Some(pending) = self.vars.pending.last_mut()
        {
            pending.push(v.clone());
        }
        self.use_var(&v);
        self.with_sigil(v)
    }

    /// Records that `v` occurs in the query.
    pub fn use_var(&mut self, v: &Variable) {
        if self.peeked.as_ref() == Some(v) {
            self.peeked = None;
        }
        if self.env.planning {
            // Planned variables are only "used" once a pattern binds them,
            // but they must not be handed out as fresh again.
            self.vars.planned.insert(v.clone());
        } else {
            self.vars.used.insert(v.clone());
        }
    }

    /// Reserves a fresh variable as the target of `AS`.
    pub fn reserve_fresh(&mut self) -> Variable {
        let saved = self.env.planning;
        self.env.planning = false;
        let v = loop {
            let v = Variable::new_unchecked(self.new_variable_name());
            if !self.vars.used.contains(&v)
                && !self.vars.reserved.contains(&v)
                && !self.vars.planned.contains(&v)
            {
                break v;
            }
        };
        self.env.planning = saved;
        self.vars.reserved.insert(v.clone());
        self.vars.used.insert(v.clone());
        self.with_sigil(v)
    }

    /// Randomly writes the variable with `$` instead of `?`.
    pub fn with_sigil(&mut self, v: Variable) -> Variable {
        if self.flip(self.w.lexical.p_dollar) {
            v.with_other_sigil(VarSigil::Dollar)
        } else {
            v
        }
    }

    fn pending_unused(&self) -> Option<Variable> {
        if self.env.planning {
            return None;
        }
        self.vars
            .pending
            .last()
            .and_then(|p| p.iter().find(|v| !self.vars.used.contains(*v)).cloned())
    }

    /// Starts a `SELECT` level with its own planned variables.
    pub fn push_pending(&mut self) {
        self.vars.pending.push(Vec::new());
    }

    pub fn pop_pending(&mut self) {
        self.vars.pending.pop();
    }

    /// The planned variables of the current `SELECT` level.
    pub fn pending(&self) -> Vec<Variable> {
        self.vars.pending.last().cloned().unwrap_or_default()
    }

    // -- scopes -------------------------------------------------------------

    pub fn scope(&mut self) -> &mut Scope {
        self.scopes
            .last_mut()
            .expect("the scope stack is never empty")
    }

    /// Variables bound so far in the current group.
    pub fn group_variables(&self) -> Vec<Variable> {
        self.scopes
            .last()
            .map(|s| s.possible.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Variables of enclosing groups, up to the enclosing sub-query.
    pub fn outer_variables(&self) -> Vec<Variable> {
        let mut out: BTreeSet<Variable> = BTreeSet::new();
        let n = self.scopes.len();
        for i in (0..n.saturating_sub(1)).rev() {
            if self.scopes[i + 1].barrier {
                break;
            }
            out.extend(self.scopes[i].possible.iter().cloned());
        }
        let own = &self.scopes[n - 1].possible;
        out.into_iter().filter(|v| !own.contains(v)).collect()
    }

    /// Variables an expression may reference outside aggregates.
    pub fn expression_variables(&self) -> Vec<Variable> {
        match &self.env.bare {
            BareVars::Only(v) => v.clone(),
            BareVars::Scope if self.env.planning => self.pending(),
            BareVars::Scope => self.group_variables(),
        }
    }

    /// Records that `v` is bound by the current group.
    pub fn bind(&mut self, v: &Variable, certain: bool) {
        self.use_var(v);
        if !self.env.planning {
            self.scope().bind(v, certain);
        }
    }

    // -- values -------------------------------------------------------------

    /// Asks the provider for a value and picks one of the valid suggestions.
    #[allow(clippy::too_many_arguments)]
    pub fn value(
        &mut self,
        slot: Slot,
        mut accepts: TermKinds,
        expected: ValueType,
        related: Option<&Variable>,
        partial: Option<&PartialTriple>,
    ) -> Value {
        if !self.allow_vars {
            accepts.remove(TermKinds::VARIABLE);
        }
        // Blank nodes from a provider cannot be referenced in a query.
        accepts.remove(TermKinds::BLANK_NODE);
        let fresh = self.peek_fresh();
        let (variables, outer) = if self.env.planning {
            (self.pending(), Vec::new())
        } else {
            (self.expression_variables(), self.outer_variables())
        };
        self.stats.provider_calls += 1;
        let request = CompletionRequest {
            partial_query: self.out.as_str(),
            prologue: &self.prologue_text,
            slot,
            accepts,
            expected,
            related_variable: related,
            partial_triple: partial,
            context: &self.context,
            variables: &variables,
            outer_variables: &outer,
            fresh_variable: &fresh,
        };
        let suggestions = if let Ok(s) = self.provider.suggest(&request) {
            s
        } else {
            self.stats.provider_errors += 1;
            Vec::new()
        };
        let valid: Vec<(usize, f64)> = suggestions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.weight > 0.0 && self.is_usable(s, accepts))
            .map(|(i, s)| (i, s.weight))
            .collect();
        if let Some(i) = self.choose(&valid) {
            match &suggestions[i].candidate {
                Candidate::Variable(v) => {
                    if *v == fresh {
                        return Value::Var(self.take_fresh());
                    }
                    if self.env.planning
                        && let Some(pending) = self.vars.pending.last_mut()
                        && !pending.contains(v)
                    {
                        pending.push(v.clone());
                    }
                    self.use_var(v);
                    Value::Var(self.with_sigil(v.clone()))
                }
                Candidate::Term(t) => Value::Term(t.clone()),
            }
        } else {
            self.stats.fallbacks += 1;
            self.fallback_value(accepts, expected)
        }
    }

    fn is_usable(&self, s: &Suggestion, accepts: TermKinds) -> bool {
        if !accepts.contains(s.candidate.kind()) {
            return false;
        }
        match &s.candidate {
            Candidate::Variable(v) => {
                !self.vars.reserved.contains(v) || self.expression_variables().contains(v)
            }
            Candidate::Term(t) => super::terms::is_representable(t),
        }
    }

    fn fallback_value(&mut self, accepts: TermKinds, expected: ValueType) -> Value {
        if accepts.contains(TermKinds::VARIABLE) {
            return Value::Var(self.take_fresh());
        }
        let term = super::terms::fallback_term(&mut self.rng, accepts, expected);
        Value::Term(term)
    }
}
