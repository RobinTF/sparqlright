//! Graph patterns, triple patterns and property paths (productions 53–109).

use super::expr::{BuiltInCall, Expression, FunctionCall};
use super::query::SubSelect;
use super::term::{Iri, Literal, VarOrIri, VarOrTerm, Variable};
use super::writer::{TokenWriter, WriteSparql, display_via_sparql, write_comma_separated};

/// `GroupGraphPattern` (production 53).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupGraphPattern {
    /// `{ SELECT ... }`
    SubSelect(Box<SubSelect>),
    /// `{ elements }` (`GroupGraphPatternSub`).
    Group(Vec<GroupElement>),
}

impl GroupGraphPattern {
    /// The empty group `{}`.
    #[must_use]
    pub fn empty() -> Self {
        Self::Group(Vec::new())
    }
}

impl WriteSparql for GroupGraphPattern {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token("{");
        match self {
            Self::SubSelect(s) => s.write_sparql(w),
            Self::Group(elements) => write_group_elements(w, elements),
        }
        w.token("}");
    }
}

/// Writes the elements of a `GroupGraphPatternSub`.
///
/// Two adjacent triples blocks are separated by `.`, as the grammar requires.
pub(crate) fn write_group_elements(w: &mut TokenWriter, elements: &[GroupElement]) {
    let mut needs_dot = false;
    for element in elements {
        let is_triples = matches!(element, GroupElement::Triples(_));
        if is_triples && needs_dot {
            w.token(".");
        }
        element.write_sparql(w);
        needs_dot = matches!(element, GroupElement::Triples(t) if !t.trailing_dot);
    }
}

/// An element of a `GroupGraphPatternSub`: a triples block or one of the
/// alternatives of `GraphPatternNotTriples` (production 56).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupElement {
    /// A `TriplesBlock`.
    Triples(TriplesBlock),
    /// A `GraphPatternNotTriples`, optionally followed by `.`.
    Pattern {
        /// The pattern.
        pattern: GraphPatternNotTriples,
        /// Whether the optional `.` after the pattern is written.
        dot: bool,
    },
}

impl WriteSparql for GroupElement {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Triples(t) => t.write_sparql(w),
            Self::Pattern { pattern, dot } => {
                pattern.write_sparql(w);
                if *dot {
                    w.token(".");
                }
            }
        }
    }
}

/// `GraphPatternNotTriples` (production 56).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphPatternNotTriples {
    /// A nested group, or a `UNION` of two or more groups
    /// (`GroupOrUnionGraphPattern`, production 67).
    GroupOrUnion(Vec<GroupGraphPattern>),
    /// `OPTIONAL { ... }`
    Optional(GroupGraphPattern),
    /// `MINUS { ... }`
    Minus(GroupGraphPattern),
    /// `GRAPH ?g { ... }`
    Graph(VarOrIri, GroupGraphPattern),
    /// `SERVICE SILENT? ?s { ... }`
    Service {
        /// Whether `SILENT` is given.
        silent: bool,
        /// The service endpoint.
        endpoint: VarOrIri,
        /// The pattern sent to the service.
        pattern: GroupGraphPattern,
    },
    /// `FILTER constraint`
    Filter(Constraint),
    /// `BIND(expr AS ?v)`
    Bind(Expression, Variable),
    /// `VALUES ...`
    Values(DataBlock),
}

impl WriteSparql for GraphPatternNotTriples {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::GroupOrUnion(groups) => {
                for (i, g) in groups.iter().enumerate() {
                    if i > 0 {
                        w.token("UNION");
                    }
                    g.write_sparql(w);
                }
            }
            Self::Optional(g) => {
                w.token("OPTIONAL");
                g.write_sparql(w);
            }
            Self::Minus(g) => {
                w.token("MINUS");
                g.write_sparql(w);
            }
            Self::Graph(name, g) => {
                w.token("GRAPH");
                name.write_sparql(w);
                g.write_sparql(w);
            }
            Self::Service {
                silent,
                endpoint,
                pattern,
            } => {
                w.token("SERVICE");
                if *silent {
                    w.token("SILENT");
                }
                endpoint.write_sparql(w);
                pattern.write_sparql(w);
            }
            Self::Filter(c) => {
                w.token("FILTER");
                c.write_sparql(w);
            }
            Self::Bind(e, v) => {
                w.token("BIND");
                w.glue_open("(");
                e.write_sparql(w);
                w.token("AS");
                v.write_sparql(w);
                w.glue(")");
            }
            Self::Values(d) => {
                w.token("VALUES");
                d.write_sparql(w);
            }
        }
    }
}

/// `Constraint` (production 69): the argument of `FILTER` and `HAVING`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Constraint {
    /// `( expr )`
    Bracketed(Expression),
    /// A built-in call.
    BuiltIn(BuiltInCall),
    /// A function call.
    Function(FunctionCall),
}

impl WriteSparql for Constraint {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Bracketed(e) => {
                w.open("(");
                e.write_sparql(w);
                w.glue(")");
            }
            Self::BuiltIn(b) => b.write_sparql(w),
            Self::Function(f) => f.write_sparql(w),
        }
    }
}

/// A value in a `VALUES` block (`DataBlockValue`, production 65).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataBlockValue {
    /// An IRI.
    Iri(Iri),
    /// A literal.
    Literal(Literal),
    /// `UNDEF`
    Undef,
}

impl WriteSparql for DataBlockValue {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Iri(i) => i.write_sparql(w),
            Self::Literal(l) => l.write_sparql(w),
            Self::Undef => w.token("UNDEF"),
        }
    }
}

/// `DataBlock` (production 62).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataBlock {
    /// `?v { value* }`
    OneVar {
        /// The variable.
        var: Variable,
        /// The values.
        values: Vec<DataBlockValue>,
    },
    /// `( ?v* ) { ( value* )* }`; every row has one value per variable.
    Full {
        /// The variables.
        vars: Vec<Variable>,
        /// The rows.
        rows: Vec<Vec<DataBlockValue>>,
    },
}

impl WriteSparql for DataBlock {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::OneVar { var, values } => {
                var.write_sparql(w);
                w.token("{");
                for v in values {
                    v.write_sparql(w);
                }
                w.token("}");
            }
            Self::Full { vars, rows } => {
                write_parenthesized(w, vars);
                w.token("{");
                for row in rows {
                    write_parenthesized(w, row);
                }
                w.token("}");
            }
        }
    }
}

/// Writes `( a b c )`, or `()` (`NIL`) for an empty list.
fn write_parenthesized<T: WriteSparql>(w: &mut TokenWriter, items: &[T]) {
    if items.is_empty() {
        w.token("()");
        return;
    }
    w.open("(");
    for item in items {
        item.write_sparql(w);
    }
    w.glue(")");
}

// ---------------------------------------------------------------------------
// Triples with property paths (used in WHERE clauses)
// ---------------------------------------------------------------------------

/// `TriplesBlock` (production 55): triples separated by `.`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TriplesBlock {
    /// The triples; must not be empty.
    pub triples: Vec<TriplesSameSubjectPath>,
    /// Whether a final `.` is written after the last triple.
    pub trailing_dot: bool,
}

impl WriteSparql for TriplesBlock {
    fn write_sparql(&self, w: &mut TokenWriter) {
        for (i, t) in self.triples.iter().enumerate() {
            if i > 0 {
                w.token(".");
            }
            t.write_sparql(w);
        }
        if self.trailing_dot {
            w.token(".");
        }
    }
}

/// `TriplesSameSubjectPath` (production 81).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TriplesSameSubjectPath {
    /// A subject term with a non-empty property list.
    Subject {
        /// The subject.
        subject: VarOrTerm,
        /// The property list.
        properties: PropertyListPath,
    },
    /// A blank node property list or collection, with an optional property list.
    Node {
        /// The node.
        node: TriplesNodePath,
        /// The property list, if any.
        properties: Option<PropertyListPath>,
    },
}

impl WriteSparql for TriplesSameSubjectPath {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Subject {
                subject,
                properties,
            } => {
                subject.write_sparql(w);
                properties.write_sparql(w);
            }
            Self::Node { node, properties } => {
                node.write_sparql(w);
                if let Some(p) = properties {
                    p.write_sparql(w);
                }
            }
        }
    }
}

/// The verb of a triple pattern in a WHERE clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VerbPath {
    /// `VerbPath`: a property path (a single IRI is a trivial path).
    Path(Path),
    /// `VerbSimple`: a variable.
    Var(Variable),
}

impl WriteSparql for VerbPath {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Path(p) => p.write_sparql(w),
            Self::Var(v) => v.write_sparql(w),
        }
    }
}

/// `PropertyListPathNotEmpty` (production 83).
///
/// Note the grammar's asymmetry: only the *first* object list may contain
/// property-path nodes (`ObjectListPath`); the object lists after `;` use
/// `ObjectList`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyListPath {
    /// The first verb.
    pub verb: VerbPath,
    /// The first object list; must not be empty.
    pub objects: Vec<GraphNodePath>,
    /// Further `; verb objects` entries; `None` stands for an empty entry
    /// (the grammar allows repeated or trailing `;`).
    pub rest: Vec<Option<(VerbPath, Vec<GraphNode>)>>,
}

impl WriteSparql for PropertyListPath {
    fn write_sparql(&self, w: &mut TokenWriter) {
        self.verb.write_sparql(w);
        write_comma_separated(w, &self.objects);
        for entry in &self.rest {
            w.token(";");
            if let Some((verb, objects)) = entry {
                verb.write_sparql(w);
                write_comma_separated(w, objects);
            }
        }
    }
}

/// `GraphNodePath` (production 105).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphNodePath {
    /// A variable or term.
    Term(VarOrTerm),
    /// A blank node property list or collection.
    Node(Box<TriplesNodePath>),
}

impl WriteSparql for GraphNodePath {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Term(t) => t.write_sparql(w),
            Self::Node(n) => n.write_sparql(w),
        }
    }
}

/// `TriplesNodePath` (production 100).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TriplesNodePath {
    /// `( node+ )`
    Collection(Vec<GraphNodePath>),
    /// `[ property list ]`
    BlankNodePropertyList(PropertyListPath),
}

impl WriteSparql for TriplesNodePath {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Collection(items) => {
                w.open("(");
                for item in items {
                    item.write_sparql(w);
                }
                w.glue(")");
            }
            Self::BlankNodePropertyList(p) => {
                w.token("[");
                p.write_sparql(w);
                w.token("]");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property paths (productions 88–96)
// ---------------------------------------------------------------------------

/// `Path` / `PathAlternative`: sequences separated by `|`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path(pub Vec<PathSequence>);

/// `PathSequence`: elements separated by `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathSequence(pub Vec<PathEltOrInverse>);

/// `PathEltOrInverse`: an element, optionally inverted with `^`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathEltOrInverse {
    /// Whether the element is prefixed with `^`.
    pub inverse: bool,
    /// The element.
    pub elt: PathElt,
}

/// `PathElt`: a primary with an optional modifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathElt {
    /// The primary.
    pub primary: PathPrimary,
    /// The modifier, if any.
    pub modifier: Option<PathMod>,
}

/// `PathMod` (production 93).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathMod {
    /// `?`
    ZeroOrOne,
    /// `*`
    ZeroOrMore,
    /// `+`
    OneOrMore,
}

/// `PathPrimary` (production 94).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathPrimary {
    /// An IRI.
    Iri(Iri),
    /// The keyword `a` (`rdf:type`).
    A,
    /// `!` followed by a negated property set.
    Negated(Vec<PathOneInPropertySet>, NegatedSetStyle),
    /// `( path )`
    Group(Box<Path>),
}

/// How a negated property set is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NegatedSetStyle {
    /// `!iri` — only valid for exactly one member.
    Single,
    /// `!( a | b )`, also valid for zero or one member.
    Parenthesized,
}

/// `PathOneInPropertySet` (production 96).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathOneInPropertySet {
    /// Whether the member is prefixed with `^`.
    pub inverse: bool,
    /// The IRI, or `None` for the keyword `a`.
    pub iri: Option<Iri>,
}

impl Path {
    /// A path consisting of a single IRI.
    #[must_use]
    pub fn iri(iri: Iri) -> Self {
        Self::from_primary(PathPrimary::Iri(iri))
    }

    /// A path consisting of a single primary.
    #[must_use]
    pub fn from_primary(primary: PathPrimary) -> Self {
        Self(vec![PathSequence(vec![PathEltOrInverse {
            inverse: false,
            elt: PathElt {
                primary,
                modifier: None,
            },
        }])])
    }

    /// Returns the IRI or `a` if this path is a plain predicate.
    #[must_use]
    pub fn as_predicate(&self) -> Option<&PathPrimary> {
        match self.0.as_slice() {
            [PathSequence(seq)] => match seq.as_slice() {
                [
                    PathEltOrInverse {
                        inverse: false,
                        elt:
                            PathElt {
                                primary: p @ (PathPrimary::Iri(_) | PathPrimary::A),
                                modifier: None,
                            },
                    },
                ] => Some(p),
                _ => None,
            },
            _ => None,
        }
    }

    /// Writes the path into a single compact token (no inner whitespace), so
    /// that modifiers are attached to their primary: `ex:p*`.
    fn compact(&self) -> String {
        let mut out = String::new();
        for (i, seq) in self.0.iter().enumerate() {
            if i > 0 {
                out.push('|');
            }
            for (j, e) in seq.0.iter().enumerate() {
                if j > 0 {
                    out.push('/');
                }
                if e.inverse {
                    out.push('^');
                }
                out.push_str(&e.elt.primary.compact());
                if let Some(m) = e.elt.modifier {
                    out.push(match m {
                        PathMod::ZeroOrOne => '?',
                        PathMod::ZeroOrMore => '*',
                        PathMod::OneOrMore => '+',
                    });
                }
            }
        }
        out
    }
}

impl PathPrimary {
    fn compact(&self) -> String {
        match self {
            Self::Iri(i) => i.to_sparql(),
            Self::A => "a".to_owned(),
            Self::Negated(set, style) => {
                let members: Vec<String> = set.iter().map(PathOneInPropertySet::compact).collect();
                match (style, members.as_slice()) {
                    (NegatedSetStyle::Single, [one]) => format!("!{one}"),
                    _ => format!("!({})", members.join("|")),
                }
            }
            Self::Group(p) => format!("({})", p.compact()),
        }
    }
}

impl PathOneInPropertySet {
    fn compact(&self) -> String {
        let target = self
            .iri
            .as_ref()
            .map_or_else(|| "a".to_owned(), WriteSparql::to_sparql);
        if self.inverse {
            format!("^{target}")
        } else {
            target
        }
    }
}

impl WriteSparql for Path {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token(&self.compact());
    }
}

// ---------------------------------------------------------------------------
// Triples without property paths (templates, and objects after `;`)
// ---------------------------------------------------------------------------

/// `Verb` (production 78).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Verb {
    /// A variable or IRI.
    VarOrIri(VarOrIri),
    /// The keyword `a`.
    A,
}

impl WriteSparql for Verb {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::VarOrIri(v) => v.write_sparql(w),
            Self::A => w.token("a"),
        }
    }
}

/// `TriplesSameSubject` (production 75).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TriplesSameSubject {
    /// A subject term with a non-empty property list.
    Subject {
        /// The subject.
        subject: VarOrTerm,
        /// The property list.
        properties: PropertyList,
    },
    /// A blank node property list or collection, with an optional property list.
    Node {
        /// The node.
        node: TriplesNode,
        /// The property list, if any.
        properties: Option<PropertyList>,
    },
}

impl WriteSparql for TriplesSameSubject {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Subject {
                subject,
                properties,
            } => {
                subject.write_sparql(w);
                properties.write_sparql(w);
            }
            Self::Node { node, properties } => {
                node.write_sparql(w);
                if let Some(p) = properties {
                    p.write_sparql(w);
                }
            }
        }
    }
}

/// `PropertyListNotEmpty` (production 77).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyList {
    /// The first verb.
    pub verb: Verb,
    /// The first object list; must not be empty.
    pub objects: Vec<GraphNode>,
    /// Further `; verb objects` entries; `None` stands for an empty entry.
    pub rest: Vec<Option<(Verb, Vec<GraphNode>)>>,
}

impl WriteSparql for PropertyList {
    fn write_sparql(&self, w: &mut TokenWriter) {
        self.verb.write_sparql(w);
        write_comma_separated(w, &self.objects);
        for entry in &self.rest {
            w.token(";");
            if let Some((verb, objects)) = entry {
                verb.write_sparql(w);
                write_comma_separated(w, objects);
            }
        }
    }
}

/// `GraphNode` (production 104).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphNode {
    /// A variable or term.
    Term(VarOrTerm),
    /// A blank node property list or collection.
    Node(Box<TriplesNode>),
}

impl WriteSparql for GraphNode {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Term(t) => t.write_sparql(w),
            Self::Node(n) => n.write_sparql(w),
        }
    }
}

/// `TriplesNode` (production 98).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TriplesNode {
    /// `( node+ )`
    Collection(Vec<GraphNode>),
    /// `[ property list ]`
    BlankNodePropertyList(PropertyList),
}

impl WriteSparql for TriplesNode {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Collection(items) => {
                w.open("(");
                for item in items {
                    item.write_sparql(w);
                }
                w.glue(")");
            }
            Self::BlankNodePropertyList(p) => {
                w.token("[");
                p.write_sparql(w);
                w.token("]");
            }
        }
    }
}

/// `TriplesTemplate` / `ConstructTriples`: triples separated by `.`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TriplesTemplate {
    /// The triples; may be empty where the grammar makes the template optional.
    pub triples: Vec<TriplesSameSubject>,
    /// Whether a final `.` is written (only if `triples` is not empty).
    pub trailing_dot: bool,
}

impl WriteSparql for TriplesTemplate {
    fn write_sparql(&self, w: &mut TokenWriter) {
        for (i, t) in self.triples.iter().enumerate() {
            if i > 0 {
                w.token(".");
            }
            t.write_sparql(w);
        }
        if self.trailing_dot && !self.triples.is_empty() {
            w.token(".");
        }
    }
}

display_via_sparql!(
    GroupGraphPattern,
    GroupElement,
    GraphPatternNotTriples,
    Constraint,
    DataBlock,
    TriplesBlock,
    TriplesSameSubjectPath,
    Path,
    TriplesSameSubject,
    TriplesTemplate,
);

#[cfg(test)]
mod tests {
    use super::super::term::{PrefixedName, Variable};
    use super::*;

    fn iri(local: &str) -> Iri {
        Iri::Prefixed(PrefixedName::new("ex", local).unwrap())
    }

    fn var(name: &str) -> VarOrTerm {
        VarOrTerm::Var(Variable::new(name).unwrap())
    }

    #[test]
    fn paths_are_compact() {
        let p = Path(vec![
            PathSequence(vec![
                PathEltOrInverse {
                    inverse: true,
                    elt: PathElt {
                        primary: PathPrimary::Iri(iri("p")),
                        modifier: Some(PathMod::ZeroOrMore),
                    },
                },
                PathEltOrInverse {
                    inverse: false,
                    elt: PathElt {
                        primary: PathPrimary::Negated(
                            vec![
                                PathOneInPropertySet {
                                    inverse: false,
                                    iri: None,
                                },
                                PathOneInPropertySet {
                                    inverse: true,
                                    iri: Some(iri("q")),
                                },
                            ],
                            NegatedSetStyle::Parenthesized,
                        ),
                        modifier: None,
                    },
                },
            ]),
            PathSequence(vec![PathEltOrInverse {
                inverse: false,
                elt: PathElt {
                    primary: PathPrimary::A,
                    modifier: Some(PathMod::ZeroOrOne),
                },
            }]),
        ]);
        assert_eq!(p.to_sparql(), "^ex:p*/!(a|^ex:q)|a?");
    }

    #[test]
    fn adjacent_triples_blocks_are_separated() {
        let block = |trailing_dot| {
            GroupElement::Triples(TriplesBlock {
                triples: vec![TriplesSameSubjectPath::Subject {
                    subject: var("s"),
                    properties: PropertyListPath {
                        verb: VerbPath::Path(Path::iri(iri("p"))),
                        objects: vec![GraphNodePath::Term(var("o"))],
                        rest: vec![None],
                    },
                }],
                trailing_dot,
            })
        };
        let g = GroupGraphPattern::Group(vec![block(false), block(true), block(false)]);
        assert_eq!(
            g.to_sparql(),
            "{ ?s ex:p ?o ; . ?s ex:p ?o ; . ?s ex:p ?o ; }"
        );
    }
}
