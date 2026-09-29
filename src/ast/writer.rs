//! Canonical token-level serialization of the syntax tree.
//!
//! Every syntax tree node implements [`WriteSparql`], which emits the node as
//! a sequence of tokens into a [`TokenWriter`]. Tokens are separated by
//! exactly one space, so the output is fully determined by the tree. The
//! generator uses the very same writer while it builds a query, which is what
//! makes the partial query handed to a
//! [`CompletionProvider`](crate::provider::CompletionProvider) a true prefix of
//! the final query string.

use std::fmt;

/// Accumulates SPARQL tokens separated by single spaces.
///
/// Punctuation that is unambiguous without surrounding whitespace can be
/// attached to its neighbours with [`TokenWriter::glue`], [`TokenWriter::open`]
/// and [`TokenWriter::glue_open`], which gives the familiar `STR(?x)` layout.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TokenWriter {
    buf: String,
    suppress_space: bool,
}

/// A position inside a [`TokenWriter`], used to roll back speculative output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Mark {
    len: usize,
    suppress_space: bool,
}

impl TokenWriter {
    /// Creates an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one token, separated from the previous one by a single space.
    pub fn token(&mut self, token: &str) {
        if !self.buf.is_empty() && !self.suppress_space {
            self.buf.push(' ');
        }
        self.suppress_space = false;
        self.buf.push_str(token);
    }

    /// Appends a token directly after the previous one, without a space.
    pub fn glue(&mut self, token: &str) {
        self.suppress_space = false;
        self.buf.push_str(token);
    }

    /// Appends an opening token after which no space is written.
    pub fn open(&mut self, token: &str) {
        self.token(token);
        self.suppress_space = true;
    }

    /// Glues an opening token to the previous one (e.g. the `(` of a call).
    pub fn glue_open(&mut self, token: &str) {
        self.glue(token);
        self.suppress_space = true;
    }

    /// Appends the serialization of a node.
    pub fn node<N: WriteSparql + ?Sized>(&mut self, node: &N) {
        node.write_sparql(self);
    }

    /// Returns the text written so far.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    /// Consumes the writer and returns the written text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.buf
    }

    /// Returns the current position, to be used with [`TokenWriter::reset`].
    #[must_use]
    pub fn mark(&self) -> Mark {
        Mark {
            len: self.buf.len(),
            suppress_space: self.suppress_space,
        }
    }

    /// Discards everything written after `mark`.
    pub fn reset(&mut self, mark: Mark) {
        self.buf.truncate(mark.len);
        self.suppress_space = mark.suppress_space;
    }
}

impl fmt::Display for TokenWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.buf)
    }
}

/// Serialization of syntax tree nodes to SPARQL tokens.
pub trait WriteSparql {
    /// Writes the tokens of this node.
    fn write_sparql(&self, w: &mut TokenWriter);

    /// Serializes this node on its own.
    fn to_sparql(&self) -> String {
        let mut w = TokenWriter::new();
        self.write_sparql(&mut w);
        w.into_string()
    }
}

impl<T: WriteSparql + ?Sized> WriteSparql for Box<T> {
    fn write_sparql(&self, w: &mut TokenWriter) {
        (**self).write_sparql(w);
    }
}

/// Writes `items` separated by glued commas, e.g. `a, b, c`.
pub(crate) fn write_comma_separated<T: WriteSparql>(w: &mut TokenWriter, items: &[T]) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            w.glue(",");
        }
        item.write_sparql(w);
    }
}

/// Implements [`std::fmt::Display`] via [`WriteSparql`].
macro_rules! display_via_sparql {
    ($($ty:ty),* $(,)?) => {
        $(
            impl ::std::fmt::Display for $ty {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    f.write_str(&$crate::ast::WriteSparql::to_sparql(self))
                }
            }
        )*
    };
}
pub(crate) use display_via_sparql;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_space_separated_and_resettable() {
        let mut w = TokenWriter::new();
        w.token("SELECT");
        let mark = w.mark();
        w.token("*");
        assert_eq!(w.as_str(), "SELECT *");
        w.reset(mark);
        w.token("?x");
        assert_eq!(w.as_str(), "SELECT ?x");
    }

    #[test]
    fn punctuation_can_be_glued() {
        let mut w = TokenWriter::new();
        w.token("FILTER");
        w.token("STR");
        w.glue_open("(");
        w.token("?x");
        w.glue(",");
        w.token("1");
        w.glue(")");
        w.open("(");
        w.glue(")");
        assert_eq!(w.as_str(), "FILTER STR(?x, 1) ()");
    }
}
