//! Group graph patterns, triple patterns, property paths and `VALUES`.

use oxrdf::vocab::rdf;
use oxrdf::{BlankNode as RdfBlankNode, NamedNode, Term};

use super::context::{Gen, Scope, Value};
use crate::ast::{
    BlankNode, BlankNodeLabel, Constraint, DataBlock, DataBlockValue, Expression, GraphNode,
    GraphNodePath, GraphPatternNotTriples, GraphTerm, GroupElement, GroupGraphPattern, Iri,
    NegatedSetStyle, Path, PathElt, PathEltOrInverse, PathMod, PathOneInPropertySet, PathPrimary,
    PathSequence, PropertyList, PropertyListPath, TriplesBlock, TriplesNode, TriplesNodePath,
    TriplesSameSubject, TriplesSameSubjectPath, TriplesTemplate, VarOrIri, VarOrTerm, Variable,
    Verb, VerbPath,
};
use crate::extension::PatternContext;
use crate::provider::{
    ContextPredicate, ContextTerm, ContextTriple, PartialTriple, Slot, TermKinds, ValueType,
};

/// How a group graph pattern relates to its surroundings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupKind {
    /// A group with its own scope.
    Plain,
    /// The pattern of `EXISTS`, which sees the variables of the current group.
    Exists,
    /// A group whose body is a sub-query.
    SubSelect,
}

/// The alternatives for an element of a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementKind {
    Triples,
    Group,
    SubSelect,
    Union,
    Optional,
    Minus,
    Graph,
    Service,
    Filter,
    Bind,
    Values,
    Extension(usize),
}

impl Gen<'_> {
    // -- groups ---------------------------------------------------------------

    /// Generates a `GroupGraphPattern` and returns it with its scope.
    ///
    /// Context triples added inside the group are kept; callers remove them
    /// (with [`Gen::truncate_context`]) where they do not constrain the
    /// enclosing pattern.
    pub fn group(&mut self, kind: GroupKind) -> (GroupGraphPattern, Scope) {
        self.hit("GroupGraphPattern");
        let scope = match kind {
            GroupKind::Exists => {
                let current = self.scopes.last().cloned().unwrap_or_default();
                Scope {
                    certain: current.certain,
                    possible: current.possible,
                    barrier: false,
                }
            }
            GroupKind::Plain | GroupKind::SubSelect => Scope::default(),
        };
        self.scopes.push(scope);
        self.depth += 1;
        self.out.token("{");
        let sub_select = kind == GroupKind::SubSelect
            || (self.can_nest() && self.coin(self.w.pattern.subselect / 20.0));
        let pattern = if sub_select {
            GroupGraphPattern::SubSelect(Box::new(self.sub_select()))
        } else {
            GroupGraphPattern::Group(self.group_elements())
        };
        self.out.token("}");
        self.depth -= 1;
        // A group ends the enclosing basic graph pattern.
        self.block_bnodes.clear();
        let scope = self.scopes.pop().unwrap_or_default();
        (pattern, scope)
    }

    /// Removes context triples added after `mark`.
    pub fn truncate_context(&mut self, mark: usize) {
        self.context.truncate(mark);
    }

    fn group_elements(&mut self) -> Vec<GroupElement> {
        let mut elements = Vec::new();
        if self.flip(0.03) {
            self.hit("EmptyGroup");
            return elements;
        }
        let n = self.count(0.55, self.limits.max_group_elements);
        let mut open_triples = false;
        for _ in 0..n {
            let kind = self.element_kind();
            if kind == ElementKind::Triples {
                if open_triples {
                    self.out.token(".");
                }
                let block = self.triples_block();
                open_triples = !block.trailing_dot;
                elements.push(GroupElement::Triples(block));
            } else {
                let pattern = self.pattern_not_triples(kind);
                let dot = self.flip(self.w.pattern.p_dot_after_pattern);
                if dot {
                    self.out.token(".");
                }
                open_triples = false;
                elements.push(GroupElement::Pattern { pattern, dot });
            }
        }
        elements
    }

    fn element_kind(&mut self) -> ElementKind {
        let p = &self.w.pattern;
        let nest = self.can_nest();
        let n = |w: f64| if nest { w } else { 0.0 };
        let mut options = vec![
            (ElementKind::Triples, p.triples),
            (ElementKind::Group, n(p.group)),
            (ElementKind::SubSelect, n(p.subselect)),
            (ElementKind::Union, n(p.union)),
            (ElementKind::Optional, n(p.optional)),
            (ElementKind::Minus, n(p.minus)),
            (ElementKind::Graph, n(p.graph)),
            (ElementKind::Service, n(p.service)),
            (
                ElementKind::Filter,
                if self.has_budget() { p.filter } else { 0.0 },
            ),
            (
                ElementKind::Bind,
                if self.has_budget() { p.bind } else { 0.0 },
            ),
            (
                ElementKind::Values,
                if self.has_budget() { p.values } else { 0.0 },
            ),
        ];
        for (i, e) in self.extensions.iter().enumerate() {
            options.push((ElementKind::Extension(i), e.pattern_weight() * p.extension));
        }
        self.choose(&options).unwrap_or(ElementKind::Triples)
    }

    fn pattern_not_triples(&mut self, kind: ElementKind) -> GraphPatternNotTriples {
        let mark = self.context.len();
        match kind {
            ElementKind::Triples => unreachable!("handled by the caller"),
            ElementKind::Group | ElementKind::SubSelect => {
                self.hit("GroupOrUnionGraphPattern");
                let group_kind = if kind == ElementKind::SubSelect {
                    GroupKind::SubSelect
                } else {
                    GroupKind::Plain
                };
                let (g, scope) = self.group(group_kind);
                self.scope().join(&scope);
                if matches!(g, GroupGraphPattern::SubSelect(_)) {
                    self.truncate_context(mark);
                }
                GraphPatternNotTriples::GroupOrUnion(vec![g])
            }
            ElementKind::Union => self.union_pattern(mark),
            ElementKind::Optional => {
                self.hit("OptionalGraphPattern");
                self.out.token("OPTIONAL");
                let (g, scope) = self.group(GroupKind::Plain);
                self.truncate_context(mark);
                self.scope().optional(&scope);
                GraphPatternNotTriples::Optional(g)
            }
            ElementKind::Minus => {
                self.hit("MinusGraphPattern");
                self.out.token("MINUS");
                let (g, _) = self.group(GroupKind::Plain);
                self.truncate_context(mark);
                GraphPatternNotTriples::Minus(g)
            }
            ElementKind::Graph => {
                self.hit("GraphGraphPattern");
                self.out.token("GRAPH");
                let name = self.var_or_iri(Slot::GraphName);
                let (g, scope) = self.group(GroupKind::Plain);
                self.truncate_context(mark);
                if let VarOrIri::Var(v) = &name {
                    self.bind(v, true);
                }
                self.scope().join(&scope);
                GraphPatternNotTriples::Graph(name, g)
            }
            ElementKind::Service => self.service_pattern(mark),
            ElementKind::Filter => {
                self.hit("Filter");
                self.out.token("FILTER");
                GraphPatternNotTriples::Filter(self.constraint())
            }
            ElementKind::Bind => self.bind_clause(),
            ElementKind::Values => {
                self.hit("InlineData");
                self.out.token("VALUES");
                GraphPatternNotTriples::Values(self.data_block())
            }
            ElementKind::Extension(i) => self.extension_pattern(i),
        }
    }

    /// Two or more groups joined by `UNION`.
    fn union_pattern(&mut self, mark: usize) -> GraphPatternNotTriples {
        self.hit("UnionGraphPattern");
        let mut groups = Vec::new();
        let mut merged: Option<Scope> = None;
        let n = 1 + self.count(self.w.pattern.p_more_union, self.limits.max_list.max(2) - 1);
        for i in 0..n {
            if i > 0 {
                self.out.token("UNION");
            }
            let (g, scope) = self.group(GroupKind::Plain);
            self.truncate_context(mark);
            // Certain in every branch, possible in any.
            merged = Some(match merged {
                None => scope,
                Some(mut m) => {
                    m.certain = m.certain.intersection(&scope.certain).cloned().collect();
                    m.possible.extend(scope.possible);
                    m
                }
            });
            groups.push(g);
        }
        if let Some(m) = merged {
            self.scope().join(&m);
        }
        GraphPatternNotTriples::GroupOrUnion(groups)
    }

    fn service_pattern(&mut self, mark: usize) -> GraphPatternNotTriples {
        self.hit("ServiceGraphPattern");
        self.out.token("SERVICE");
        let silent = self.flip(self.w.pattern.p_service_silent);
        if silent {
            self.out.token("SILENT");
        }
        let endpoint = self.var_or_iri(Slot::ServiceEndpoint);
        let (pattern, scope) = self.group(GroupKind::Plain);
        self.truncate_context(mark);
        if let VarOrIri::Var(v) = &endpoint {
            self.bind(v, true);
        }
        self.scope().join(&scope);
        GraphPatternNotTriples::Service {
            silent,
            endpoint,
            pattern,
        }
    }

    fn bind_clause(&mut self) -> GraphPatternNotTriples {
        self.hit("Bind");
        self.out.token("BIND");
        self.out.glue_open("(");
        let (e, ty) = self.expr_typed(ValueType::Any, 1);
        self.out.token("AS");
        let v = self.take_fresh();
        self.emit(&v);
        self.out.glue(")");
        self.scope().bind(&v, false);
        self.vars.types.insert(v.clone(), ty);
        GraphPatternNotTriples::Bind(e, v)
    }

    fn extension_pattern(&mut self, index: usize) -> GraphPatternNotTriples {
        let extensions = self.extensions;
        let extension = &extensions[index];
        let variables = self.group_variables();
        let fresh = self.peek_fresh();
        let mut context = PatternContext {
            rng: &mut self.rng,
            variables: &variables,
            fresh_variable: &fresh,
        };
        if let Some(p) = extension.generate_pattern(&mut context) {
            self.hit("ExtensionPattern");
            self.emit(&p.pattern);
            for v in &p.binds {
                self.bind(v, false);
            }
            p.pattern
        } else {
            self.hit("Filter");
            self.out.token("FILTER");
            GraphPatternNotTriples::Filter(self.constraint())
        }
    }

    /// A `Constraint` for `FILTER` or `HAVING`.
    pub fn constraint(&mut self) -> Constraint {
        if self.flip(0.2) {
            match self.call_expr(ValueType::Boolean) {
                Expression::BuiltIn(b) => return Constraint::BuiltIn(b),
                Expression::Function(f) => return Constraint::Function(f),
                other => unreachable!("call_expr returned {other:?}"),
            }
        }
        self.out.open("(");
        let e = self.expr(ValueType::Boolean, 1);
        self.out.glue(")");
        Constraint::Bracketed(e)
    }

    fn var_or_iri(&mut self, slot: Slot) -> VarOrIri {
        match self.value(
            slot,
            TermKinds::VARIABLE | TermKinds::IRI,
            ValueType::Iri,
            None,
            None,
        ) {
            Value::Var(v) => {
                self.emit(&v);
                VarOrIri::Var(v)
            }
            Value::Term(t) => {
                let iri = self.iri_of(&t);
                self.emit(&iri);
                VarOrIri::Iri(iri)
            }
        }
    }

    /// An IRI for a term that is expected to be an IRI.
    pub fn iri_of(&mut self, term: &Term) -> Iri {
        match term {
            Term::NamedNode(n) => self.iri(n.as_ref()),
            _ => self.iri(NamedNode::new_unchecked("http://example.org/sparqlright/0").as_ref()),
        }
    }

    // -- VALUES ---------------------------------------------------------------

    /// A `DataBlock`.
    pub fn data_block(&mut self) -> DataBlock {
        let one = self.w.data.one_var;
        let full = self.w.data.full;
        let one_var = self.choose(&[(true, one), (false, full)]).unwrap_or(true);
        let rows = self.rng_rows();
        if one_var {
            let var = self.values_var(&[]);
            self.emit(&var);
            self.out.token("{");
            let mut values = Vec::new();
            for _ in 0..rows {
                values.push(self.values_cell(&var));
            }
            self.out.token("}");
            let certain = !values.is_empty() && !values.contains(&DataBlockValue::Undef);
            self.bind(&var, certain);
            DataBlock::OneVar { var, values }
        } else {
            let n = if self.flip(0.05) {
                0
            } else {
                self.count(0.4, self.limits.max_values_vars)
            };
            let mut vars = Vec::new();
            for _ in 0..n {
                let v = self.values_var(&vars);
                vars.push(v);
            }
            self.parenthesized(&vars);
            self.out.token("{");
            let mut data = Vec::new();
            for _ in 0..rows {
                if vars.is_empty() {
                    self.out.token("()");
                    data.push(Vec::new());
                    continue;
                }
                self.out.open("(");
                let row: Vec<DataBlockValue> = vars.iter().map(|v| self.values_cell(v)).collect();
                self.out.glue(")");
                data.push(row);
            }
            self.out.token("}");
            for (i, v) in vars.iter().enumerate() {
                let certain =
                    !data.is_empty() && data.iter().all(|row| row[i] != DataBlockValue::Undef);
                self.bind(v, certain);
            }
            DataBlock::Full { vars, rows: data }
        }
    }

    fn rng_rows(&mut self) -> u32 {
        if self.flip(0.05) {
            0
        } else {
            self.count(0.5, self.limits.max_values_rows)
        }
    }

    fn parenthesized(&mut self, vars: &[Variable]) {
        if vars.is_empty() {
            self.out.token("()");
            return;
        }
        self.out.open("(");
        for v in vars {
            self.emit(v);
        }
        self.out.glue(")");
    }

    /// A variable for a `VALUES` block: often one in scope (a join), never
    /// one already in `taken`.
    fn values_var(&mut self, taken: &[Variable]) -> Variable {
        let candidates: Vec<Variable> = self
            .group_variables()
            .into_iter()
            .filter(|v| !taken.contains(v))
            .collect();
        if !self.env.planning && !candidates.is_empty() && self.flip(0.5) {
            let v = self
                .pick(&candidates)
                .cloned()
                .unwrap_or_else(|| self.take_fresh());
            return self.with_sigil(v);
        }
        self.take_fresh()
    }

    fn values_cell(&mut self, var: &Variable) -> DataBlockValue {
        if self.flip(self.w.data.p_undef) {
            self.out.token("UNDEF");
            return DataBlockValue::Undef;
        }
        let value = self.value(
            Slot::ValuesCell,
            TermKinds::IRI | TermKinds::LITERAL,
            ValueType::Any,
            Some(var),
            None,
        );
        let cell = match value {
            Value::Term(Term::NamedNode(n)) => DataBlockValue::Iri(self.iri(n.as_ref())),
            Value::Term(Term::Literal(l)) => DataBlockValue::Literal(self.literal(&l)),
            _ => DataBlockValue::Undef,
        };
        self.emit(&cell);
        cell
    }

    // -- triples with paths ---------------------------------------------------

    fn triples_block(&mut self) -> TriplesBlock {
        self.hit("TriplesBlock");
        self.block_bnodes.clear();
        let n = self.count(0.45, self.limits.max_triples);
        let mut triples = Vec::new();
        for i in 0..n {
            if i > 0 {
                self.out.token(".");
            }
            triples.push(self.triples_same_subject_path());
        }
        let trailing_dot = self.flip(self.w.pattern.p_trailing_dot);
        if trailing_dot {
            self.out.token(".");
        }
        TriplesBlock {
            triples,
            trailing_dot,
        }
    }

    fn node_kind(&mut self, term: f64, list: f64, collection: f64) -> u8 {
        let nodes =
            self.allow_bnodes && self.node_depth < self.limits.max_node_depth && self.has_budget();
        let n = |w: f64| if nodes { w } else { 0.0 };
        self.choose(&[(0, term), (1, n(list)), (2, n(collection))])
            .unwrap_or(0)
    }

    fn triples_same_subject_path(&mut self) -> TriplesSameSubjectPath {
        self.hit("TriplesSameSubjectPath");
        let t = &self.w.triple;
        match self.node_kind(
            t.subject_term,
            t.subject_property_list,
            t.subject_collection,
        ) {
            0 => {
                let (subject, ctx) = self.subject_term();
                let properties = self.property_list_path(&ctx);
                TriplesSameSubjectPath::Subject {
                    subject,
                    properties,
                }
            }
            kind => {
                let (node, ctx) = if kind == 1 {
                    self.blank_node_property_list_path()
                } else {
                    self.collection_path()
                };
                let properties = if self.flip(self.w.triple.p_node_without_properties) {
                    None
                } else {
                    Some(self.property_list_path(&ctx))
                };
                TriplesSameSubjectPath::Node { node, properties }
            }
        }
    }

    /// A blank node label that is valid in the current basic graph pattern.
    fn blank_node_label(&mut self) -> (BlankNode, ContextTerm) {
        let label = if !self.block_bnodes.is_empty() && self.flip(0.5) {
            self.pick(&self.block_bnodes.clone()).cloned()
        } else {
            None
        };
        let label = label.unwrap_or_else(|| {
            self.bnode_counter += 1;
            let l = BlankNodeLabel::new(format!("b{}", self.bnode_counter)).expect("valid label");
            self.block_bnodes.push(l.clone());
            l
        });
        let ctx = ContextTerm::Term(Term::BlankNode(RdfBlankNode::new_unchecked(format!(
            "q{}",
            label.as_str()
        ))));
        (BlankNode::Labeled(label), ctx)
    }

    fn anonymous_context(&mut self) -> ContextTerm {
        self.anon_counter += 1;
        ContextTerm::Term(Term::BlankNode(RdfBlankNode::new_unchecked(format!(
            "anon{}",
            self.anon_counter
        ))))
    }

    /// A subject that is a variable, IRI or blank node label.
    fn subject_term(&mut self) -> (VarOrTerm, ContextTerm) {
        if self.allow_bnodes && self.flip(self.w.triple.p_blank_node_label) {
            let (b, ctx) = self.blank_node_label();
            let term = VarOrTerm::Term(GraphTerm::BlankNode(b));
            self.emit(&term);
            return (term, ctx);
        }
        let partial = PartialTriple::default();
        let value = self.value(
            Slot::Subject,
            TermKinds::VARIABLE | TermKinds::IRI,
            ValueType::Any,
            None,
            Some(&partial),
        );
        self.term_value(value, false)
    }

    /// Emits a chosen subject or object value.
    fn term_value(&mut self, value: Value, predicate_position: bool) -> (VarOrTerm, ContextTerm) {
        match value {
            Value::Var(v) => {
                self.bind(&v, true);
                if predicate_position {
                    self.vars.types.insert(v.clone(), ValueType::Iri);
                }
                self.emit(&v);
                (VarOrTerm::Var(v.clone()), ContextTerm::Variable(v))
            }
            Value::Term(t) => {
                let term = self.graph_term(&t);
                self.emit(&term);
                (VarOrTerm::Term(term), ContextTerm::Term(t))
            }
        }
    }

    fn object_term(
        &mut self,
        subject: &ContextTerm,
        predicate: &ContextPredicate,
    ) -> (VarOrTerm, ContextTerm) {
        if self.allow_bnodes && self.flip(self.w.triple.p_blank_node_label) {
            let (b, ctx) = self.blank_node_label();
            let term = VarOrTerm::Term(GraphTerm::BlankNode(b));
            self.emit(&term);
            return (term, ctx);
        }
        let partial = PartialTriple {
            subject: Some(subject.clone()),
            predicate: Some(predicate.clone()),
        };
        let value = self.value(
            Slot::Object,
            TermKinds::VARIABLE | TermKinds::IRI | TermKinds::LITERAL,
            ValueType::Any,
            None,
            Some(&partial),
        );
        self.term_value(value, false)
    }

    fn property_list_path(&mut self, subject: &ContextTerm) -> PropertyListPath {
        let (verb, predicate) = self.verb_path(subject);
        let objects = self.object_list_path(subject, &predicate);
        let mut rest = Vec::new();
        while rest.len() < self.limits.max_list as usize && self.coin(self.w.triple.p_more_verbs) {
            self.out.token(";");
            if self.flip(self.w.triple.p_empty_entry) {
                rest.push(None);
                continue;
            }
            let (verb, predicate) = self.verb_path(subject);
            let objects = self.object_list(subject, &predicate);
            rest.push(Some((verb, objects)));
        }
        PropertyListPath {
            verb,
            objects,
            rest,
        }
    }

    fn verb_path(&mut self, subject: &ContextTerm) -> (VerbPath, ContextPredicate) {
        if self.coin(self.w.triple.p_path) {
            self.hit("Path");
            let path = self.path();
            let text = crate::ast::WriteSparql::to_sparql(&path);
            return (VerbPath::Path(path), ContextPredicate::Path(text));
        }
        let partial = PartialTriple {
            subject: Some(subject.clone()),
            predicate: None,
        };
        match self.value(
            Slot::Predicate,
            TermKinds::VARIABLE | TermKinds::IRI,
            ValueType::Iri,
            None,
            Some(&partial),
        ) {
            Value::Var(v) => {
                self.bind(&v, true);
                self.vars.types.insert(v.clone(), ValueType::Iri);
                self.emit(&v);
                (VerbPath::Var(v.clone()), ContextPredicate::Variable(v))
            }
            Value::Term(t) => {
                let n = match &t {
                    Term::NamedNode(n) => n.clone(),
                    _ => NamedNode::new_unchecked("http://example.org/sparqlright/0"),
                };
                let primary = if n == rdf::TYPE && self.flip(self.w.prologue.p_a_keyword) {
                    PathPrimary::A
                } else {
                    PathPrimary::Iri(self.iri(n.as_ref()))
                };
                let path = Path::from_primary(primary);
                self.emit(&path);
                (VerbPath::Path(path), ContextPredicate::Iri(n))
            }
        }
    }

    fn object_list_path(
        &mut self,
        subject: &ContextTerm,
        predicate: &ContextPredicate,
    ) -> Vec<GraphNodePath> {
        let n = self.count(self.w.triple.p_more_objects, self.limits.max_list);
        let mut objects = Vec::new();
        for i in 0..n {
            if i > 0 {
                self.out.glue(",");
            }
            let t = &self.w.triple;
            let (node, ctx) =
                match self.node_kind(t.object_term, t.object_property_list, t.object_collection) {
                    0 => {
                        let (term, ctx) = self.object_term(subject, predicate);
                        (GraphNodePath::Term(term), ctx)
                    }
                    1 => {
                        let (node, ctx) = self.blank_node_property_list_path();
                        (GraphNodePath::Node(Box::new(node)), ctx)
                    }
                    _ => {
                        let (node, ctx) = self.collection_path();
                        (GraphNodePath::Node(Box::new(node)), ctx)
                    }
                };
            self.context.push(ContextTriple {
                subject: subject.clone(),
                predicate: predicate.clone(),
                object: ctx,
            });
            objects.push(node);
        }
        objects
    }

    fn blank_node_property_list_path(&mut self) -> (TriplesNodePath, ContextTerm) {
        self.hit("BlankNodePropertyListPath");
        self.node_depth += 1;
        self.out.token("[");
        let ctx = self.anonymous_context();
        let properties = self.property_list_path(&ctx);
        self.out.token("]");
        self.node_depth -= 1;
        (TriplesNodePath::BlankNodePropertyList(properties), ctx)
    }

    fn collection_path(&mut self) -> (TriplesNodePath, ContextTerm) {
        self.hit("CollectionPath");
        self.node_depth += 1;
        let ctx = self.anonymous_context();
        let first = ContextPredicate::Iri(rdf::FIRST.into_owned());
        self.out.open("(");
        let n = self.count(0.5, self.limits.max_list);
        let mut items = Vec::new();
        for _ in 0..n {
            let t = &self.w.triple;
            let item =
                match self.node_kind(t.object_term, t.object_property_list, t.object_collection) {
                    0 => GraphNodePath::Term(self.object_term(&ctx, &first).0),
                    1 => GraphNodePath::Node(Box::new(self.blank_node_property_list_path().0)),
                    _ => GraphNodePath::Node(Box::new(self.collection_path().0)),
                };
            items.push(item);
        }
        self.out.glue(")");
        self.node_depth -= 1;
        (TriplesNodePath::Collection(items), ctx)
    }

    // -- triples without paths ------------------------------------------------

    /// A `TriplesTemplate` (for `CONSTRUCT`, `CONSTRUCT WHERE` and updates).
    pub fn triples_template(&mut self, max: u32) -> TriplesTemplate {
        self.hit("TriplesTemplate");
        self.block_bnodes.clear();
        let n = self.count(0.45, max);
        let mut triples = Vec::new();
        for i in 0..n {
            if i > 0 {
                self.out.token(".");
            }
            triples.push(self.triples_same_subject());
        }
        let trailing_dot = !triples.is_empty() && self.flip(self.w.pattern.p_trailing_dot);
        if trailing_dot {
            self.out.token(".");
        }
        TriplesTemplate {
            triples,
            trailing_dot,
        }
    }

    fn triples_same_subject(&mut self) -> TriplesSameSubject {
        self.hit("TriplesSameSubject");
        let t = &self.w.triple;
        match self.node_kind(
            t.subject_term,
            t.subject_property_list,
            t.subject_collection,
        ) {
            0 => {
                let (subject, ctx) = self.subject_term();
                let properties = self.property_list(&ctx);
                TriplesSameSubject::Subject {
                    subject,
                    properties,
                }
            }
            kind => {
                let (node, ctx) = if kind == 1 {
                    self.blank_node_property_list()
                } else {
                    self.collection()
                };
                let properties = if self.flip(self.w.triple.p_node_without_properties) {
                    None
                } else {
                    Some(self.property_list(&ctx))
                };
                TriplesSameSubject::Node { node, properties }
            }
        }
    }

    fn property_list(&mut self, subject: &ContextTerm) -> PropertyList {
        let (verb, predicate) = self.verb(subject);
        let objects = self.object_list(subject, &predicate);
        let mut rest = Vec::new();
        while rest.len() < self.limits.max_list as usize && self.coin(self.w.triple.p_more_verbs) {
            self.out.token(";");
            if self.flip(self.w.triple.p_empty_entry) {
                rest.push(None);
                continue;
            }
            let (verb, predicate) = self.verb(subject);
            let objects = self.object_list(subject, &predicate);
            rest.push(Some((verb, objects)));
        }
        PropertyList {
            verb,
            objects,
            rest,
        }
    }

    /// A `Verb`, which is converted to a [`VerbPath`] where needed.
    fn verb(&mut self, subject: &ContextTerm) -> (Verb, ContextPredicate) {
        let partial = PartialTriple {
            subject: Some(subject.clone()),
            predicate: None,
        };
        match self.value(
            Slot::Predicate,
            TermKinds::VARIABLE | TermKinds::IRI,
            ValueType::Iri,
            None,
            Some(&partial),
        ) {
            Value::Var(v) => {
                self.bind(&v, true);
                self.vars.types.insert(v.clone(), ValueType::Iri);
                self.emit(&v);
                (
                    Verb::VarOrIri(VarOrIri::Var(v.clone())),
                    ContextPredicate::Variable(v),
                )
            }
            Value::Term(t) => {
                let n = match &t {
                    Term::NamedNode(n) => n.clone(),
                    _ => NamedNode::new_unchecked("http://example.org/sparqlright/0"),
                };
                let verb = if n == rdf::TYPE && self.flip(self.w.prologue.p_a_keyword) {
                    Verb::A
                } else {
                    Verb::VarOrIri(VarOrIri::Iri(self.iri(n.as_ref())))
                };
                self.emit(&verb);
                (verb, ContextPredicate::Iri(n))
            }
        }
    }

    fn object_list(
        &mut self,
        subject: &ContextTerm,
        predicate: &ContextPredicate,
    ) -> Vec<GraphNode> {
        let n = self.count(self.w.triple.p_more_objects, self.limits.max_list);
        let mut objects = Vec::new();
        for i in 0..n {
            if i > 0 {
                self.out.glue(",");
            }
            let t = &self.w.triple;
            let (node, ctx) =
                match self.node_kind(t.object_term, t.object_property_list, t.object_collection) {
                    0 => {
                        let (term, ctx) = self.object_term(subject, predicate);
                        (GraphNode::Term(term), ctx)
                    }
                    1 => {
                        let (node, ctx) = self.blank_node_property_list();
                        (GraphNode::Node(Box::new(node)), ctx)
                    }
                    _ => {
                        let (node, ctx) = self.collection();
                        (GraphNode::Node(Box::new(node)), ctx)
                    }
                };
            self.context.push(ContextTriple {
                subject: subject.clone(),
                predicate: predicate.clone(),
                object: ctx,
            });
            objects.push(node);
        }
        objects
    }

    fn blank_node_property_list(&mut self) -> (TriplesNode, ContextTerm) {
        self.hit("BlankNodePropertyList");
        self.node_depth += 1;
        self.out.token("[");
        let ctx = self.anonymous_context();
        let properties = self.property_list(&ctx);
        self.out.token("]");
        self.node_depth -= 1;
        (TriplesNode::BlankNodePropertyList(properties), ctx)
    }

    fn collection(&mut self) -> (TriplesNode, ContextTerm) {
        self.hit("Collection");
        self.node_depth += 1;
        let ctx = self.anonymous_context();
        let first = ContextPredicate::Iri(rdf::FIRST.into_owned());
        self.out.open("(");
        let n = self.count(0.5, self.limits.max_list);
        let mut items = Vec::new();
        for _ in 0..n {
            let t = &self.w.triple;
            let item =
                match self.node_kind(t.object_term, t.object_property_list, t.object_collection) {
                    0 => GraphNode::Term(self.object_term(&ctx, &first).0),
                    1 => GraphNode::Node(Box::new(self.blank_node_property_list().0)),
                    _ => GraphNode::Node(Box::new(self.collection().0)),
                };
            items.push(item);
        }
        self.out.glue(")");
        self.node_depth -= 1;
        (TriplesNode::Collection(items), ctx)
    }

    // -- property paths -------------------------------------------------------

    /// A complex property path, written piece by piece as one token.
    fn path(&mut self) -> Path {
        let mut started = false;
        self.path_alternative(0, &mut started)
    }

    fn piece(&mut self, text: &str, started: &mut bool) {
        if *started {
            self.out.glue(text);
        } else {
            self.out.token(text);
            *started = true;
        }
    }

    fn path_alternative(&mut self, depth: u32, started: &mut bool) -> Path {
        let mut sequences = vec![self.path_sequence(depth, started)];
        while sequences.len() < self.limits.max_list as usize
            && self.coin(self.w.path.p_alternative)
        {
            self.piece("|", started);
            sequences.push(self.path_sequence(depth, started));
        }
        Path(sequences)
    }

    fn path_sequence(&mut self, depth: u32, started: &mut bool) -> PathSequence {
        let mut elements = vec![self.path_element(depth, started)];
        while elements.len() < self.limits.max_list as usize && self.coin(self.w.path.p_sequence) {
            self.piece("/", started);
            elements.push(self.path_element(depth, started));
        }
        PathSequence(elements)
    }

    fn path_element(&mut self, depth: u32, started: &mut bool) -> PathEltOrInverse {
        self.hit("PathElt");
        let inverse = self.flip(self.w.path.p_inverse);
        if inverse {
            self.piece("^", started);
        }
        let p = &self.w.path;
        let group = if depth < self.limits.max_path_depth && self.has_budget() {
            p.group
        } else {
            0.0
        };
        let kind = self
            .choose(&[(0, p.iri), (1, p.a), (2, p.negated), (3, group)])
            .unwrap_or(0);
        let primary = match kind {
            1 => {
                self.piece("a", started);
                PathPrimary::A
            }
            2 => {
                self.hit("PathNegatedPropertySet");
                self.piece("!", started);
                // `!()` is allowed by the EBNF but cannot be parsed with the
                // tokenization the grammar prescribes (`()` is `NIL`).
                let n = self.count(self.w.path.p_more_negated, self.limits.max_list);
                let style = if n == 1 && self.flip(0.5) {
                    NegatedSetStyle::Single
                } else {
                    NegatedSetStyle::Parenthesized
                };
                if style == NegatedSetStyle::Parenthesized {
                    self.piece("(", started);
                }
                let mut members = Vec::new();
                for i in 0..n {
                    if i > 0 {
                        self.piece("|", started);
                    }
                    let inverse = self.flip(self.w.path.p_negated_inverse);
                    if inverse {
                        self.piece("^", started);
                    }
                    let iri = if self.flip(0.1) {
                        self.piece("a", started);
                        None
                    } else {
                        let iri = self.path_iri();
                        self.piece(&crate::ast::WriteSparql::to_sparql(&iri), started);
                        Some(iri)
                    };
                    members.push(PathOneInPropertySet { inverse, iri });
                }
                if style == NegatedSetStyle::Parenthesized {
                    self.piece(")", started);
                }
                PathPrimary::Negated(members, style)
            }
            3 => {
                self.piece("(", started);
                let inner = self.path_alternative(depth + 1, started);
                self.piece(")", started);
                PathPrimary::Group(Box::new(inner))
            }
            _ => {
                let iri = self.path_iri();
                self.piece(&crate::ast::WriteSparql::to_sparql(&iri), started);
                PathPrimary::Iri(iri)
            }
        };
        let modifier = if self.coin(self.w.path.p_modifier) {
            let m = self
                .choose(&[
                    (PathMod::ZeroOrOne, 1.0),
                    (PathMod::ZeroOrMore, 1.0),
                    (PathMod::OneOrMore, 1.0),
                ])
                .unwrap_or(PathMod::ZeroOrMore);
            self.piece(
                match m {
                    PathMod::ZeroOrOne => "?",
                    PathMod::ZeroOrMore => "*",
                    PathMod::OneOrMore => "+",
                },
                started,
            );
            Some(m)
        } else {
            None
        };
        PathEltOrInverse {
            inverse,
            elt: PathElt { primary, modifier },
        }
    }

    fn path_iri(&mut self) -> Iri {
        let value = self.value(Slot::PathIri, TermKinds::IRI, ValueType::Iri, None, None);
        match value {
            Value::Term(t) => self.iri_of(&t),
            Value::Var(_) => self.iri_of(&Term::NamedNode(NamedNode::new_unchecked(
                "http://example.org/sparqlright/0",
            ))),
        }
    }
}
