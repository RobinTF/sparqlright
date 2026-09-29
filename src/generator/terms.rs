//! Conversion of RDF terms into their (randomly chosen) surface syntax.

use oxrdf::vocab::xsd;
use oxrdf::{NamedNodeRef, Term};
use rand::RngExt;
use rand_chacha::ChaCha8Rng;

use super::context::Gen;
use crate::ast::{
    GraphTerm, Iri, IriRef, LanguageTag, Literal, LiteralAnnotation, NumericKind, NumericLiteral,
    PrefixedName, QuoteStyle, RdfLiteral, StringLiteral,
};
use crate::provider::{TermKinds, ValueType};

/// The namespace of fallback IRIs.
const FALLBACK_NS: &str = "http://example.org/sparqlright/";

/// Whether a term can be written in a query.
pub(super) fn is_representable(term: &Term) -> bool {
    match term {
        Term::NamedNode(n) => IriRef::new(n.as_str()).is_ok(),
        Term::BlankNode(_) => false,
        Term::Literal(l) => {
            StringLiteral::new(l.value(), QuoteStyle::Double).is_ok()
                && l.language().is_none_or(|tag| LanguageTag::new(tag).is_ok())
                && IriRef::new(l.datatype().as_str()).is_ok()
        }
    }
}

/// A term used when no provider suggestion is usable.
pub(super) fn fallback_term(rng: &mut ChaCha8Rng, accepts: TermKinds, expected: ValueType) -> Term {
    use oxrdf::{Literal as RdfLiteral, NamedNode};
    let wants_iri = matches!(expected, ValueType::Iri | ValueType::Any);
    if accepts.contains(TermKinds::IRI) && (wants_iri || !accepts.contains(TermKinds::LITERAL)) {
        let n = rng.random_range(0..4);
        return Term::NamedNode(NamedNode::new_unchecked(format!("{FALLBACK_NS}{n}")));
    }
    let typed = |v: &str, dt| Term::Literal(RdfLiteral::new_typed_literal(v, dt));
    match expected {
        ValueType::Numeric => typed("1", xsd::INTEGER),
        ValueType::Boolean => typed("true", xsd::BOOLEAN),
        ValueType::DateTime => typed("2000-01-01T00:00:00Z", xsd::DATE_TIME),
        ValueType::String => {
            Term::Literal(RdfLiteral::new_language_tagged_literal_unchecked("a", "en"))
        }
        ValueType::SimpleString | ValueType::Literal | ValueType::Any | ValueType::Iri => {
            Term::Literal(RdfLiteral::new_simple_literal("a"))
        }
    }
}

impl Gen<'_> {
    /// Writes an IRI as prefixed name, relative IRI or full IRI.
    pub fn iri(&mut self, iri: NamedNodeRef<'_>) -> Iri {
        let s = iri.as_str();
        if self.flip(self.w.prologue.p_prefixed_name)
            && let Some(p) = self.compact(s)
        {
            return Iri::Prefixed(p);
        }
        if let Some(base) = &self.base
            && let Some(rest) = s.strip_prefix(base.as_str())
            && !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && self.flip(self.w.prologue.p_relative_iri)
            && let Ok(r) = IriRef::new(rest)
        {
            return Iri::Ref(r);
        }
        // Callers only pass representable IRIs.
        IriRef::new(s).map_or_else(
            |_| Iri::Ref(IriRef::new(FALLBACK_NS).expect("valid IRI")),
            Iri::Ref,
        )
    }

    fn compact(&self, iri: &str) -> Option<PrefixedName> {
        self.prefixes
            .iter()
            .filter(|(_, ns)| iri.starts_with(ns.as_str()))
            .max_by_key(|(_, ns)| ns.len())
            .and_then(|(prefix, ns)| {
                let local = PrefixedName::escape_local(&iri[ns.len()..])?;
                PrefixedName::new(prefix.clone(), local).ok()
            })
    }

    /// Picks a quote style and wraps `value`.
    pub fn string(&mut self, value: &str) -> StringLiteral {
        let lex = &self.w.lexical;
        let style = self
            .choose(&[
                (QuoteStyle::Double, lex.double_quote),
                (QuoteStyle::Single, lex.single_quote),
                (QuoteStyle::LongDouble, lex.long_double_quote),
                (QuoteStyle::LongSingle, lex.long_single_quote),
            ])
            .unwrap_or_default();
        StringLiteral::new(value, style)
            .or_else(|_| StringLiteral::new(value.replace('\\', "/"), style))
            .expect("backslashes removed")
    }

    /// Writes a literal, using numeric and boolean shorthands where possible.
    pub fn literal(&mut self, literal: &oxrdf::Literal) -> Literal {
        let dt = literal.datatype();
        let value = literal.value();
        let shorthand_kind = if dt == xsd::INTEGER {
            Some(NumericKind::Integer)
        } else if dt == xsd::DECIMAL {
            Some(NumericKind::Decimal)
        } else if dt == xsd::DOUBLE {
            Some(NumericKind::Double)
        } else {
            None
        };
        if let Some(kind) = shorthand_kind
            && self.flip(self.w.lexical.p_numeric_shorthand)
            && let Ok(n) = NumericLiteral::new(value)
            && n.kind() == kind
        {
            return Literal::Numeric(n);
        }
        if dt == xsd::BOOLEAN
            && matches!(value, "true" | "false")
            && self.flip(self.w.lexical.p_boolean_shorthand)
        {
            return Literal::Boolean(value == "true");
        }
        let string = self.string(value);
        let annotation = if let Some(tag) = literal.language() {
            LanguageTag::new(tag).ok().map(LiteralAnnotation::Language)
        } else if dt == xsd::STRING {
            self.flip(self.w.lexical.p_explicit_string_type)
                .then(|| LiteralAnnotation::Datatype(self.iri(xsd::STRING)))
        } else {
            Some(LiteralAnnotation::Datatype(self.iri(dt)))
        };
        Literal::Rdf(RdfLiteral {
            value: string,
            annotation,
        })
    }

    /// Writes an IRI or literal; blank nodes are not expected here.
    pub fn graph_term(&mut self, term: &Term) -> GraphTerm {
        match term {
            Term::NamedNode(n) => GraphTerm::Iri(self.iri(n.as_ref())),
            Term::Literal(l) => GraphTerm::Literal(self.literal(l)),
            Term::BlankNode(_) => GraphTerm::BlankNode(crate::ast::BlankNode::Anon),
        }
    }

    /// Writes an integer as numeric literal.
    pub fn integer(value: i64) -> Literal {
        Literal::Numeric(NumericLiteral::new(value.to_string()).expect("integers are valid"))
    }
}
