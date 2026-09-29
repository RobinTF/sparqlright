//! Query forms, `SELECT` clauses, solution modifiers and the prologue.

use std::collections::BTreeSet;

use rand::RngExt;

use super::context::{BareVars, ExprEnv, Gen, Scope, Value};
use super::pattern::GroupKind;
use crate::ast::{
    AskQuery, Constraint, ConstructQuery, DatasetClause, DescribeQuery, Expression, GroupCondition,
    IriRef, LimitOffset, OrderCondition, PrefixedName, Projection, ProjectionItem, Prologue,
    PrologueDecl, Query, QueryForm, SelectClause, SelectModifier, SelectQuery, SolutionModifier,
    SubSelect, VarOrIri, Variable, WhereClause, WriteSparql,
};
use crate::provider::{Namespace, Slot, TermKinds, ValueType};

/// A group key planned in the `SELECT` clause.
#[derive(Debug, Clone)]
enum GroupKey {
    /// `GROUP BY ?v`
    Var(Variable),
    /// `GROUP BY (expr AS ?v)`
    Named(Variable),
    /// `GROUP BY (expr)` or `GROUP BY f(...)`
    Unnamed,
}

/// What the `SELECT` clause committed to.
#[derive(Debug, Clone)]
struct SelectPlan {
    clause: SelectClause,
    aggregated: bool,
    keys: Vec<GroupKey>,
}

impl SelectPlan {
    fn key_vars(&self) -> Vec<Variable> {
        self.keys
            .iter()
            .filter_map(|k| match k {
                GroupKey::Var(v) | GroupKey::Named(v) => Some(v.clone()),
                GroupKey::Unnamed => None,
            })
            .collect()
    }

    fn as_vars(&self) -> Vec<Variable> {
        match &self.clause.projection {
            Projection::Star => Vec::new(),
            Projection::Items(items) => items
                .iter()
                .filter_map(|i| match i {
                    ProjectionItem::Expr(_, v) => Some(v.clone()),
                    ProjectionItem::Var(_) => None,
                })
                .collect(),
        }
    }
}

impl Gen<'_> {
    /// Generates a complete query.
    pub fn query(&mut self) -> Query {
        self.hit("Query");
        let prologue = self.prologue();
        let f = &self.w.form;
        let form = self
            .choose(&[
                (0, f.select),
                (1, f.construct),
                (2, f.construct_where),
                (3, f.describe),
                (4, f.ask),
            ])
            .unwrap_or(0);
        let form = match form {
            1 => QueryForm::Construct(self.construct_query(false)),
            2 => QueryForm::Construct(self.construct_query(true)),
            3 => QueryForm::Describe(self.describe_query()),
            4 => QueryForm::Ask(self.ask_query()),
            _ => QueryForm::Select(self.select_query()),
        };
        let values = self.values_clause();
        Query {
            prologue,
            form,
            values,
        }
    }

    fn values_clause(&mut self) -> Option<crate::ast::DataBlock> {
        if self.coin(self.w.data.p_trailing_values) {
            self.hit("ValuesClause");
            self.out.token("VALUES");
            Some(self.data_block())
        } else {
            None
        }
    }

    // -- prologue -------------------------------------------------------------

    /// Declares `BASE` and `PREFIX`es and writes them.
    pub fn prologue(&mut self) -> Prologue {
        let mut namespaces = Namespace::standard();
        match self.provider.namespaces() {
            Ok(ns) => namespaces.extend(ns),
            Err(_) => self.stats.provider_errors += 1,
        }
        for e in self.extensions {
            namespaces.extend(e.namespaces());
        }
        let mut seen_prefixes = BTreeSet::new();
        let mut seen_iris = BTreeSet::new();
        let mut decls = Vec::new();
        for ns in namespaces {
            let valid = PrefixedName::new(ns.prefix.clone(), "").is_ok()
                && IriRef::new(ns.iri.clone()).is_ok();
            if !valid || seen_prefixes.contains(&ns.prefix) || seen_iris.contains(&ns.iri) {
                continue;
            }
            if self.flip(self.w.prologue.p_prefix) {
                // Occasionally use the empty prefix for a namespace.
                let prefix = if !seen_prefixes.contains("") && self.flip(0.1) {
                    String::new()
                } else {
                    ns.prefix.clone()
                };
                if seen_prefixes.insert(prefix.clone()) {
                    seen_iris.insert(ns.iri.clone());
                    self.prefixes.push((prefix.clone(), ns.iri.clone()));
                    decls.push(PrologueDecl::Prefix {
                        prefix,
                        iri: IriRef::new(ns.iri).expect("validated above"),
                    });
                }
            }
        }
        if self.flip(self.w.prologue.p_base) {
            let candidates: Vec<String> = self
                .prefixes
                .iter()
                .map(|(_, iri)| iri.clone())
                .filter(|iri| iri.ends_with('/'))
                .collect();
            if let Some(base) = self.pick(&candidates).cloned() {
                let at = self.rng.random_range(0..=decls.len());
                decls.insert(
                    at,
                    PrologueDecl::Base(IriRef::new(base.clone()).expect("validated above")),
                );
                self.base = Some(base);
            }
        }
        if !decls.is_empty() {
            self.hit("Prologue");
        }
        let prologue = Prologue(decls);
        self.emit(&prologue);
        self.prologue_text = prologue.to_sparql();
        prologue
    }

    // -- SELECT -----------------------------------------------------------------

    fn select_query(&mut self) -> SelectQuery {
        self.hit("SelectQuery");
        self.push_pending();
        let plan = self.select_clause();
        let dataset = self.dataset_clauses();
        let (where_clause, scope) = self.where_clause();
        let modifiers = self.solution_modifiers(&plan, scope, false);
        self.pop_pending();
        SelectQuery {
            select: plan.clause,
            dataset,
            where_clause,
            modifiers,
        }
    }

    /// The body of `{ SELECT ... }`; the enclosing group is already open.
    pub fn sub_select(&mut self) -> SubSelect {
        self.hit("SubSelect");
        let saved_env = std::mem::replace(&mut self.env, ExprEnv::pattern());
        self.scope().barrier = true;
        self.push_pending();
        let plan = self.select_clause();
        let (where_clause, scope) = self.where_clause();
        let exported = Self::exported(&plan, &scope);
        let modifiers = self.solution_modifiers(&plan, scope, true);
        let values = self.values_clause();
        self.pop_pending();
        for (v, certain) in exported {
            self.bind(&v, certain);
        }
        self.env = saved_env;
        SubSelect {
            select: plan.clause,
            where_clause,
            modifiers,
            values,
        }
    }

    /// The variables a sub-query makes visible, with their certainty.
    fn exported(plan: &SelectPlan, scope: &Scope) -> Vec<(Variable, bool)> {
        match &plan.clause.projection {
            Projection::Star => scope
                .possible
                .iter()
                .map(|v| (v.clone(), scope.certain.contains(v)))
                .collect(),
            Projection::Items(items) => items
                .iter()
                .map(|i| match i {
                    ProjectionItem::Var(v) => {
                        (v.clone(), !plan.aggregated && scope.certain.contains(v))
                    }
                    ProjectionItem::Expr(_, v) => (v.clone(), false),
                })
                .collect(),
        }
    }

    fn select_clause(&mut self) -> SelectPlan {
        self.hit("SelectClause");
        self.out.token("SELECT");
        let s = &self.w.select;
        let (p_distinct, p_reduced) = (s.p_distinct, s.p_reduced);
        let modifier = if self.flip(p_distinct) {
            self.out.token("DISTINCT");
            Some(SelectModifier::Distinct)
        } else if self.flip(p_reduced) {
            self.out.token("REDUCED");
            Some(SelectModifier::Reduced)
        } else {
            None
        };
        let s = &self.w.select;
        let mode = self
            .choose(&[
                (0, s.star),
                (1, s.explicit),
                (2, if self.has_budget() { s.aggregate } else { 0.0 }),
            ])
            .unwrap_or(0);
        let saved_env = self.env.clone();
        let plan = match mode {
            1 => self.explicit_projection(modifier),
            2 => self.aggregate_projection(modifier),
            _ => {
                self.out.token("*");
                SelectPlan {
                    clause: SelectClause {
                        modifier,
                        projection: Projection::Star,
                    },
                    aggregated: false,
                    keys: Vec::new(),
                }
            }
        };
        self.env = saved_env;
        plan
    }

    fn explicit_projection(&mut self, modifier: Option<SelectModifier>) -> SelectPlan {
        self.env = ExprEnv {
            bare: BareVars::Scope,
            aggregates: false,
            aggregate_vars: BareVars::Scope,
            planning: true,
            exists: false,
        };
        let n = self.count(0.5, self.limits.max_projection);
        let mut items = Vec::new();
        for _ in 0..n {
            if self.coin(self.w.select.p_expression) {
                items.push(self.projection_expression(false));
            } else {
                let v = self.take_fresh();
                self.emit(&v);
                items.push(ProjectionItem::Var(v));
            }
        }
        SelectPlan {
            clause: SelectClause {
                modifier,
                projection: Projection::Items(items),
            },
            aggregated: false,
            keys: Vec::new(),
        }
    }

    /// `(expr AS ?v)`; with `aggregate`, the expression is usually an aggregate.
    fn projection_expression(&mut self, aggregate: bool) -> ProjectionItem {
        self.hit("ProjectionExpression");
        self.out.open("(");
        let e = if aggregate && self.flip(0.7) {
            self.aggregate_expr(ValueType::Any).0
        } else {
            self.expr(ValueType::Any, 1)
        };
        self.out.token("AS");
        let v = self.reserve_fresh();
        self.emit(&v);
        self.out.glue(")");
        ProjectionItem::Expr(e, v)
    }

    fn aggregate_projection(&mut self, modifier: Option<SelectModifier>) -> SelectPlan {
        self.hit("AggregateQuery");
        // Plan the group keys.
        self.env.planning = true;
        let n_keys = if self.flip(self.w.select.p_implicit_group) {
            0
        } else {
            self.count(0.5, self.limits.max_group_keys)
        };
        let mut keys = Vec::new();
        for _ in 0..n_keys {
            let key = if self.flip(self.w.select.p_unnamed_key) {
                GroupKey::Unnamed
            } else if self.flip(self.w.select.p_expression_key) {
                GroupKey::Named(self.reserve_fresh())
            } else {
                GroupKey::Var(self.take_fresh())
            };
            keys.push(key);
        }
        let plan_keys = SelectPlan {
            clause: SelectClause {
                modifier,
                projection: Projection::Star,
            },
            aggregated: true,
            keys,
        };
        let key_vars = plan_keys.key_vars();
        self.env = ExprEnv {
            bare: BareVars::Only(key_vars.clone()),
            aggregates: true,
            aggregate_vars: BareVars::Scope,
            planning: true,
            exists: false,
        };
        let n = self.count(0.5, self.limits.max_projection);
        let mut items = Vec::new();
        let mut unprojected: Vec<Variable> = key_vars;
        for i in 0..n {
            let project_key =
                !unprojected.is_empty() && (i > 0 || !plan_keys.keys.is_empty()) && self.flip(0.4);
            if project_key {
                let k = unprojected.remove(self.rng.random_range(0..unprojected.len()));
                let k = self.with_sigil(k);
                self.emit(&k);
                items.push(ProjectionItem::Var(k));
            } else {
                items.push(self.projection_expression(true));
            }
        }
        SelectPlan {
            clause: SelectClause {
                modifier,
                projection: Projection::Items(items),
            },
            ..plan_keys
        }
    }

    fn dataset_clauses(&mut self) -> Vec<DatasetClause> {
        let mut clauses = Vec::new();
        while clauses.len() < 2 {
            let named = if self.coin(self.w.data.p_from) {
                false
            } else if self.coin(self.w.data.p_from_named) {
                true
            } else {
                break;
            };
            self.hit("DatasetClause");
            self.out.token("FROM");
            if named {
                self.out.token("NAMED");
            }
            let value = self.value(Slot::GraphName, TermKinds::IRI, ValueType::Iri, None, None);
            let iri = match value {
                Value::Term(t) => self.iri_of(&t),
                Value::Var(_) => self.iri_of(&oxrdf::Term::NamedNode(
                    oxrdf::NamedNode::new_unchecked("http://example.org/sparqlright/graph"),
                )),
            };
            self.emit(&iri);
            clauses.push(if named {
                DatasetClause::Named(iri)
            } else {
                DatasetClause::Default(iri)
            });
        }
        clauses
    }

    fn where_clause(&mut self) -> (WhereClause, Scope) {
        let keyword = self.flip(self.w.pattern.p_where_keyword);
        if keyword {
            self.out.token("WHERE");
        }
        let saved = std::mem::replace(&mut self.env, ExprEnv::pattern());
        let (pattern, scope) = self.group(GroupKind::Plain);
        self.env = saved;
        (WhereClause { keyword, pattern }, scope)
    }

    // -- solution modifiers ----------------------------------------------------

    fn solution_modifiers(
        &mut self,
        plan: &SelectPlan,
        mut scope: Scope,
        sub_query: bool,
    ) -> SolutionModifier {
        scope.barrier = sub_query;
        self.scopes.push(scope);
        let saved_env = self.env.clone();
        let mut modifiers = SolutionModifier::default();
        let keys = plan.key_vars();
        if plan.aggregated && !plan.keys.is_empty() {
            self.hit("GroupClause");
            self.out.token("GROUP BY");
            self.env = ExprEnv {
                exists: false,
                ..ExprEnv::pattern()
            };
            for key in &plan.keys {
                modifiers.group_by.push(self.group_condition(key));
            }
        }
        let aggregate_env = ExprEnv {
            bare: BareVars::Only(keys),
            aggregates: true,
            aggregate_vars: BareVars::Scope,
            planning: false,
            exists: false,
        };
        if plan.aggregated && self.coin(self.w.modifier.p_having) {
            self.hit("HavingClause");
            self.out.token("HAVING");
            self.env = aggregate_env.clone();
            let n = self.count(0.3, 2);
            for _ in 0..n {
                modifiers.having.push(self.constraint());
            }
        }
        if self.coin(self.w.modifier.p_order_by) {
            self.hit("OrderClause");
            self.out.token("ORDER BY");
            self.env = if plan.aggregated {
                aggregate_env
            } else {
                let mut visible: Vec<Variable> = self.group_variables();
                visible.extend(plan.as_vars());
                ExprEnv {
                    bare: BareVars::Only(visible),
                    exists: false,
                    ..ExprEnv::pattern()
                }
            };
            let n = self.count(self.w.modifier.p_more_order, 3);
            for _ in 0..n {
                modifiers.order_by.push(self.order_condition());
            }
        }
        modifiers.limit_offset = self.limit_offset();
        self.env = saved_env;
        self.scopes.pop();
        modifiers
    }

    fn group_condition(&mut self, key: &GroupKey) -> GroupCondition {
        match key {
            GroupKey::Var(v) => {
                self.emit(v);
                GroupCondition::Var(v.clone())
            }
            GroupKey::Named(v) => {
                self.out.open("(");
                let e = self.expr(ValueType::Any, 1);
                self.out.token("AS");
                self.emit(v);
                self.out.glue(")");
                GroupCondition::Expr(e, Some(v.clone()))
            }
            GroupKey::Unnamed => {
                if self.flip(0.5) {
                    match self.call_expr(ValueType::Any) {
                        Expression::BuiltIn(b) => GroupCondition::BuiltIn(b),
                        Expression::Function(f) => GroupCondition::Function(f),
                        other => unreachable!("call_expr returned {other:?}"),
                    }
                } else {
                    self.out.open("(");
                    let e = self.expr(ValueType::Any, 1);
                    self.out.glue(")");
                    GroupCondition::Expr(e, None)
                }
            }
        }
    }

    fn order_condition(&mut self) -> OrderCondition {
        let m = &self.w.modifier;
        let kind = self
            .choose(&[(0, m.asc), (1, m.desc), (2, m.var), (3, m.constraint)])
            .unwrap_or(2);
        match kind {
            2 => {
                let vars = self.expression_variables();
                if let Some(v) = self.pick(&vars).cloned() {
                    let v = self.with_sigil(v);
                    self.emit(&v);
                    return OrderCondition::Var(v);
                }
                self.order_asc_desc(true)
            }
            3 => {
                if self.flip(0.5) {
                    match self.call_expr(ValueType::Any) {
                        Expression::BuiltIn(b) => {
                            OrderCondition::Constraint(Constraint::BuiltIn(b))
                        }
                        Expression::Function(f) => {
                            OrderCondition::Constraint(Constraint::Function(f))
                        }
                        other => unreachable!("call_expr returned {other:?}"),
                    }
                } else {
                    self.out.open("(");
                    let e = self.expr(ValueType::Any, 1);
                    self.out.glue(")");
                    OrderCondition::Constraint(Constraint::Bracketed(e))
                }
            }
            k => self.order_asc_desc(k == 0),
        }
    }

    fn order_asc_desc(&mut self, asc: bool) -> OrderCondition {
        self.out.token(if asc { "ASC" } else { "DESC" });
        self.out.glue_open("(");
        let e = self.expr(ValueType::Any, 1);
        self.out.glue(")");
        if asc {
            OrderCondition::Asc(e)
        } else {
            OrderCondition::Desc(e)
        }
    }

    fn limit_offset(&mut self) -> Option<LimitOffset> {
        let m = &self.w.modifier;
        let (p_limit, p_offset, p_offset_first) = (m.p_limit, m.p_offset, m.p_offset_first);
        let limit = self
            .flip(p_limit)
            .then(|| self.rng.random_range(0..=self.limits.max_limit));
        let offset = self
            .flip(p_offset)
            .then(|| self.rng.random_range(0..=self.limits.max_limit));
        let lo = match (limit, offset) {
            (None, None) => return None,
            (Some(l), None) => LimitOffset::LimitFirst {
                limit: l,
                offset: None,
            },
            (None, Some(o)) => LimitOffset::OffsetFirst {
                offset: o,
                limit: None,
            },
            (Some(l), Some(o)) => {
                if self.flip(p_offset_first) {
                    LimitOffset::OffsetFirst {
                        offset: o,
                        limit: Some(l),
                    }
                } else {
                    LimitOffset::LimitFirst {
                        limit: l,
                        offset: Some(o),
                    }
                }
            }
        };
        self.hit("LimitOffsetClauses");
        self.emit(&lo);
        Some(lo)
    }

    /// Modifiers of non-`SELECT` forms: `ORDER BY`, `LIMIT` and `OFFSET`.
    fn plain_modifiers(&mut self, scope: Scope) -> SolutionModifier {
        let plan = SelectPlan {
            clause: SelectClause {
                modifier: None,
                projection: Projection::Star,
            },
            aggregated: false,
            keys: Vec::new(),
        };
        self.solution_modifiers(&plan, scope, false)
    }

    // -- other forms --------------------------------------------------------------

    fn construct_query(&mut self, short: bool) -> ConstructQuery {
        self.hit(if short {
            "ConstructWhere"
        } else {
            "ConstructQuery"
        });
        self.out.token("CONSTRUCT");
        if short {
            let dataset = self.dataset_clauses();
            self.out.token("WHERE");
            self.out.token("{");
            self.scopes.push(Scope::default());
            let triples = self.triples_template(self.limits.max_triples);
            let scope = self.scopes.pop().unwrap_or_default();
            self.out.token("}");
            let modifiers = self.plain_modifiers(scope);
            return ConstructQuery::Short {
                dataset,
                triples,
                modifiers,
            };
        }
        self.push_pending();
        let saved = self.env.clone();
        self.env.planning = true;
        let mark = self.context.len();
        self.out.token("{");
        let template = self.triples_template(self.limits.max_triples);
        self.out.token("}");
        self.truncate_context(mark);
        self.env = saved;
        let dataset = self.dataset_clauses();
        let (where_clause, scope) = self.where_clause();
        let modifiers = self.plain_modifiers(scope);
        self.pop_pending();
        ConstructQuery::Template {
            template,
            dataset,
            where_clause,
            modifiers,
        }
    }

    fn describe_query(&mut self) -> DescribeQuery {
        self.hit("DescribeQuery");
        self.out.token("DESCRIBE");
        self.push_pending();
        let mut targets = Vec::new();
        let star = self.flip(0.2);
        if star {
            self.out.token("*");
        } else {
            let saved = self.env.clone();
            self.env.planning = true;
            let n = self.count(0.4, 3);
            for _ in 0..n {
                let target = match self.value(
                    Slot::DescribeTarget,
                    TermKinds::VARIABLE | TermKinds::IRI,
                    ValueType::Iri,
                    None,
                    None,
                ) {
                    // A variable may be described only once.
                    Value::Var(v) if targets.contains(&VarOrIri::Var(v.clone())) => continue,
                    Value::Var(v) => VarOrIri::Var(v),
                    Value::Term(t) => VarOrIri::Iri(self.iri_of(&t)),
                };
                self.emit(&target);
                targets.push(target);
            }
            self.env = saved;
        }
        let dataset = self.dataset_clauses();
        let (where_clause, modifiers) = if star || self.flip(0.85) {
            let (w, scope) = self.where_clause();
            (Some(w), self.plain_modifiers(scope))
        } else {
            (None, self.plain_modifiers(Scope::default()))
        };
        self.pop_pending();
        DescribeQuery {
            targets,
            dataset,
            where_clause,
            modifiers,
        }
    }

    fn ask_query(&mut self) -> AskQuery {
        self.hit("AskQuery");
        self.out.token("ASK");
        let dataset = self.dataset_clauses();
        let (where_clause, scope) = self.where_clause();
        let modifiers = self.plain_modifiers(scope);
        AskQuery {
            dataset,
            where_clause,
            modifiers,
        }
    }
}
