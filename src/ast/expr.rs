//! Expressions (productions 110–128).
//!
//! [`Expression`] is an operator tree rather than a literal transcription of
//! the precedence-climbing productions. Serialization inserts the parentheses
//! required by the grammar (for example, unary operators only apply to a
//! `PrimaryExpression` and relational operators do not associate), while
//! [`Expression::Bracketed`] records *explicit* parentheses chosen by the
//! generator. Parsing the output therefore yields the same tree.

use super::pattern::GroupGraphPattern;
use super::term::{Iri, Literal, NumericLiteral, StringLiteral, Variable};
use super::writer::{TokenWriter, WriteSparql, display_via_sparql, write_comma_separated};

/// A relational operator (production 114).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareOp {
    /// `=`
    Equal,
    /// `!=`
    NotEqual,
    /// `<`
    Less,
    /// `>`
    Greater,
    /// `<=`
    LessOrEqual,
    /// `>=`
    GreaterOrEqual,
}

impl CompareOp {
    /// All relational operators.
    pub const ALL: [Self; 6] = [
        Self::Equal,
        Self::NotEqual,
        Self::Less,
        Self::Greater,
        Self::LessOrEqual,
        Self::GreaterOrEqual,
    ];

    /// The operator token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::Greater => ">",
            Self::LessOrEqual => "<=",
            Self::GreaterOrEqual => ">=",
        }
    }
}

/// A multiplicative operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MultiplicativeOp {
    /// `*`
    Multiply,
    /// `/`
    Divide,
}

impl MultiplicativeOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Multiply => "*",
            Self::Divide => "/",
        }
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expression {
    /// `a || b`
    Or(Box<Expression>, Box<Expression>),
    /// `a && b`
    And(Box<Expression>, Box<Expression>),
    /// `a op b` for a relational operator.
    Compare(Box<Expression>, CompareOp, Box<Expression>),
    /// `a IN (b, ...)`
    In(Box<Expression>, Vec<Expression>),
    /// `a NOT IN (b, ...)`
    NotIn(Box<Expression>, Vec<Expression>),
    /// `a + b`
    Add(Box<Expression>, Box<Expression>),
    /// `a - b`
    Subtract(Box<Expression>, Box<Expression>),
    /// The special `AdditiveExpression` branch where a signed numeric literal
    /// directly follows an expression, e.g. `?a -1 * ?b` meaning `?a - (1 * ?b)`.
    SignedTail {
        /// The left operand.
        lhs: Box<Expression>,
        /// The signed literal; its sign acts as the additive operator.
        literal: NumericLiteral,
        /// Multiplicative continuation applied to the unsigned literal.
        tail: Vec<(MultiplicativeOp, Expression)>,
    },
    /// `a * b`
    Multiply(Box<Expression>, Box<Expression>),
    /// `a / b`
    Divide(Box<Expression>, Box<Expression>),
    /// `!a`
    Not(Box<Expression>),
    /// `+a`
    UnaryPlus(Box<Expression>),
    /// `-a`
    UnaryMinus(Box<Expression>),
    /// `(a)` — explicit parentheses.
    Bracketed(Box<Expression>),
    /// A built-in call, including aggregates and `EXISTS`.
    BuiltIn(BuiltInCall),
    /// A call of a function identified by an IRI (casts, extension functions).
    Function(FunctionCall),
    /// An IRI used as a value.
    Iri(Iri),
    /// A literal.
    Literal(Literal),
    /// A variable.
    Var(Variable),
}

impl Expression {
    /// The binding strength of the top-level operator (higher binds tighter).
    fn precedence(&self) -> u8 {
        match self {
            Self::Or(..) => 1,
            Self::And(..) => 2,
            Self::Compare(..) | Self::In(..) | Self::NotIn(..) => 3,
            Self::Add(..) | Self::Subtract(..) | Self::SignedTail { .. } => 4,
            Self::Multiply(..) | Self::Divide(..) => 5,
            Self::Not(_) | Self::UnaryPlus(_) | Self::UnaryMinus(_) => 6,
            Self::Bracketed(_)
            | Self::BuiltIn(_)
            | Self::Function(_)
            | Self::Iri(_)
            | Self::Literal(_)
            | Self::Var(_) => 7,
        }
    }

    /// Writes `self`, adding parentheses if it binds weaker than `min`.
    fn write_at(&self, w: &mut TokenWriter, min: u8) {
        if self.precedence() < min {
            w.open("(");
            self.write_sparql(w);
            w.glue(")");
        } else {
            self.write_sparql(w);
        }
    }

    fn write_binary(w: &mut TokenWriter, lhs: &Self, op: &str, rhs: &Self, prec: u8) {
        // All binary operators are left-associative (relational ones do not
        // associate at all), so the right operand must bind strictly tighter.
        let lhs_min = if prec == 3 { 4 } else { prec };
        lhs.write_at(w, lhs_min);
        w.token(op);
        rhs.write_at(w, prec + 1);
    }

    /// Wraps a variable.
    #[must_use]
    pub fn var(v: Variable) -> Self {
        Self::Var(v)
    }

    /// Builds `lhs op rhs`.
    #[must_use]
    pub fn compare(lhs: Self, op: CompareOp, rhs: Self) -> Self {
        Self::Compare(Box::new(lhs), op, Box::new(rhs))
    }
}

impl WriteSparql for Expression {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Or(a, b) => Self::write_binary(w, a, "||", b, 1),
            Self::And(a, b) => Self::write_binary(w, a, "&&", b, 2),
            Self::Compare(a, op, b) => Self::write_binary(w, a, op.as_str(), b, 3),
            Self::In(a, list) | Self::NotIn(a, list) => {
                a.write_at(w, 4);
                w.token(if matches!(self, Self::In(..)) {
                    "IN"
                } else {
                    "NOT IN"
                });
                write_expression_list(w, list, false);
            }
            Self::Add(a, b) => Self::write_binary(w, a, "+", b, 4),
            Self::Subtract(a, b) => Self::write_binary(w, a, "-", b, 4),
            Self::SignedTail { lhs, literal, tail } => {
                lhs.write_at(w, 4);
                literal.write_sparql(w);
                for (op, operand) in tail {
                    w.token(op.as_str());
                    operand.write_at(w, 6);
                }
            }
            Self::Multiply(a, b) => Self::write_binary(w, a, "*", b, 5),
            Self::Divide(a, b) => Self::write_binary(w, a, "/", b, 5),
            Self::Not(a) | Self::UnaryPlus(a) | Self::UnaryMinus(a) => {
                w.token(match self {
                    Self::Not(_) => "!",
                    Self::UnaryPlus(_) => "+",
                    _ => "-",
                });
                a.write_at(w, 7);
            }
            Self::Bracketed(a) => {
                w.open("(");
                a.write_sparql(w);
                w.glue(")");
            }
            Self::BuiltIn(b) => b.write_sparql(w),
            Self::Function(f) => f.write_sparql(w),
            Self::Iri(i) => i.write_sparql(w),
            Self::Literal(l) => l.write_sparql(w),
            Self::Var(v) => v.write_sparql(w),
        }
    }
}

/// Writes an `ExpressionList` (production 72) or, with `glued`, an argument
/// list directly attached to a function name.
fn write_expression_list(w: &mut TokenWriter, list: &[Expression], glued: bool) {
    if list.is_empty() {
        if glued {
            w.glue("()");
        } else {
            w.token("()");
        }
        return;
    }
    if glued {
        w.glue_open("(");
    } else {
        w.open("(");
    }
    write_comma_separated(w, list);
    w.glue(")");
}

/// Built-in functions called with a list of expressions (production 121).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[allow(missing_docs)] // The variants are the SPARQL keywords.
pub enum BuiltInFunction {
    Str,
    Lang,
    LangMatches,
    Datatype,
    Iri,
    Uri,
    Bnode,
    Rand,
    Abs,
    Ceil,
    Floor,
    Round,
    Concat,
    Substr,
    StrLen,
    Replace,
    UCase,
    LCase,
    EncodeForUri,
    Contains,
    StrStarts,
    StrEnds,
    StrBefore,
    StrAfter,
    Year,
    Month,
    Day,
    Hours,
    Minutes,
    Seconds,
    Timezone,
    Tz,
    Now,
    Uuid,
    StrUuid,
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
    Coalesce,
    If,
    StrLang,
    StrDt,
    SameTerm,
    IsIri,
    IsUri,
    IsBlank,
    IsLiteral,
    IsNumeric,
    Regex,
}

/// The number of arguments a built-in function accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// Exactly `n` arguments.
    Exact(usize),
    /// Between `min` and `max` arguments (inclusive).
    Range(usize, usize),
    /// Any number of arguments (an `ExpressionList`).
    Variadic,
}

impl Arity {
    /// Whether `n` arguments are allowed.
    #[must_use]
    pub fn accepts(self, n: usize) -> bool {
        match self {
            Self::Exact(k) => n == k,
            Self::Range(a, b) => (a..=b).contains(&n),
            Self::Variadic => true,
        }
    }
}

impl BuiltInFunction {
    /// All built-in functions of SPARQL 1.1 that take expression arguments.
    pub const ALL: [Self; 51] = [
        Self::Str,
        Self::Lang,
        Self::LangMatches,
        Self::Datatype,
        Self::Iri,
        Self::Uri,
        Self::Bnode,
        Self::Rand,
        Self::Abs,
        Self::Ceil,
        Self::Floor,
        Self::Round,
        Self::Concat,
        Self::Substr,
        Self::StrLen,
        Self::Replace,
        Self::UCase,
        Self::LCase,
        Self::EncodeForUri,
        Self::Contains,
        Self::StrStarts,
        Self::StrEnds,
        Self::StrBefore,
        Self::StrAfter,
        Self::Year,
        Self::Month,
        Self::Day,
        Self::Hours,
        Self::Minutes,
        Self::Seconds,
        Self::Timezone,
        Self::Tz,
        Self::Now,
        Self::Uuid,
        Self::StrUuid,
        Self::Md5,
        Self::Sha1,
        Self::Sha256,
        Self::Sha384,
        Self::Sha512,
        Self::Coalesce,
        Self::If,
        Self::StrLang,
        Self::StrDt,
        Self::SameTerm,
        Self::IsIri,
        Self::IsUri,
        Self::IsBlank,
        Self::IsLiteral,
        Self::IsNumeric,
        Self::Regex,
    ];

    /// The keyword as written in the grammar.
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Str => "STR",
            Self::Lang => "LANG",
            Self::LangMatches => "LANGMATCHES",
            Self::Datatype => "DATATYPE",
            Self::Iri => "IRI",
            Self::Uri => "URI",
            Self::Bnode => "BNODE",
            Self::Rand => "RAND",
            Self::Abs => "ABS",
            Self::Ceil => "CEIL",
            Self::Floor => "FLOOR",
            Self::Round => "ROUND",
            Self::Concat => "CONCAT",
            Self::Substr => "SUBSTR",
            Self::StrLen => "STRLEN",
            Self::Replace => "REPLACE",
            Self::UCase => "UCASE",
            Self::LCase => "LCASE",
            Self::EncodeForUri => "ENCODE_FOR_URI",
            Self::Contains => "CONTAINS",
            Self::StrStarts => "STRSTARTS",
            Self::StrEnds => "STRENDS",
            Self::StrBefore => "STRBEFORE",
            Self::StrAfter => "STRAFTER",
            Self::Year => "YEAR",
            Self::Month => "MONTH",
            Self::Day => "DAY",
            Self::Hours => "HOURS",
            Self::Minutes => "MINUTES",
            Self::Seconds => "SECONDS",
            Self::Timezone => "TIMEZONE",
            Self::Tz => "TZ",
            Self::Now => "NOW",
            Self::Uuid => "UUID",
            Self::StrUuid => "STRUUID",
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
            Self::Sha384 => "SHA384",
            Self::Sha512 => "SHA512",
            Self::Coalesce => "COALESCE",
            Self::If => "IF",
            Self::StrLang => "STRLANG",
            Self::StrDt => "STRDT",
            Self::SameTerm => "sameTerm",
            Self::IsIri => "isIRI",
            Self::IsUri => "isURI",
            Self::IsBlank => "isBLANK",
            Self::IsLiteral => "isLITERAL",
            Self::IsNumeric => "isNUMERIC",
            Self::Regex => "REGEX",
        }
    }

    /// The number of arguments allowed by the grammar.
    #[must_use]
    pub fn arity(self) -> Arity {
        match self {
            Self::Rand | Self::Now | Self::Uuid | Self::StrUuid => Arity::Exact(0),
            Self::Bnode => Arity::Range(0, 1),
            Self::Concat | Self::Coalesce => Arity::Variadic,
            Self::LangMatches
            | Self::Contains
            | Self::StrStarts
            | Self::StrEnds
            | Self::StrBefore
            | Self::StrAfter
            | Self::StrLang
            | Self::StrDt
            | Self::SameTerm => Arity::Exact(2),
            Self::Substr | Self::Regex => Arity::Range(2, 3),
            Self::Replace => Arity::Range(3, 4),
            Self::If => Arity::Exact(3),
            _ => Arity::Exact(1),
        }
    }

    /// Whether repeated evaluation may return different results
    /// (`RAND`, `NOW`, `UUID`, `STRUUID`, `BNODE`).
    #[must_use]
    pub fn is_nondeterministic(self) -> bool {
        matches!(
            self,
            Self::Rand | Self::Now | Self::Uuid | Self::StrUuid | Self::Bnode
        )
    }
}

/// A built-in call (`BuiltInCall`, production 121).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuiltInCall {
    /// A built-in function applied to expression arguments. The number of
    /// arguments must satisfy [`BuiltInFunction::arity`].
    Call {
        /// The function.
        function: BuiltInFunction,
        /// The arguments.
        args: Vec<Expression>,
    },
    /// `BOUND(?v)`
    Bound(Variable),
    /// `EXISTS { ... }`
    Exists(Box<GroupGraphPattern>),
    /// `NOT EXISTS { ... }`
    NotExists(Box<GroupGraphPattern>),
    /// An aggregate; only allowed in `SELECT`, `HAVING` and `ORDER BY`.
    Aggregate(Aggregate),
}

impl WriteSparql for BuiltInCall {
    fn write_sparql(&self, w: &mut TokenWriter) {
        match self {
            Self::Call { function, args } => {
                w.token(function.keyword());
                match function.arity() {
                    // These are written with an `ExpressionList`/`NIL` in the grammar.
                    Arity::Variadic | Arity::Exact(0) => write_expression_list(w, args, true),
                    _ if args.is_empty() => w.glue("()"), // BNODE()
                    _ => {
                        w.glue_open("(");
                        write_comma_separated(w, args);
                        w.glue(")");
                    }
                }
            }
            Self::Bound(v) => {
                w.token("BOUND");
                w.glue_open("(");
                v.write_sparql(w);
                w.glue(")");
            }
            Self::Exists(p) => {
                w.token("EXISTS");
                p.write_sparql(w);
            }
            Self::NotExists(p) => {
                w.token("NOT EXISTS");
                p.write_sparql(w);
            }
            Self::Aggregate(a) => a.write_sparql(w),
        }
    }
}

/// Aggregate set functions other than `COUNT` and `GROUP_CONCAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SetFunction {
    /// `SUM`
    Sum,
    /// `MIN`
    Min,
    /// `MAX`
    Max,
    /// `AVG`
    Avg,
    /// `SAMPLE`
    Sample,
}

impl SetFunction {
    /// All set functions of this kind.
    pub const ALL: [Self; 5] = [Self::Sum, Self::Min, Self::Max, Self::Avg, Self::Sample];

    fn keyword(self) -> &'static str {
        match self {
            Self::Sum => "SUM",
            Self::Min => "MIN",
            Self::Max => "MAX",
            Self::Avg => "AVG",
            Self::Sample => "SAMPLE",
        }
    }
}

/// An aggregate (`Aggregate`, production 127).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Aggregate {
    /// `COUNT(DISTINCT? *)` if `expr` is `None`, else `COUNT(DISTINCT? expr)`.
    Count {
        /// Whether `DISTINCT` is given.
        distinct: bool,
        /// The counted expression; `None` for `*`.
        expr: Option<Box<Expression>>,
    },
    /// `SUM`, `MIN`, `MAX`, `AVG` or `SAMPLE`.
    Set {
        /// The function.
        function: SetFunction,
        /// Whether `DISTINCT` is given.
        distinct: bool,
        /// The aggregated expression.
        expr: Box<Expression>,
    },
    /// `GROUP_CONCAT(DISTINCT? expr ; SEPARATOR = "...")`
    GroupConcat {
        /// Whether `DISTINCT` is given.
        distinct: bool,
        /// The aggregated expression.
        expr: Box<Expression>,
        /// The separator, if given.
        separator: Option<StringLiteral>,
    },
}

impl WriteSparql for Aggregate {
    fn write_sparql(&self, w: &mut TokenWriter) {
        let (keyword, distinct) = match self {
            Self::Count { distinct, .. } => ("COUNT", *distinct),
            Self::Set {
                function, distinct, ..
            } => (function.keyword(), *distinct),
            Self::GroupConcat { distinct, .. } => ("GROUP_CONCAT", *distinct),
        };
        w.token(keyword);
        w.glue_open("(");
        if distinct {
            w.token("DISTINCT");
        }
        match self {
            Self::Count { expr: None, .. } => w.token("*"),
            Self::Count { expr: Some(e), .. } | Self::Set { expr: e, .. } => e.write_sparql(w),
            Self::GroupConcat {
                expr, separator, ..
            } => {
                expr.write_sparql(w);
                if let Some(sep) = separator {
                    w.token(";");
                    w.token("SEPARATOR");
                    w.token("=");
                    sep.write_sparql(w);
                }
            }
        }
        w.glue(")");
    }
}

/// A function call identified by an IRI (`FunctionCall`, production 70).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionCall {
    /// The function IRI.
    pub iri: Iri,
    /// Whether `DISTINCT` is given (only meaningful for custom aggregates).
    pub distinct: bool,
    /// The arguments; an empty list is written as `NIL`.
    pub args: Vec<Expression>,
}

impl WriteSparql for FunctionCall {
    fn write_sparql(&self, w: &mut TokenWriter) {
        self.iri.write_sparql(w);
        if self.args.is_empty() {
            w.glue("()");
            return;
        }
        w.glue_open("(");
        if self.distinct {
            w.token("DISTINCT");
        }
        write_comma_separated(w, &self.args);
        w.glue(")");
    }
}

display_via_sparql!(Expression, BuiltInCall, Aggregate, FunctionCall);

#[cfg(test)]
mod tests {
    use super::*;

    fn v(name: &str) -> Expression {
        Expression::Var(Variable::new(name).unwrap())
    }

    fn num(lexical: &str) -> Expression {
        Expression::Literal(Literal::Numeric(NumericLiteral::new(lexical).unwrap()))
    }

    #[test]
    fn parentheses_follow_precedence() {
        let e = Expression::Multiply(
            Box::new(Expression::Add(Box::new(v("a")), Box::new(v("b")))),
            Box::new(v("c")),
        );
        assert_eq!(e.to_sparql(), "(?a + ?b) * ?c");
        let e = Expression::Subtract(
            Box::new(v("a")),
            Box::new(Expression::Subtract(Box::new(v("b")), Box::new(v("c")))),
        );
        assert_eq!(e.to_sparql(), "?a - (?b - ?c)");
        let e = Expression::Not(Box::new(Expression::And(
            Box::new(v("a")),
            Box::new(v("b")),
        )));
        assert_eq!(e.to_sparql(), "! (?a && ?b)");
        let e = Expression::compare(
            Expression::compare(v("a"), CompareOp::Less, v("b")),
            CompareOp::Equal,
            Expression::Literal(Literal::Boolean(true)),
        );
        assert_eq!(e.to_sparql(), "(?a < ?b) = true");
        let e = Expression::Subtract(Box::new(v("a")), Box::new(num("-1")));
        assert_eq!(e.to_sparql(), "?a - -1");
    }

    #[test]
    fn signed_tail_is_written_without_operator() {
        let e = Expression::SignedTail {
            lhs: Box::new(v("a")),
            literal: NumericLiteral::new("-1").unwrap(),
            tail: vec![(MultiplicativeOp::Multiply, v("b"))],
        };
        assert_eq!(e.to_sparql(), "?a -1 * ?b");
    }

    #[test]
    fn calls_are_written_compactly() {
        let e = Expression::BuiltIn(BuiltInCall::Call {
            function: BuiltInFunction::Rand,
            args: vec![],
        });
        assert_eq!(e.to_sparql(), "RAND()");
        let e = Expression::BuiltIn(BuiltInCall::Aggregate(Aggregate::GroupConcat {
            distinct: true,
            expr: Box::new(v("x")),
            separator: Some(
                StringLiteral::new(",", super::super::term::QuoteStyle::Single).unwrap(),
            ),
        }));
        assert_eq!(e.to_sparql(), "GROUP_CONCAT(DISTINCT ?x ; SEPARATOR = ',')");
        let e = Expression::In(Box::new(v("x")), vec![num("1"), num("2")]);
        assert_eq!(e.to_sparql(), "?x IN (1, 2)");
    }
}
