//! SPARQL 1.1 Update requests (productions 29–52).

use super::pattern::{GroupGraphPattern, TriplesTemplate};
use super::query::Prologue;
use super::term::{Iri, VarOrIri};
use super::writer::{TokenWriter, WriteSparql, display_via_sparql};

/// A complete update request (`Update`, production 29).
///
/// The grammar is `Prologue ( Update1 ( ';' Update )? )?`: every operation
/// may be preceded by its own prologue, and the request may end with `;`
/// followed by a (possibly empty) prologue.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Update {
    /// The operations with their preceding prologues.
    pub operations: Vec<(Prologue, UpdateOperation)>,
    /// A final `; prologue` after the last operation, or the only prologue
    /// of an empty request.
    pub tail: Option<Prologue>,
}

impl WriteSparql for Update {
    fn write_sparql(&self, w: &mut TokenWriter) {
        for (i, (prologue, op)) in self.operations.iter().enumerate() {
            if i > 0 {
                w.token(";");
            }
            prologue.write_sparql(w);
            op.write_sparql(w);
        }
        if let Some(tail) = &self.tail {
            if !self.operations.is_empty() {
                w.token(";");
            }
            tail.write_sparql(w);
        }
    }
}

/// `GraphRefAll` (production 47).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphRefAll {
    /// `GRAPH <iri>`
    Graph(Iri),
    /// `DEFAULT`
    Default,
    /// `NAMED`
    Named,
    /// `ALL`
    All,
}

impl WriteSparql for GraphRefAll {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Graph(i) => {
                w.token("GRAPH");
                i.write_sparql(w);
            }
            Self::Default => w.token("DEFAULT"),
            Self::Named => w.token("NAMED"),
            Self::All => w.token("ALL"),
        }
    }
}

/// `GraphOrDefault` (production 45).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphOrDefault {
    /// `DEFAULT`
    Default,
    /// `GRAPH? <iri>`
    Graph {
        /// Whether the `GRAPH` keyword is written.
        keyword: bool,
        /// The graph.
        iri: Iri,
    },
}

impl WriteSparql for GraphOrDefault {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Default => w.token("DEFAULT"),
            Self::Graph { keyword, iri } => {
                if *keyword {
                    w.token("GRAPH");
                }
                iri.write_sparql(w);
            }
        }
    }
}

/// An element of `Quads` (production 50).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuadsElement {
    /// Triples in the default graph.
    Triples(TriplesTemplate),
    /// `GRAPH ?g { triples? }` (`QuadsNotTriples`), optionally followed by `.`.
    Graph {
        /// The graph.
        graph: VarOrIri,
        /// The triples.
        triples: TriplesTemplate,
        /// Whether the optional `.` is written.
        dot: bool,
    },
}

/// `QuadPattern` / `QuadData` (productions 48–49): `{ Quads }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Quads(pub Vec<QuadsElement>);

impl WriteSparql for Quads {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token("{");
        let mut needs_dot = false;
        for element in &self.0 {
            match element {
                QuadsElement::Triples(t) => {
                    if needs_dot {
                        w.token(".");
                    }
                    t.write_sparql(w);
                    needs_dot = !t.trailing_dot && !t.triples.is_empty();
                }
                QuadsElement::Graph {
                    graph,
                    triples,
                    dot,
                } => {
                    w.token("GRAPH");
                    graph.write_sparql(w);
                    w.token("{");
                    triples.write_sparql(w);
                    w.token("}");
                    if *dot {
                        w.token(".");
                    }
                    needs_dot = false;
                }
            }
        }
        w.token("}");
    }
}

/// `UsingClause` (production 44).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UsingClause {
    /// `USING <iri>`
    Default(Iri),
    /// `USING NAMED <iri>`
    Named(Iri),
}

/// The kind of a graph management operation that copies between graphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferKind {
    /// `ADD`
    Add,
    /// `MOVE`
    Move,
    /// `COPY`
    Copy,
}

/// `Update1` (production 30).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UpdateOperation {
    /// `LOAD SILENT? <iri> (INTO GRAPH <iri>)?`
    Load {
        /// Whether `SILENT` is given.
        silent: bool,
        /// The document to load.
        source: Iri,
        /// The target graph.
        into: Option<Iri>,
    },
    /// `CLEAR SILENT? target`
    Clear {
        /// Whether `SILENT` is given.
        silent: bool,
        /// The cleared graphs.
        target: GraphRefAll,
    },
    /// `DROP SILENT? target`
    Drop {
        /// Whether `SILENT` is given.
        silent: bool,
        /// The dropped graphs.
        target: GraphRefAll,
    },
    /// `CREATE SILENT? GRAPH <iri>`
    Create {
        /// Whether `SILENT` is given.
        silent: bool,
        /// The created graph.
        graph: Iri,
    },
    /// `ADD|MOVE|COPY SILENT? from TO to`
    Transfer {
        /// The operation.
        kind: TransferKind,
        /// Whether `SILENT` is given.
        silent: bool,
        /// The source graph.
        from: GraphOrDefault,
        /// The target graph.
        to: GraphOrDefault,
    },
    /// `INSERT DATA { quads }` (no variables).
    InsertData(Quads),
    /// `DELETE DATA { quads }` (no variables, no blank nodes).
    DeleteData(Quads),
    /// `DELETE WHERE { quads }` (no blank nodes).
    DeleteWhere(Quads),
    /// `WITH? DELETE? INSERT? USING* WHERE pattern`
    Modify {
        /// The `WITH` graph.
        with: Option<Iri>,
        /// The `DELETE` template (no blank nodes).
        delete: Option<Quads>,
        /// The `INSERT` template.
        insert: Option<Quads>,
        /// The `USING` clauses.
        using: Vec<UsingClause>,
        /// The pattern.
        pattern: GroupGraphPattern,
    },
}

impl WriteSparql for UpdateOperation {
    fn write_sparql(&self, w: &mut TokenWriter) {
        let silent = |w: &mut TokenWriter, s: bool| {
            if s {
                w.token("SILENT");
            }
        };
        match self {
            Self::Load {
                silent: s,
                source,
                into,
            } => {
                w.token("LOAD");
                silent(w, *s);
                source.write_sparql(w);
                if let Some(g) = into {
                    w.token("INTO");
                    w.token("GRAPH");
                    g.write_sparql(w);
                }
            }
            Self::Clear { silent: s, target } | Self::Drop { silent: s, target } => {
                w.token(if matches!(self, Self::Clear { .. }) {
                    "CLEAR"
                } else {
                    "DROP"
                });
                silent(w, *s);
                target.write_sparql(w);
            }
            Self::Create { silent: s, graph } => {
                w.token("CREATE");
                silent(w, *s);
                w.token("GRAPH");
                graph.write_sparql(w);
            }
            Self::Transfer {
                kind,
                silent: s,
                from,
                to,
            } => {
                w.token(match kind {
                    TransferKind::Add => "ADD",
                    TransferKind::Move => "MOVE",
                    TransferKind::Copy => "COPY",
                });
                silent(w, *s);
                from.write_sparql(w);
                w.token("TO");
                to.write_sparql(w);
            }
            Self::InsertData(q) => {
                w.token("INSERT DATA");
                q.write_sparql(w);
            }
            Self::DeleteData(q) => {
                w.token("DELETE DATA");
                q.write_sparql(w);
            }
            Self::DeleteWhere(q) => {
                w.token("DELETE WHERE");
                q.write_sparql(w);
            }
            Self::Modify {
                with,
                delete,
                insert,
                using,
                pattern,
            } => {
                if let Some(g) = with {
                    w.token("WITH");
                    g.write_sparql(w);
                }
                if let Some(d) = delete {
                    w.token("DELETE");
                    d.write_sparql(w);
                }
                if let Some(i) = insert {
                    w.token("INSERT");
                    i.write_sparql(w);
                }
                for u in using {
                    w.token("USING");
                    match u {
                        UsingClause::Default(i) => i.write_sparql(w),
                        UsingClause::Named(i) => {
                            w.token("NAMED");
                            i.write_sparql(w);
                        }
                    }
                }
                w.token("WHERE");
                pattern.write_sparql(w);
            }
        }
    }
}

display_via_sparql!(Update, UpdateOperation, Quads);
