//! SPARQL Update requests.

use super::context::{Gen, Scope, Value};
use super::pattern::GroupKind;
use crate::ast::{
    GraphOrDefault, GraphRefAll, Iri, Prologue, Quads, QuadsElement, TransferKind, Update,
    UpdateOperation, UsingClause, VarOrIri,
};
use crate::provider::{Slot, TermKinds, ValueType};

/// What quads may contain.
#[derive(Debug, Clone, Copy)]
struct QuadsMode {
    vars: bool,
    bnodes: bool,
}

impl Gen<'_> {
    /// Generates a complete update request.
    pub fn update(&mut self) -> Update {
        self.hit("Update");
        let prologue = self.prologue();
        let n = self.count(
            self.w.update.p_more_operations,
            self.limits.max_update_operations.max(1),
        );
        let mut operations = Vec::new();
        for i in 0..n {
            if i > 0 {
                self.out.token(";");
            }
            let p = if i == 0 {
                prologue.clone()
            } else {
                Prologue::default()
            };
            let op = self.update_operation();
            operations.push((p, op));
        }
        let tail = if self.flip(0.05) {
            self.out.token(";");
            Some(Prologue::default())
        } else {
            None
        };
        Update { operations, tail }
    }

    fn update_operation(&mut self) -> UpdateOperation {
        let u = &self.w.update;
        let kind = self
            .choose(&[
                (0, u.insert_data),
                (1, u.delete_data),
                (2, u.delete_where),
                (3, u.modify),
                (4, u.load),
                (5, u.clear),
                (6, u.drop),
                (7, u.create),
                (8, u.transfer),
            ])
            .unwrap_or(0);
        match kind {
            0 => {
                self.hit("InsertData");
                self.out.token("INSERT DATA");
                UpdateOperation::InsertData(self.quads(QuadsMode {
                    vars: false,
                    bnodes: true,
                }))
            }
            1 => {
                self.hit("DeleteData");
                self.out.token("DELETE DATA");
                UpdateOperation::DeleteData(self.quads(QuadsMode {
                    vars: false,
                    bnodes: false,
                }))
            }
            2 => {
                self.hit("DeleteWhere");
                self.out.token("DELETE WHERE");
                self.scopes.push(Scope::default());
                let quads = self.quads(QuadsMode {
                    vars: true,
                    bnodes: false,
                });
                self.scopes.pop();
                UpdateOperation::DeleteWhere(quads)
            }
            3 => self.modify(),
            4 => self.load(),
            5 => self.clear_or_drop(true),
            6 => self.clear_or_drop(false),
            7 => self.create(),
            _ => self.transfer(),
        }
    }

    fn load(&mut self) -> UpdateOperation {
        self.hit("Load");
        self.out.token("LOAD");
        let silent = self.silent();
        let source = self.graph_iri();
        let into = if self.flip(0.5) {
            self.out.token("INTO");
            self.out.token("GRAPH");
            Some(self.graph_iri())
        } else {
            None
        };
        UpdateOperation::Load {
            silent,
            source,
            into,
        }
    }

    fn clear_or_drop(&mut self, clear: bool) -> UpdateOperation {
        self.hit(if clear { "Clear" } else { "Drop" });
        self.out.token(if clear { "CLEAR" } else { "DROP" });
        let silent = self.silent();
        let target = match self
            .choose(&[(0, 3.0), (1, 1.0), (2, 0.5), (3, 0.5)])
            .unwrap_or(0)
        {
            0 => {
                self.out.token("GRAPH");
                GraphRefAll::Graph(self.graph_iri())
            }
            1 => {
                self.out.token("DEFAULT");
                GraphRefAll::Default
            }
            2 => {
                self.out.token("NAMED");
                GraphRefAll::Named
            }
            _ => {
                self.out.token("ALL");
                GraphRefAll::All
            }
        };
        if clear {
            UpdateOperation::Clear { silent, target }
        } else {
            UpdateOperation::Drop { silent, target }
        }
    }

    fn create(&mut self) -> UpdateOperation {
        self.hit("Create");
        self.out.token("CREATE");
        let silent = self.silent();
        self.out.token("GRAPH");
        let graph = self.graph_iri();
        UpdateOperation::Create { silent, graph }
    }

    fn transfer(&mut self) -> UpdateOperation {
        let kind = *self
            .pick(&[TransferKind::Add, TransferKind::Move, TransferKind::Copy])
            .unwrap_or(&TransferKind::Add);
        self.hit("Transfer");
        self.out.token(match kind {
            TransferKind::Add => "ADD",
            TransferKind::Move => "MOVE",
            TransferKind::Copy => "COPY",
        });
        let silent = self.silent();
        let from = self.graph_or_default();
        self.out.token("TO");
        let to = self.graph_or_default();
        UpdateOperation::Transfer {
            kind,
            silent,
            from,
            to,
        }
    }

    fn silent(&mut self) -> bool {
        let silent = self.flip(self.w.update.p_silent);
        if silent {
            self.out.token("SILENT");
        }
        silent
    }

    fn graph_iri(&mut self) -> Iri {
        let value = self.value(Slot::GraphName, TermKinds::IRI, ValueType::Iri, None, None);
        let term = match value {
            Value::Term(t) => t,
            Value::Var(_) => oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                "http://example.org/sparqlright/graph",
            )),
        };
        let iri = self.iri_of(&term);
        self.emit(&iri);
        iri
    }

    fn graph_or_default(&mut self) -> GraphOrDefault {
        if self.flip(0.3) {
            self.out.token("DEFAULT");
            return GraphOrDefault::Default;
        }
        let keyword = self.flip(0.5);
        if keyword {
            self.out.token("GRAPH");
        }
        let iri = self.graph_iri();
        GraphOrDefault::Graph { keyword, iri }
    }

    fn modify(&mut self) -> UpdateOperation {
        self.hit("Modify");
        let with = if self.flip(self.w.update.p_with) {
            self.out.token("WITH");
            Some(self.graph_iri())
        } else {
            None
        };
        let (delete, insert) = match self.choose(&[(0, 1.0), (1, 1.0), (2, 1.0)]).unwrap_or(2) {
            0 => (true, false),
            1 => (false, true),
            _ => (true, true),
        };
        self.push_pending();
        let saved = self.env.clone();
        self.env.planning = true;
        let mark = self.context.len();
        let delete = delete.then(|| {
            self.out.token("DELETE");
            self.quads(QuadsMode {
                vars: true,
                bnodes: false,
            })
        });
        let insert = insert.then(|| {
            self.out.token("INSERT");
            self.quads(QuadsMode {
                vars: true,
                bnodes: true,
            })
        });
        self.truncate_context(mark);
        self.env = saved;
        let mut using = Vec::new();
        while using.len() < 2 && self.coin(self.w.update.p_using) {
            self.out.token("USING");
            if self.flip(0.5) {
                self.out.token("NAMED");
                using.push(UsingClause::Named(self.graph_iri()));
            } else {
                using.push(UsingClause::Default(self.graph_iri()));
            }
        }
        self.out.token("WHERE");
        let (pattern, _) = self.group(GroupKind::Plain);
        self.pop_pending();
        UpdateOperation::Modify {
            with,
            delete,
            insert,
            using,
            pattern,
        }
    }

    fn quads(&mut self, mode: QuadsMode) -> Quads {
        self.hit("Quads");
        let (saved_vars, saved_bnodes) = (self.allow_vars, self.allow_bnodes);
        self.allow_vars = mode.vars;
        self.allow_bnodes = mode.bnodes;
        self.out.token("{");
        let n = self.count(0.5, 3);
        let mut elements = Vec::new();
        let mut needs_dot = false;
        let mut previous_triples = false;
        for _ in 0..n {
            if !previous_triples && !self.flip(self.w.update.p_graph_quads) {
                if needs_dot {
                    self.out.token(".");
                }
                let t = self.triples_template(self.limits.max_triples);
                needs_dot = !t.trailing_dot && !t.triples.is_empty();
                previous_triples = true;
                elements.push(QuadsElement::Triples(t));
            } else {
                self.hit("QuadsNotTriples");
                self.out.token("GRAPH");
                let accepts = if mode.vars {
                    TermKinds::VARIABLE | TermKinds::IRI
                } else {
                    TermKinds::IRI
                };
                let graph = match self.value(Slot::GraphName, accepts, ValueType::Iri, None, None) {
                    Value::Var(v) => {
                        self.bind(&v, true);
                        VarOrIri::Var(v)
                    }
                    Value::Term(t) => VarOrIri::Iri(self.iri_of(&t)),
                };
                self.emit(&graph);
                self.out.token("{");
                let triples = if self.flip(0.1) {
                    crate::ast::TriplesTemplate::default()
                } else {
                    self.triples_template(self.limits.max_triples)
                };
                self.out.token("}");
                let dot = self.flip(0.3);
                if dot {
                    self.out.token(".");
                }
                needs_dot = false;
                previous_triples = false;
                elements.push(QuadsElement::Graph {
                    graph,
                    triples,
                    dot,
                });
            }
        }
        self.out.token("}");
        self.allow_vars = saved_vars;
        self.allow_bnodes = saved_bnodes;
        Quads(elements)
    }
}
