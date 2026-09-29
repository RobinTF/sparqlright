//! Terminals of the grammar: variables, IRIs, literals and blank nodes.
//!
//! The types in this module keep the *surface form* of a term (for example
//! whether an IRI is written as `<...>` or as a prefixed name, or which quote
//! style a string uses), because covering these alternatives is part of
//! exercising the grammar. Constructors validate their input against the
//! corresponding terminal of the SPARQL 1.1 grammar.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use super::writer::{TokenWriter, WriteSparql, display_via_sparql};

/// Error returned when a string is not a valid instance of a grammar terminal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {terminal}: {value:?}")]
pub struct InvalidTerminal {
    /// The name of the grammar terminal, e.g. `VARNAME`.
    pub terminal: &'static str,
    /// The rejected input.
    pub value: String,
}

impl InvalidTerminal {
    fn new(terminal: &'static str, value: &str) -> Self {
        Self {
            terminal,
            value: value.to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// Character classes of the grammar (productions [164]–[173]).
// ---------------------------------------------------------------------------

/// `PN_CHARS_BASE` (production 164).
pub(crate) fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z'
        | '\u{00C0}'..='\u{00D6}' | '\u{00D8}'..='\u{00F6}' | '\u{00F8}'..='\u{02FF}'
        | '\u{0370}'..='\u{037D}' | '\u{037F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// `PN_CHARS_U` (production 165).
pub(crate) fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}

/// `PN_CHARS` (production 167).
pub(crate) fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{00B7}'
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

/// Characters that may be escaped with a backslash in a local name (`PN_LOCAL_ESC`).
fn is_pn_local_esc(c: char) -> bool {
    matches!(
        c,
        '_' | '~'
            | '.'
            | '-'
            | '!'
            | '$'
            | '&'
            | '\''
            | '('
            | ')'
            | '*'
            | '+'
            | ','
            | ';'
            | '='
            | '/'
            | '?'
            | '#'
            | '@'
            | '%'
    )
}

/// Checks `first rest*` where the last character must additionally satisfy `last`.
fn check_word(
    s: &str,
    first: impl Fn(char) -> bool,
    middle: impl Fn(char) -> bool,
    last: impl Fn(char) -> bool,
) -> bool {
    let mut chars = s.chars();
    let Some(c0) = chars.next() else {
        return false;
    };
    if !first(c0) {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    match rest.split_last() {
        None => true,
        Some((l, mid)) => mid.iter().all(|&c| middle(c)) && last(*l),
    }
}

fn is_varname(s: &str) -> bool {
    let tail = |c: char| {
        is_pn_chars_u(c)
            || c.is_ascii_digit()
            || c == '\u{00B7}'
            || ('\u{0300}'..='\u{036F}').contains(&c)
            || ('\u{203F}'..='\u{2040}').contains(&c)
    };
    check_word(s, |c| is_pn_chars_u(c) || c.is_ascii_digit(), tail, tail)
}

fn is_pn_prefix(s: &str) -> bool {
    check_word(
        s,
        is_pn_chars_base,
        |c| is_pn_chars(c) || c == '.',
        is_pn_chars,
    )
}

fn is_blank_node_label(s: &str) -> bool {
    check_word(
        s,
        |c| is_pn_chars_u(c) || c.is_ascii_digit(),
        |c| is_pn_chars(c) || c == '.',
        is_pn_chars,
    )
}

/// Checks the `PN_LOCAL` production (169) on an already escaped surface form.
fn is_pn_local(s: &str) -> bool {
    // Tokenize into units: plain chars, `%HH` and `\x` escapes.
    #[derive(Clone, Copy)]
    enum Unit {
        Char(char),
        Plx,
    }
    let chars: Vec<char> = s.chars().collect();
    let mut units = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '%' => {
                if chars.get(i + 1).is_some_and(char::is_ascii_hexdigit)
                    && chars.get(i + 2).is_some_and(char::is_ascii_hexdigit)
                {
                    units.push(Unit::Plx);
                    i += 3;
                } else {
                    return false;
                }
            }
            '\\' => {
                if chars.get(i + 1).is_some_and(|&c| is_pn_local_esc(c)) {
                    units.push(Unit::Plx);
                    i += 2;
                } else {
                    return false;
                }
            }
            c => {
                units.push(Unit::Char(c));
                i += 1;
            }
        }
    }
    let Some((first, rest)) = units.split_first() else {
        return true; // the empty local name is allowed (`PNAME_NS` alone)
    };
    let first_ok = match *first {
        Unit::Plx => true,
        Unit::Char(c) => is_pn_chars_u(c) || c == ':' || c.is_ascii_digit(),
    };
    if !first_ok {
        return false;
    }
    let Some((last, mid)) = rest.split_last() else {
        return true;
    };
    let mid_ok = mid.iter().all(|u| match *u {
        Unit::Plx => true,
        Unit::Char(c) => is_pn_chars(c) || c == '.' || c == ':',
    });
    let last_ok = match *last {
        Unit::Plx => true,
        Unit::Char(c) => is_pn_chars(c) || c == ':',
    };
    mid_ok && last_ok
}

/// Characters allowed inside `IRIREF` (production 139).
fn is_iriref_char(c: char) -> bool {
    !matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\') && c > '\u{20}'
}

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

/// The sigil used to write a variable (`VAR1` or `VAR2`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum VarSigil {
    /// `?name`
    #[default]
    Question,
    /// `$name`
    Dollar,
}

/// A query variable (`Var`, production 108).
///
/// Two variables are equal if their names are equal: `?x` and `$x` denote the
/// same variable, so equality, ordering and hashing ignore the sigil.
#[derive(Debug, Clone)]
pub struct Variable {
    name: String,
    sigil: VarSigil,
}

impl Variable {
    /// Creates a variable written with `?`.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` does not match `VARNAME`.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidTerminal> {
        Self::with_sigil(name, VarSigil::Question)
    }

    /// Creates a variable with an explicit sigil.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` does not match `VARNAME`.
    pub fn with_sigil(name: impl Into<String>, sigil: VarSigil) -> Result<Self, InvalidTerminal> {
        let name = name.into();
        if is_varname(&name) {
            Ok(Self { name, sigil })
        } else {
            Err(InvalidTerminal::new("VARNAME", &name))
        }
    }

    /// Creates a variable from a name known to be valid.
    pub(crate) fn new_unchecked(name: impl Into<String>) -> Self {
        let name = name.into();
        debug_assert!(is_varname(&name), "invalid variable name {name:?}");
        Self {
            name,
            sigil: VarSigil::Question,
        }
    }

    /// The name without sigil.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The sigil used when writing this variable.
    #[must_use]
    pub fn sigil(&self) -> VarSigil {
        self.sigil
    }

    /// Returns the same variable written with another sigil.
    #[must_use]
    pub fn with_other_sigil(&self, sigil: VarSigil) -> Self {
        Self {
            name: self.name.clone(),
            sigil,
        }
    }
}

impl PartialEq for Variable {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Variable {}

impl Hash for Variable {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl PartialOrd for Variable {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Variable {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}

/// Variables are serialized as their SPARQL token, e.g. `"?x"`.
impl serde::Serialize for Variable {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_sparql())
    }
}

impl<'de> serde::Deserialize<'de> for Variable {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = String::deserialize(deserializer)?;
        let (sigil, name) = if let Some(n) = token.strip_prefix('?') {
            (VarSigil::Question, n)
        } else if let Some(n) = token.strip_prefix('$') {
            (VarSigil::Dollar, n)
        } else {
            return Err(serde::de::Error::custom(format!(
                "not a variable: {token:?}"
            )));
        };
        Self::with_sigil(name, sigil).map_err(serde::de::Error::custom)
    }
}

impl WriteSparql for Variable {
    fn write_sparql(&self, w: &mut TokenWriter) {
        let sigil = match self.sigil {
            VarSigil::Question => '?',
            VarSigil::Dollar => '$',
        };
        w.token(&format!("{sigil}{}", self.name));
    }
}

// ---------------------------------------------------------------------------
// IRIs
// ---------------------------------------------------------------------------

/// An IRI written in angle brackets (`IRIREF`, production 139); may be relative.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IriRef(String);

impl IriRef {
    /// Creates an IRI reference from the text between the angle brackets.
    ///
    /// # Errors
    ///
    /// Returns an error if the text contains characters excluded by `IRIREF`.
    pub fn new(iri: impl Into<String>) -> Result<Self, InvalidTerminal> {
        let iri = iri.into();
        if iri.chars().all(is_iriref_char) {
            Ok(Self(iri))
        } else {
            Err(InvalidTerminal::new("IRIREF", &iri))
        }
    }

    /// The text between the angle brackets.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl WriteSparql for IriRef {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token(&format!("<{}>", self.0));
    }
}

/// A prefixed name (`PNAME_LN` or `PNAME_NS`, productions 140–141).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrefixedName {
    prefix: String,
    local: String,
}

impl PrefixedName {
    /// Creates a prefixed name from a prefix (possibly empty) and a local
    /// part in its *escaped surface form* (possibly empty).
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix is not a `PN_PREFIX` or the local part
    /// is not a `PN_LOCAL`.
    pub fn new(
        prefix: impl Into<String>,
        local: impl Into<String>,
    ) -> Result<Self, InvalidTerminal> {
        let prefix = prefix.into();
        let local = local.into();
        if !prefix.is_empty() && !is_pn_prefix(&prefix) {
            return Err(InvalidTerminal::new("PN_PREFIX", &prefix));
        }
        if !is_pn_local(&local) {
            return Err(InvalidTerminal::new("PN_LOCAL", &local));
        }
        Ok(Self { prefix, local })
    }

    /// Escapes a raw local name so that it forms a valid `PN_LOCAL`, if possible.
    ///
    /// `%` followed by two hex digits is kept as is (percent sequences are not
    /// decoded by prefix expansion); other reserved characters are escaped
    /// with a backslash. Returns `None` if the name contains characters that
    /// cannot appear in a local name at all.
    #[must_use]
    pub fn escape_local(raw: &str) -> Option<String> {
        let chars: Vec<char> = raw.chars().collect();
        let mut out = String::with_capacity(raw.len());
        for (i, &c) in chars.iter().enumerate() {
            let is_first = i == 0;
            let is_last = i + 1 == chars.len();
            if c == '%' {
                let hex = chars.get(i + 1).is_some_and(char::is_ascii_hexdigit)
                    && chars.get(i + 2).is_some_and(char::is_ascii_hexdigit);
                out.push_str(if hex { "%" } else { "\\%" });
                continue;
            }
            let raw_ok = if is_first {
                is_pn_chars_u(c) || c == ':' || c.is_ascii_digit()
            } else if is_last {
                is_pn_chars(c) || c == ':'
            } else {
                is_pn_chars(c) || c == '.' || c == ':'
            };
            if raw_ok {
                out.push(c);
            } else if is_pn_local_esc(c) {
                out.push('\\');
                out.push(c);
            } else {
                return None;
            }
        }
        debug_assert!(is_pn_local(&out), "escape_local produced {out:?}");
        Some(out)
    }

    /// The prefix (without colon); empty for the default prefix `:`.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The local part in its escaped surface form.
    #[must_use]
    pub fn local(&self) -> &str {
        &self.local
    }

    /// The local part with backslash escapes removed, as used in prefix expansion.
    #[must_use]
    pub fn unescaped_local(&self) -> String {
        let mut out = String::with_capacity(self.local.len());
        let mut chars = self.local.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(e) = chars.next() {
                    out.push(e);
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

impl WriteSparql for PrefixedName {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token(&format!("{}:{}", self.prefix, self.local));
    }
}

/// An IRI in either syntax (`iri`, production 136).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Iri {
    /// `<...>`
    Ref(IriRef),
    /// `prefix:local`
    Prefixed(PrefixedName),
}

impl WriteSparql for Iri {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Ref(i) => i.write_sparql(w),
            Self::Prefixed(p) => p.write_sparql(w),
        }
    }
}

impl From<IriRef> for Iri {
    fn from(value: IriRef) -> Self {
        Self::Ref(value)
    }
}

impl From<PrefixedName> for Iri {
    fn from(value: PrefixedName) -> Self {
        Self::Prefixed(value)
    }
}

// ---------------------------------------------------------------------------
// Literals
// ---------------------------------------------------------------------------

/// The quoting style of a string (`String`, production 135).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum QuoteStyle {
    /// `'...'` (`STRING_LITERAL1`)
    Single,
    /// `"..."` (`STRING_LITERAL2`)
    #[default]
    Double,
    /// `'''...'''` (`STRING_LITERAL_LONG1`)
    LongSingle,
    /// `"""..."""` (`STRING_LITERAL_LONG2`)
    LongDouble,
}

/// A string token with its (unescaped) value and quoting style.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StringLiteral {
    value: String,
    quote: QuoteStyle,
}

impl StringLiteral {
    /// Creates a string with the given unescaped value.
    ///
    /// # Errors
    ///
    /// Returns an error if the value contains a backslash followed by `u` or
    /// `U`. Such text is ambiguous with respect to the codepoint escape
    /// processing that happens *before* parsing (spec section 19.2), so
    /// engines legitimately disagree on it.
    pub fn new(value: impl Into<String>, quote: QuoteStyle) -> Result<Self, InvalidTerminal> {
        let value = value.into();
        if value.contains("\\u") || value.contains("\\U") {
            return Err(InvalidTerminal::new("String", &value));
        }
        Ok(Self { value, quote })
    }

    /// The unescaped value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The quoting style.
    #[must_use]
    pub fn quote(&self) -> QuoteStyle {
        self.quote
    }

    fn escaped(&self) -> String {
        let (quote_char, long) = match self.quote {
            QuoteStyle::Single => ('\'', false),
            QuoteStyle::Double => ('"', false),
            QuoteStyle::LongSingle => ('\'', true),
            QuoteStyle::LongDouble => ('"', true),
        };
        let delimiter: String = if long {
            std::iter::repeat_n(quote_char, 3).collect()
        } else {
            quote_char.to_string()
        };
        let mut out = String::with_capacity(self.value.len() + 6);
        out.push_str(&delimiter);
        for c in self.value.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' if !long => out.push_str("\\n"),
                '\r' if !long => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if c == quote_char => {
                    out.push('\\');
                    out.push(c);
                }
                c => out.push(c),
            }
        }
        out.push_str(&delimiter);
        out
    }
}

impl WriteSparql for StringLiteral {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token(&self.escaped());
    }
}

/// A language tag (`LANGTAG`, production 145), stored without the `@`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageTag(String);

impl LanguageTag {
    /// Creates a language tag.
    ///
    /// # Errors
    ///
    /// Returns an error if the tag does not match `[a-zA-Z]+ ('-' [a-zA-Z0-9]+)*`.
    pub fn new(tag: impl Into<String>) -> Result<Self, InvalidTerminal> {
        let tag = tag.into();
        let mut parts = tag.split('-');
        let valid = parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphabetic()))
            && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()));
        if valid {
            Ok(Self(tag))
        } else {
            Err(InvalidTerminal::new("LANGTAG", &tag))
        }
    }

    /// The tag without `@`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The optional suffix of an RDF literal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LiteralAnnotation {
    /// `@lang`
    Language(LanguageTag),
    /// `^^iri`
    Datatype(Iri),
}

/// A quoted literal (`RDFLiteral`, production 129).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RdfLiteral {
    /// The string part.
    pub value: StringLiteral,
    /// The language tag or datatype, if any.
    pub annotation: Option<LiteralAnnotation>,
}

impl WriteSparql for RdfLiteral {
    fn write_sparql(&self, w: &mut TokenWriter) {
        let mut token = self.value.escaped();
        match &self.annotation {
            None => {}
            Some(LiteralAnnotation::Language(tag)) => {
                token.push('@');
                token.push_str(tag.as_str());
            }
            Some(LiteralAnnotation::Datatype(iri)) => {
                token.push_str("^^");
                token.push_str(&iri.to_sparql());
            }
        }
        w.token(&token);
    }
}

/// The lexical class of a numeric literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NumericKind {
    /// `INTEGER` and its signed variants (`xsd:integer`).
    Integer,
    /// `DECIMAL` and its signed variants (`xsd:decimal`).
    Decimal,
    /// `DOUBLE` and its signed variants (`xsd:double`).
    Double,
}

/// A numeric literal (`NumericLiteral`, production 130), possibly signed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NumericLiteral {
    lexical: String,
    kind: NumericKind,
}

impl NumericLiteral {
    /// Parses a numeric token such as `42`, `-1.5`, `+.5e3`.
    ///
    /// # Errors
    ///
    /// Returns an error if `lexical` matches none of the numeric terminals.
    pub fn new(lexical: impl Into<String>) -> Result<Self, InvalidTerminal> {
        let lexical = lexical.into();
        match Self::classify(&lexical) {
            Some(kind) => Ok(Self { lexical, kind }),
            None => Err(InvalidTerminal::new("NumericLiteral", &lexical)),
        }
    }

    fn classify(s: &str) -> Option<NumericKind> {
        let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
        let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
            Some(i) => (&unsigned[..i], Some(&unsigned[i + 1..])),
            None => (unsigned, None),
        };
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        if let Some(exp) = exponent {
            let exp_digits = exp.strip_prefix(['+', '-']).unwrap_or(exp);
            if !digits(exp_digits) {
                return None;
            }
            // [0-9]+ '.' [0-9]* | '.' [0-9]+ | [0-9]+
            let valid = match mantissa.split_once('.') {
                Some((int, frac)) => {
                    (digits(int) && (frac.is_empty() || digits(frac)))
                        || (int.is_empty() && digits(frac))
                }
                None => digits(mantissa),
            };
            return valid.then_some(NumericKind::Double);
        }
        match mantissa.split_once('.') {
            Some((int, frac)) => {
                ((int.is_empty() || digits(int)) && digits(frac)).then_some(NumericKind::Decimal)
            }
            None => digits(mantissa).then_some(NumericKind::Integer),
        }
    }

    /// The token text.
    #[must_use]
    pub fn lexical(&self) -> &str {
        &self.lexical
    }

    /// The lexical class.
    #[must_use]
    pub fn kind(&self) -> NumericKind {
        self.kind
    }

    /// Whether the token starts with `+` or `-`.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        self.lexical.starts_with(['+', '-'])
    }
}

impl WriteSparql for NumericLiteral {
    fn write_sparql(&self, w: &mut TokenWriter) {
        w.token(&self.lexical);
    }
}

/// Any literal: quoted, numeric or boolean.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Literal {
    /// A quoted literal.
    Rdf(RdfLiteral),
    /// A numeric shorthand.
    Numeric(NumericLiteral),
    /// `true` or `false`.
    Boolean(bool),
}

impl WriteSparql for Literal {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Rdf(l) => l.write_sparql(w),
            Self::Numeric(n) => n.write_sparql(w),
            Self::Boolean(b) => w.token(if *b { "true" } else { "false" }),
        }
    }
}

// ---------------------------------------------------------------------------
// Blank nodes and composite terms
// ---------------------------------------------------------------------------

/// A blank node (`BlankNode`, production 138).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BlankNode {
    /// `_:label`
    Labeled(BlankNodeLabel),
    /// `[]`
    Anon,
}

/// The label of a blank node, without `_:`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlankNodeLabel(String);

impl BlankNodeLabel {
    /// Creates a label.
    ///
    /// # Errors
    ///
    /// Returns an error if the label does not match `BLANK_NODE_LABEL`.
    pub fn new(label: impl Into<String>) -> Result<Self, InvalidTerminal> {
        let label = label.into();
        if is_blank_node_label(&label) {
            Ok(Self(label))
        } else {
            Err(InvalidTerminal::new("BLANK_NODE_LABEL", &label))
        }
    }

    /// The label without `_:`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl WriteSparql for BlankNode {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Labeled(l) => w.token(&format!("_:{}", l.0)),
            Self::Anon => w.token("[]"),
        }
    }
}

/// An RDF term usable in a triple pattern (`GraphTerm`, production 109).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GraphTerm {
    /// An IRI.
    Iri(Iri),
    /// A literal.
    Literal(Literal),
    /// A blank node.
    BlankNode(BlankNode),
    /// The empty list `()`.
    Nil,
}

impl WriteSparql for GraphTerm {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Iri(i) => i.write_sparql(w),
            Self::Literal(l) => l.write_sparql(w),
            Self::BlankNode(b) => b.write_sparql(w),
            Self::Nil => w.token("()"),
        }
    }
}

/// `VarOrTerm` (production 106).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VarOrTerm {
    /// A variable.
    Var(Variable),
    /// A term.
    Term(GraphTerm),
}

impl WriteSparql for VarOrTerm {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Var(v) => v.write_sparql(w),
            Self::Term(t) => t.write_sparql(w),
        }
    }
}

/// `VarOrIri` (production 107).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VarOrIri {
    /// A variable.
    Var(Variable),
    /// An IRI.
    Iri(Iri),
}

impl WriteSparql for VarOrIri {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Var(v) => v.write_sparql(w),
            Self::Iri(i) => i.write_sparql(w),
        }
    }
}

display_via_sparql!(
    Variable,
    IriRef,
    PrefixedName,
    Iri,
    StringLiteral,
    RdfLiteral,
    NumericLiteral,
    Literal,
    BlankNode,
    GraphTerm,
    VarOrTerm,
    VarOrIri,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_classification_follows_the_grammar() {
        for (s, kind) in [
            ("0", NumericKind::Integer),
            ("-12", NumericKind::Integer),
            ("+007", NumericKind::Integer),
            (".5", NumericKind::Decimal),
            ("-1.25", NumericKind::Decimal),
            ("1e3", NumericKind::Double),
            ("1.E-3", NumericKind::Double),
            (".5e+7", NumericKind::Double),
        ] {
            assert_eq!(NumericLiteral::new(s).map(|n| n.kind()), Ok(kind), "{s}");
        }
        for s in [
            "", "1.", "+", "1e", "e3", ".e3", "1..2", "--1", "NaN", "INF",
        ] {
            assert!(NumericLiteral::new(s).is_err(), "{s} should be rejected");
        }
    }

    #[test]
    fn local_names_are_escaped_where_needed() {
        assert_eq!(PrefixedName::escape_local("abc").as_deref(), Some("abc"));
        assert_eq!(PrefixedName::escape_local("a.b").as_deref(), Some("a.b"));
        assert_eq!(PrefixedName::escape_local("ab.").as_deref(), Some("ab\\."));
        assert_eq!(PrefixedName::escape_local("-x").as_deref(), Some("\\-x"));
        assert_eq!(
            PrefixedName::escape_local("a/b?c").as_deref(),
            Some("a\\/b\\?c")
        );
        assert_eq!(
            PrefixedName::escape_local("a%20b").as_deref(),
            Some("a%20b")
        );
        assert_eq!(
            PrefixedName::escape_local("a%zz").as_deref(),
            Some("a\\%zz")
        );
        assert_eq!(PrefixedName::escape_local("Q42").as_deref(), Some("Q42"));
        assert_eq!(PrefixedName::escape_local("a b"), None);
        let p = PrefixedName::new("ex", "a\\/b").unwrap();
        assert_eq!(p.unescaped_local(), "a/b");
    }

    #[test]
    fn strings_are_escaped_per_quote_style() {
        let s = StringLiteral::new("a\"b'c\nd\\", QuoteStyle::Double).unwrap();
        assert_eq!(s.to_sparql(), r#""a\"b'c\nd\\""#);
        let s = StringLiteral::new("a\"b'c\nd", QuoteStyle::LongSingle).unwrap();
        assert_eq!(s.to_sparql(), "'''a\"b\\'c\nd'''");
        assert!(StringLiteral::new("x\\u0041", QuoteStyle::Double).is_err());
    }

    #[test]
    fn variables_compare_by_name_only() {
        let a = Variable::new("x").unwrap();
        let b = Variable::with_sigil("x", VarSigil::Dollar).unwrap();
        assert_eq!(a, b);
        assert_eq!(b.to_sparql(), "$x");
        assert!(Variable::new("1a").is_ok());
        assert!(Variable::new("a-b").is_err());
    }

    #[test]
    fn terminals_are_validated() {
        assert!(IriRef::new("http://example.org/a b").is_err());
        assert!(IriRef::new("").is_ok());
        assert!(LanguageTag::new("en-US").is_ok());
        assert!(LanguageTag::new("en-").is_err());
        assert!(BlankNodeLabel::new("b.1").is_ok());
        assert!(BlankNodeLabel::new("b1.").is_err());
        assert!(PrefixedName::new("", "").is_ok());
        assert!(PrefixedName::new("1ex", "a").is_err());
    }
}
