//! Queries, the prologue and solution modifiers (productions 1–28).

use super::expr::{BuiltInCall, Expression, FunctionCall};
use super::pattern::{Constraint, DataBlock, GroupGraphPattern, TriplesTemplate};
use super::term::{Iri, IriRef, VarOrIri, Variable};
use super::writer::{TokenWriter, WriteSparql, display_via_sparql};

/// A `BASE` or `PREFIX` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PrologueDecl {
    /// `BASE <iri>`; the IRI must be absolute.
    Base(IriRef),
    /// `PREFIX p: <iri>`; a prefix must not be declared twice.
    Prefix {
        /// The prefix without colon; may be empty.
        prefix: String,
        /// The namespace IRI.
        iri: IriRef,
    },
}

/// `Prologue` (production 4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Prologue(pub Vec<PrologueDecl>);

impl WriteSparql for Prologue {
    fn write_sparql(&self, w: &mut TokenWriter) {
        for decl in &self.0 {
            match decl {
                PrologueDecl::Base(iri) => {
                    w.token("BASE");
                    iri.write_sparql(w);
                }
                PrologueDecl::Prefix { prefix, iri } => {
                    w.token("PREFIX");
                    w.token(&format!("{prefix}:"));
                    iri.write_sparql(w);
                }
            }
        }
    }
}

/// A complete query (`Query`, production 2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    /// The prologue.
    pub prologue: Prologue,
    /// The query form.
    pub form: QueryForm,
    /// The trailing `VALUES` clause.
    pub values: Option<DataBlock>,
}

impl WriteSparql for Query {
    fn write_sparql(&self, w: &mut TokenWriter) {
        self.prologue.write_sparql(w);
        self.form.write_sparql(w);
        write_values_clause(w, self.values.as_ref());
    }
}

fn write_values_clause(w: &mut TokenWriter, values: Option<&DataBlock>) {
    if let Some(v) = values {
        w.token("VALUES");
        v.write_sparql(w);
    }
}

/// The four query forms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueryForm {
    /// `SELECT`
    Select(SelectQuery),
    /// `CONSTRUCT`
    Construct(ConstructQuery),
    /// `DESCRIBE`
    Describe(DescribeQuery),
    /// `ASK`
    Ask(AskQuery),
}

impl WriteSparql for QueryForm {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Select(q) => q.write_sparql(w),
            Self::Construct(q) => q.write_sparql(w),
            Self::Describe(q) => q.write_sparql(w),
            Self::Ask(q) => q.write_sparql(w),
        }
    }
}

/// `DISTINCT` or `REDUCED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectModifier {
    /// `DISTINCT`
    Distinct,
    /// `REDUCED`
    Reduced,
}

/// One entry of an explicit projection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectionItem {
    /// `?v`
    Var(Variable),
    /// `(expr AS ?v)`
    Expr(Expression, Variable),
}

/// The projection of a `SELECT` clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Projection {
    /// `*`
    Star,
    /// A non-empty list of variables and expressions.
    Items(Vec<ProjectionItem>),
}

/// `SelectClause` (production 9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectClause {
    /// `DISTINCT` or `REDUCED`, if given.
    pub modifier: Option<SelectModifier>,
    /// The projection.
    pub projection: Projection,
}

impl WriteSparql for SelectClause {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token("SELECT");
        match self.modifier {
            Some(SelectModifier::Distinct) => w.token("DISTINCT"),
            Some(SelectModifier::Reduced) => w.token("REDUCED"),
            None => {}
        }
        match &self.projection {
            Projection::Star => w.token("*"),
            Projection::Items(items) => {
                for item in items {
                    match item {
                        ProjectionItem::Var(v) => v.write_sparql(w),
                        ProjectionItem::Expr(e, v) => {
                            w.open("(");
                            e.write_sparql(w);
                            w.token("AS");
                            v.write_sparql(w);
                            w.glue(")");
                        }
                    }
                }
            }
        }
    }
}

/// `DatasetClause` (production 13).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DatasetClause {
    /// `FROM <iri>`
    Default(Iri),
    /// `FROM NAMED <iri>`
    Named(Iri),
}

impl WriteSparql for DatasetClause {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token("FROM");
        match self {
            Self::Default(i) => i.write_sparql(w),
            Self::Named(i) => {
                w.token("NAMED");
                i.write_sparql(w);
            }
        }
    }
}

fn write_dataset(w: &mut TokenWriter, dataset: &[DatasetClause]) {
    for d in dataset {
        d.write_sparql(w);
    }
}

/// `WhereClause` (production 17).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WhereClause {
    /// Whether the optional `WHERE` keyword is written.
    pub keyword: bool,
    /// The pattern.
    pub pattern: GroupGraphPattern,
}

impl WriteSparql for WhereClause {
    fn write_sparql(&self, w: &mut TokenWriter) {
        if self.keyword {
            w.token("WHERE");
        }
        self.pattern.write_sparql(w);
    }
}

/// `GroupCondition` (production 20).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupCondition {
    /// A built-in call.
    BuiltIn(BuiltInCall),
    /// A function call.
    Function(FunctionCall),
    /// `(expr)` or `(expr AS ?v)`.
    Expr(Expression, Option<Variable>),
    /// A variable.
    Var(Variable),
}

impl WriteSparql for GroupCondition {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::BuiltIn(b) => b.write_sparql(w),
            Self::Function(f) => f.write_sparql(w),
            Self::Expr(e, v) => {
                w.open("(");
                e.write_sparql(w);
                if let Some(v) = v {
                    w.token("AS");
                    v.write_sparql(w);
                }
                w.glue(")");
            }
            Self::Var(v) => v.write_sparql(w),
        }
    }
}

/// `OrderCondition` (production 24).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OrderCondition {
    /// `ASC(expr)`
    Asc(Expression),
    /// `DESC(expr)`
    Desc(Expression),
    /// A constraint (ascending).
    Constraint(Constraint),
    /// A variable (ascending).
    Var(Variable),
}

impl WriteSparql for OrderCondition {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Asc(e) | Self::Desc(e) => {
                w.token(if matches!(self, Self::Asc(_)) {
                    "ASC"
                } else {
                    "DESC"
                });
                w.glue_open("(");
                e.write_sparql(w);
                w.glue(")");
            }
            Self::Constraint(c) => c.write_sparql(w),
            Self::Var(v) => v.write_sparql(w),
        }
    }
}

/// `LimitOffsetClauses` (production 25), preserving the clause order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitOffset {
    /// `LIMIT n (OFFSET m)?`
    LimitFirst {
        /// The limit.
        limit: u64,
        /// The offset.
        offset: Option<u64>,
    },
    /// `OFFSET m (LIMIT n)?`
    OffsetFirst {
        /// The offset.
        offset: u64,
        /// The limit.
        limit: Option<u64>,
    },
}

impl WriteSparql for LimitOffset {
    fn write_sparql(&self, w: &mut TokenWriter) {
        let limit = |w: &mut TokenWriter, n: u64| {
            w.token("LIMIT");
            w.token(&n.to_string());
        };
        let offset = |w: &mut TokenWriter, n: u64| {
            w.token("OFFSET");
            w.token(&n.to_string());
        };
        match *self {
            Self::LimitFirst {
                limit: l,
                offset: o,
            } => {
                limit(w, l);
                if let Some(o) = o {
                    offset(w, o);
                }
            }
            Self::OffsetFirst {
                offset: o,
                limit: l,
            } => {
                offset(w, o);
                if let Some(l) = l {
                    limit(w, l);
                }
            }
        }
    }
}

/// `SolutionModifier` (production 18).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SolutionModifier {
    /// `GROUP BY` conditions; empty if absent.
    pub group_by: Vec<GroupCondition>,
    /// `HAVING` conditions; empty if absent.
    pub having: Vec<Constraint>,
    /// `ORDER BY` conditions; empty if absent.
    pub order_by: Vec<OrderCondition>,
    /// `LIMIT` and `OFFSET`.
    pub limit_offset: Option<LimitOffset>,
}

impl WriteSparql for SolutionModifier {
    fn write_sparql(&self, w: &mut TokenWriter) {
        if !self.group_by.is_empty() {
            w.token("GROUP BY");
            for c in &self.group_by {
                c.write_sparql(w);
            }
        }
        if !self.having.is_empty() {
            w.token("HAVING");
            for c in &self.having {
                c.write_sparql(w);
            }
        }
        if !self.order_by.is_empty() {
            w.token("ORDER BY");
            for c in &self.order_by {
                c.write_sparql(w);
            }
        }
        if let Some(lo) = &self.limit_offset {
            lo.write_sparql(w);
        }
    }
}

/// `SelectQuery` (production 7).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectQuery {
    /// The select clause.
    pub select: SelectClause,
    /// The dataset clauses.
    pub dataset: Vec<DatasetClause>,
    /// The where clause.
    pub where_clause: WhereClause,
    /// The solution modifiers.
    pub modifiers: SolutionModifier,
}

impl WriteSparql for SelectQuery {
    fn write_sparql(&self, w: &mut TokenWriter) {
        self.select.write_sparql(w);
        write_dataset(w, &self.dataset);
        self.where_clause.write_sparql(w);
        self.modifiers.write_sparql(w);
    }
}

/// `SubSelect` (production 8).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubSelect {
    /// The select clause.
    pub select: SelectClause,
    /// The where clause.
    pub where_clause: WhereClause,
    /// The solution modifiers.
    pub modifiers: SolutionModifier,
    /// The trailing `VALUES` clause.
    pub values: Option<DataBlock>,
}

impl WriteSparql for SubSelect {
    fn write_sparql(&self, w: &mut TokenWriter) {
        self.select.write_sparql(w);
        self.where_clause.write_sparql(w);
        self.modifiers.write_sparql(w);
        write_values_clause(w, self.values.as_ref());
    }
}

/// `ConstructQuery` (production 10).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstructQuery {
    /// `CONSTRUCT { template } dataset* where modifiers`
    Template {
        /// The template.
        template: TriplesTemplate,
        /// The dataset clauses.
        dataset: Vec<DatasetClause>,
        /// The where clause.
        where_clause: WhereClause,
        /// The solution modifiers.
        modifiers: SolutionModifier,
    },
    /// `CONSTRUCT dataset* WHERE { triples } modifiers`
    Short {
        /// The dataset clauses.
        dataset: Vec<DatasetClause>,
        /// The triples, serving both as pattern and as template.
        triples: TriplesTemplate,
        /// The solution modifiers.
        modifiers: SolutionModifier,
    },
}

impl WriteSparql for ConstructQuery {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token("CONSTRUCT");
        match self {
            Self::Template {
                template,
                dataset,
                where_clause,
                modifiers,
            } => {
                w.token("{");
                template.write_sparql(w);
                w.token("}");
                write_dataset(w, dataset);
                where_clause.write_sparql(w);
                modifiers.write_sparql(w);
            }
            Self::Short {
                dataset,
                triples,
                modifiers,
            } => {
                write_dataset(w, dataset);
                w.token("WHERE");
                w.token("{");
                triples.write_sparql(w);
                w.token("}");
                modifiers.write_sparql(w);
            }
        }
    }
}

/// `DescribeQuery` (production 11).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DescribeQuery {
    /// The described resources; empty means `*`.
    pub targets: Vec<VarOrIri>,
    /// The dataset clauses.
    pub dataset: Vec<DatasetClause>,
    /// The optional where clause.
    pub where_clause: Option<WhereClause>,
    /// The solution modifiers.
    pub modifiers: SolutionModifier,
}

impl WriteSparql for DescribeQuery {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token("DESCRIBE");
        if self.targets.is_empty() {
            w.token("*");
        }
        for t in &self.targets {
            t.write_sparql(w);
        }
        write_dataset(w, &self.dataset);
        if let Some(wc) = &self.where_clause {
            wc.write_sparql(w);
        }
        self.modifiers.write_sparql(w);
    }
}

/// `AskQuery` (production 12).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AskQuery {
    /// The dataset clauses.
    pub dataset: Vec<DatasetClause>,
    /// The where clause.
    pub where_clause: WhereClause,
    /// The solution modifiers.
    pub modifiers: SolutionModifier,
}

impl WriteSparql for AskQuery {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token("ASK");
        write_dataset(w, &self.dataset);
        self.where_clause.write_sparql(w);
        self.modifiers.write_sparql(w);
    }
}

display_via_sparql!(
    Prologue,
    Query,
    QueryForm,
    SelectClause,
    SelectQuery,
    SubSelect,
    ConstructQuery,
    DescribeQuery,
    AskQuery,
    SolutionModifier,
);
