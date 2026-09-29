//! Expressions.
//!
//! Expressions are generated top-down in text order. Each production knows
//! its precedence when it is chosen, so parentheses required by the grammar
//! are emitted before the operands are generated, exactly as the syntax
//! tree's serializer would write them.

use oxrdf::vocab::xsd;
use oxrdf::{NamedNode, Term};

use super::context::{BareVars, Gen, Value};
use super::pattern::GroupKind;
use crate::ast::{
    Aggregate, BuiltInCall, BuiltInFunction, CompareOp, Expression, FunctionCall, Literal,
    MultiplicativeOp, NumericLiteral, RdfLiteral, SetFunction, Variable,
};
use crate::extension::FunctionSpec;
use crate::provider::{Slot, TermKinds, ValueType};

/// The productions an expression can be generated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Production {
    Variable,
    Constant,
    Or,
    And,
    Not,
    Compare,
    In,
    NotIn,
    Arithmetic,
    Unary,
    SignedTail,
    Bracketed,
    BuiltIn,
    Cast,
    ExtensionFunction,
    Exists,
    NotExists,
    Bound,
    Aggregate,
}

/// Parameter and result types of a built-in function.
struct Signature {
    params: &'static [ValueType],
    /// Optional trailing parameters.
    optional: &'static [ValueType],
    returns: ValueType,
}

/// The type standing for "same as the requested type" (`IF`, `COALESCE`).
const POLY: ValueType = ValueType::Any;

fn signature(f: BuiltInFunction) -> Signature {
    use BuiltInFunction as F;
    use ValueType::{
        Any, Boolean, DateTime, Iri, Literal as Lit, Numeric, SimpleString as Simple, String as Str,
    };
    let s = |params, optional, returns| Signature {
        params,
        optional,
        returns,
    };
    match f {
        F::Str => s(&[Any], &[], Simple),
        F::Lang => s(&[Lit], &[], Simple),
        F::LangMatches => s(&[Simple, Simple], &[], Boolean),
        F::Datatype => s(&[Lit], &[], Iri),
        F::Iri | F::Uri => s(&[Any], &[], Iri),
        F::Bnode => s(&[], &[Simple], Any),
        F::Rand => s(&[], &[], Numeric),
        F::Abs | F::Ceil | F::Floor | F::Round => s(&[Numeric], &[], Numeric),
        F::Concat => s(&[], &[], Str),
        F::Substr => s(&[Str, Numeric], &[Numeric], Str),
        F::StrLen => s(&[Str], &[], Numeric),
        F::Replace => s(&[Str, Simple, Simple], &[Simple], Str),
        F::UCase | F::LCase => s(&[Str], &[], Str),
        F::EncodeForUri => s(&[Str], &[], Simple),
        F::Contains | F::StrStarts | F::StrEnds => s(&[Str, Str], &[], Boolean),
        F::StrBefore | F::StrAfter => s(&[Str, Str], &[], Str),
        F::Year | F::Month | F::Day | F::Hours | F::Minutes | F::Seconds => {
            s(&[DateTime], &[], Numeric)
        }
        F::Timezone => s(&[DateTime], &[], Lit),
        F::Tz => s(&[DateTime], &[], Simple),
        F::Now => s(&[], &[], DateTime),
        F::Uuid => s(&[], &[], Iri),
        F::StrUuid => s(&[], &[], Simple),
        F::Md5 | F::Sha1 | F::Sha256 | F::Sha384 | F::Sha512 => s(&[Simple], &[], Simple),
        F::Coalesce => s(&[], &[], POLY),
        F::If => s(&[Boolean, POLY, POLY], &[], POLY),
        F::StrLang => s(&[Simple, Simple], &[], Str),
        F::StrDt => s(&[Simple, Iri], &[], Lit),
        F::SameTerm => s(&[Any, Any], &[], Boolean),
        F::IsIri | F::IsUri | F::IsBlank | F::IsLiteral | F::IsNumeric => s(&[Any], &[], Boolean),
        F::Regex => s(&[Str, Simple], &[Simple], Boolean),
    }
}

/// XSD constructor functions (casts) and their result types.
const CASTS: [(&str, ValueType); 7] = [
    ("boolean", ValueType::Boolean),
    ("integer", ValueType::Numeric),
    ("decimal", ValueType::Numeric),
    ("float", ValueType::Numeric),
    ("double", ValueType::Numeric),
    ("string", ValueType::SimpleString),
    ("dateTime", ValueType::DateTime),
];

const REGEX_PATTERNS: [&str; 8] = ["a", "^A", "e$", "[0-9]+", "a.*b", "(x|y)", "\\\\d", ""];
/// The flags of `XPath` F&O 1.0 `fn:matches`, which SPARQL 1.1 refers to.
const REGEX_FLAGS: [&str; 6] = ["i", "s", "m", "x", "im", ""];
const LANGUAGE_RANGES: [&str; 5] = ["*", "en", "de", "EN-us", ""];
const LANGUAGE_TAGS: [&str; 4] = ["en", "de", "fr-CA", "EN"];
const DATATYPES: [&str; 6] = [
    "string", "integer", "decimal", "double", "boolean", "dateTime",
];

/// Whether an expression whose type is `produced` can stand where `want`
/// is required. Unknown types are accepted.
fn fits(want: ValueType, produced: ValueType) -> bool {
    want == ValueType::Any || produced == ValueType::Any || want.accepts(produced)
}

impl Gen<'_> {
    /// Generates an expression of type `want`, parenthesized if its
    /// precedence is below `min_prec`.
    pub fn expr(&mut self, want: ValueType, min_prec: u8) -> Expression {
        self.expr_typed(want, min_prec).0
    }

    /// Like [`Gen::expr`], also returning the type of the expression.
    pub fn expr_typed(&mut self, want: ValueType, min_prec: u8) -> (Expression, ValueType) {
        self.hit("Expression");
        let leaves_only = self.expr_depth >= self.limits.max_expression_depth || !self.has_budget();
        let options = self.options(want, leaves_only, false);
        let production = self.choose(&options).unwrap_or(Production::Constant);
        self.expr_depth += 1;
        let result = self.production(production, want, min_prec);
        self.expr_depth -= 1;
        result
    }

    /// Generates a built-in or function call (for `FILTER f(...)`,
    /// `GROUP BY f(...)` and `ORDER BY f(...)`).
    pub fn call_expr(&mut self, want: ValueType) -> Expression {
        self.hit("Expression");
        let options = self.options(want, false, true);
        self.expr_depth += 1;
        let e = match self.choose(&options) {
            Some(production) => self.production(production, want, 7).0,
            None => self.forced_call(want),
        };
        self.expr_depth -= 1;
        debug_assert!(
            matches!(e, Expression::BuiltIn(_) | Expression::Function(_)),
            "{e:?}"
        );
        e
    }

    /// A call that is always possible: `isLITERAL(constant)` or `STR(constant)`.
    fn forced_call(&mut self, want: ValueType) -> Expression {
        let function = if fits(want, ValueType::Boolean) {
            BuiltInFunction::IsLiteral
        } else {
            BuiltInFunction::Str
        };
        self.hit(function.keyword());
        self.out.token(function.keyword());
        self.out.glue_open("(");
        let (arg, _) = self.constant_expr(ValueType::Any, None);
        self.out.glue(")");
        Expression::BuiltIn(BuiltInCall::Call {
            function,
            args: vec![arg],
        })
    }

    fn options(
        &mut self,
        want: ValueType,
        leaves_only: bool,
        calls_only: bool,
    ) -> Vec<(Production, f64)> {
        use Production as P;
        let w = self.w.expression.clone();
        let mut options: Vec<(P, f64)> = Vec::new();
        let boolean = fits(want, ValueType::Boolean);
        let numeric = fits(want, ValueType::Numeric);
        let in_scope = matches!(self.env.bare, BareVars::Scope) && !self.env.planning;
        let nest = !leaves_only;
        if !calls_only {
            options.push((P::Variable, w.variable));
            options.push((P::Constant, w.constant));
            if nest {
                if boolean {
                    options.extend([
                        (P::Or, w.or),
                        (P::And, w.and),
                        (P::Not, w.not),
                        (P::Compare, w.compare),
                        (P::In, w.r#in),
                        (P::NotIn, w.not_in),
                    ]);
                }
                if numeric {
                    options.extend([
                        (P::Arithmetic, w.arithmetic),
                        (P::Unary, w.unary),
                        (P::SignedTail, w.signed_tail),
                    ]);
                }
                options.push((P::Bracketed, w.bracketed));
            }
        }
        if nest || calls_only {
            if BuiltInFunction::ALL
                .iter()
                .any(|&f| self.function_weight(f) > 0.0 && fits(want, signature(f).returns))
            {
                options.push((P::BuiltIn, w.builtin));
            }
            if CASTS.iter().any(|(_, ty)| fits(want, *ty)) {
                options.push((P::Cast, w.cast));
            }
            if self
                .functions
                .iter()
                .any(|f| f.weight > 0.0 && fits(want, f.returns))
            {
                options.push((P::ExtensionFunction, w.extension_function));
            }
            if boolean && self.env.exists && in_scope && self.can_nest() {
                options.extend([(P::Exists, w.exists), (P::NotExists, w.not_exists)]);
            }
            if boolean
                && (matches!(self.env.bare, BareVars::Scope)
                    || !self.expression_variables().is_empty())
            {
                options.push((P::Bound, w.bound));
            }
        }
        if self.env.aggregates && self.aggregate_options(want).iter().any(|(_, w)| *w > 0.0) {
            // Aggregates replace leaves where allowed.
            let leaf_weight = w.variable + w.constant;
            let p = w.p_aggregate.clamp(0.0, 0.99);
            options.push((P::Aggregate, leaf_weight * p / (1.0 - p)));
        }
        options
    }

    fn aggregate_options(&self, want: ValueType) -> [(u8, f64); 8] {
        let a = &self.w.aggregate;
        let numeric = fits(want, ValueType::Numeric);
        let string = fits(want, ValueType::SimpleString);
        let n = |ok: bool, w: f64| if ok { w } else { 0.0 };
        [
            (0u8, n(numeric, a.count_star)),
            (1, n(numeric, a.count)),
            (2, n(numeric, a.sum)),
            (3, n(numeric, a.avg)),
            (4, a.min),
            (5, a.max),
            (6, a.sample),
            (7, n(string, a.group_concat)),
        ]
    }

    fn production(
        &mut self,
        production: Production,
        want: ValueType,
        min_prec: u8,
    ) -> (Expression, ValueType) {
        use Production as P;
        match production {
            P::Variable => self.variable_expr(want),
            P::Constant => self.constant_expr(want, None),
            P::Or => self.logical_expr(true, min_prec),
            P::And => self.logical_expr(false, min_prec),
            P::Not => self.not_expr(min_prec),
            P::Compare => self.compare_expr(min_prec),
            P::In => self.in_expr(false, min_prec),
            P::NotIn => self.in_expr(true, min_prec),
            P::Arithmetic => self.arithmetic_expr(min_prec),
            P::Unary => self.unary_expr(min_prec),
            P::SignedTail => self.signed_tail_expr(min_prec),
            P::Bracketed => self.bracketed_expr(want),
            P::BuiltIn => self.builtin_expr(want),
            P::Cast => self.cast_expr(want),
            P::ExtensionFunction => self.extension_call(want),
            P::Exists => self.exists_expr(false),
            P::NotExists => self.exists_expr(true),
            P::Bound => self.bound_expr(want),
            P::Aggregate => self.aggregate_expr(want),
        }
    }

    /// `a || b` or `a && b`.
    fn logical_expr(&mut self, or: bool, min_prec: u8) -> (Expression, ValueType) {
        let prec = if or { 1 } else { 2 };
        self.parenthesized_if(prec < min_prec, |g| {
            g.hit(if or {
                "ConditionalOrExpression"
            } else {
                "ConditionalAndExpression"
            });
            let lhs = Box::new(g.expr(ValueType::Boolean, prec));
            g.out.token(if or { "||" } else { "&&" });
            let rhs = Box::new(g.expr(ValueType::Boolean, prec + 1));
            let e = if or {
                Expression::Or(lhs, rhs)
            } else {
                Expression::And(lhs, rhs)
            };
            (e, ValueType::Boolean)
        })
    }

    /// `!a`
    fn not_expr(&mut self, min_prec: u8) -> (Expression, ValueType) {
        self.parenthesized_if(6 < min_prec, |g| {
            g.hit("UnaryNot");
            g.out.token("!");
            let e = g.expr(ValueType::Boolean, 7);
            (Expression::Not(Box::new(e)), ValueType::Boolean)
        })
    }

    /// `a op b` for a relational operator.
    fn compare_expr(&mut self, min_prec: u8) -> (Expression, ValueType) {
        self.parenthesized_if(3 < min_prec, |g| {
            g.hit("RelationalExpression");
            let ty = g.operand_type();
            let lhs = g.expr(ty, 4);
            // IRIs are mostly compared for (in)equality; ordering them is an error.
            let op = if ty == ValueType::Iri && g.flip(0.8) {
                if g.flip(0.5) {
                    CompareOp::Equal
                } else {
                    CompareOp::NotEqual
                }
            } else {
                *g.pick(&CompareOp::ALL).unwrap_or(&CompareOp::Equal)
            };
            g.out.token(op.as_str());
            let rhs = g.related_operand(&lhs, ty);
            (Expression::compare(lhs, op, rhs), ValueType::Boolean)
        })
    }

    /// `a IN (...)` or `a NOT IN (...)`.
    fn in_expr(&mut self, negated: bool, min_prec: u8) -> (Expression, ValueType) {
        self.parenthesized_if(3 < min_prec, |g| {
            g.hit(if negated { "NotIn" } else { "In" });
            let ty = g.operand_type();
            let lhs = g.expr(ty, 4);
            g.out.token(if negated { "NOT IN" } else { "IN" });
            let n = if g.flip(0.05) {
                0
            } else {
                g.count(0.5, g.limits.max_list)
            };
            let mut list = Vec::new();
            if n == 0 {
                g.out.token("()");
            } else {
                g.out.open("(");
                for i in 0..n {
                    if i > 0 {
                        g.out.glue(",");
                    }
                    list.push(g.related_operand_at(&lhs, ty, 1));
                }
                g.out.glue(")");
            }
            let e = if negated {
                Expression::NotIn(Box::new(lhs), list)
            } else {
                Expression::In(Box::new(lhs), list)
            };
            (e, ValueType::Boolean)
        })
    }

    /// `a + b`, `a - b`, `a * b` or `a / b`.
    fn arithmetic_expr(&mut self, min_prec: u8) -> (Expression, ValueType) {
        let ops = [(0u8, 1.0), (1, 1.0), (2, 1.0), (3, 0.7)];
        let op = self.choose(&ops).unwrap_or(0);
        let prec = if op < 2 { 4 } else { 5 };
        self.parenthesized_if(prec < min_prec, |g| {
            g.hit("ArithmeticExpression");
            let lhs = Box::new(g.expr(ValueType::Numeric, prec));
            g.out.token(["+", "-", "*", "/"][usize::from(op)]);
            let rhs = Box::new(g.expr(ValueType::Numeric, prec + 1));
            let e = match op {
                0 => Expression::Add(lhs, rhs),
                1 => Expression::Subtract(lhs, rhs),
                2 => Expression::Multiply(lhs, rhs),
                _ => Expression::Divide(lhs, rhs),
            };
            (e, ValueType::Numeric)
        })
    }

    /// `+a` or `-a`.
    fn unary_expr(&mut self, min_prec: u8) -> (Expression, ValueType) {
        self.parenthesized_if(6 < min_prec, |g| {
            g.hit("UnaryExpression");
            let plus = g.flip(0.3);
            g.out.token(if plus { "+" } else { "-" });
            let e = Box::new(g.expr(ValueType::Numeric, 7));
            let e = if plus {
                Expression::UnaryPlus(e)
            } else {
                Expression::UnaryMinus(e)
            };
            (e, ValueType::Numeric)
        })
    }

    /// `a -1 * b`: a signed literal directly after an expression.
    fn signed_tail_expr(&mut self, min_prec: u8) -> (Expression, ValueType) {
        self.parenthesized_if(4 < min_prec, |g| {
            g.hit("AdditiveSignedTail");
            let lhs = g.expr(ValueType::Numeric, 4);
            let literal = g.signed_number();
            g.emit(&literal);
            let mut tail = Vec::new();
            while tail.len() < 2 && g.coin(0.3) {
                let multiply = g.flip(0.5);
                g.out.token(if multiply { "*" } else { "/" });
                let op = if multiply {
                    MultiplicativeOp::Multiply
                } else {
                    MultiplicativeOp::Divide
                };
                tail.push((op, g.expr(ValueType::Numeric, 6)));
            }
            let e = Expression::SignedTail {
                lhs: Box::new(lhs),
                literal,
                tail,
            };
            (e, ValueType::Numeric)
        })
    }

    /// `( expr )` with explicit parentheses.
    fn bracketed_expr(&mut self, want: ValueType) -> (Expression, ValueType) {
        self.hit("BrackettedExpression");
        self.out.open("(");
        let (e, ty) = self.expr_typed(want, 1);
        self.out.glue(")");
        (Expression::Bracketed(Box::new(e)), ty)
    }

    /// `EXISTS { ... }` or `NOT EXISTS { ... }`.
    fn exists_expr(&mut self, negated: bool) -> (Expression, ValueType) {
        self.hit(if negated {
            "NotExistsFunc"
        } else {
            "ExistsFunc"
        });
        self.out
            .token(if negated { "NOT EXISTS" } else { "EXISTS" });
        let mark = self.context.len();
        let (g, _) = self.group(GroupKind::Exists);
        self.truncate_context(mark);
        let call = if negated {
            BuiltInCall::NotExists(Box::new(g))
        } else {
            BuiltInCall::Exists(Box::new(g))
        };
        (Expression::BuiltIn(call), ValueType::Boolean)
    }

    /// `BOUND(?v)`
    fn bound_expr(&mut self, want: ValueType) -> (Expression, ValueType) {
        let candidates = self.expression_variables();
        let may_introduce = matches!(self.env.bare, BareVars::Scope);
        if candidates.is_empty() && !may_introduce {
            return self.constant_expr(want, None);
        }
        self.hit("Bound");
        self.out.token("BOUND");
        self.out.glue_open("(");
        let v = match self.pick(&candidates).cloned() {
            Some(v) if !(may_introduce && self.flip(0.1)) => self.with_sigil(v),
            _ => self.take_fresh(),
        };
        self.emit(&v);
        self.out.glue(")");
        (
            Expression::BuiltIn(BuiltInCall::Bound(v)),
            ValueType::Boolean,
        )
    }

    /// Runs `f`, surrounding its output with parentheses if `paren`.
    fn parenthesized_if(
        &mut self,
        paren: bool,
        f: impl FnOnce(&mut Self) -> (Expression, ValueType),
    ) -> (Expression, ValueType) {
        if paren {
            self.out.open("(");
        }
        let result = f(self);
        if paren {
            self.out.glue(")");
        }
        result
    }

    /// The operand type of a comparison.
    fn operand_type(&mut self) -> ValueType {
        self.choose(&[
            (ValueType::Numeric, 3.0),
            (ValueType::String, 2.0),
            (ValueType::SimpleString, 1.0),
            (ValueType::DateTime, 0.5),
            (ValueType::Iri, 1.0),
            (ValueType::Boolean, 0.5),
            (ValueType::Any, 1.5),
        ])
        .unwrap_or(ValueType::Any)
    }

    /// The right-hand side of a comparison with `lhs`: often a data value of
    /// the variable `lhs`, which makes the comparison selective.
    fn related_operand(&mut self, lhs: &Expression, ty: ValueType) -> Expression {
        self.related_operand_at(lhs, ty, 4)
    }

    fn related_operand_at(&mut self, lhs: &Expression, ty: ValueType, min_prec: u8) -> Expression {
        if let Expression::Var(v) = lhs
            && self.flip(self.w.expression.p_data_value)
        {
            let v = v.clone();
            return self.constant_expr(ValueType::Any, Some(&v)).0;
        }
        self.expr(ty, min_prec)
    }

    fn variable_expr(&mut self, want: ValueType) -> (Expression, ValueType) {
        self.hit("Var");
        let ignore_types = self.flip(self.w.expression.p_ignore_types);
        let candidates = match (&self.env.bare, self.env.planning) {
            (BareVars::Only(list), _) => list.clone(),
            (BareVars::Scope, true) => {
                // Before the pattern: reference planned variables, or plan a new one.
                let pending = self.pending();
                let v = match self.pick(&pending).cloned() {
                    Some(v) if self.flip(0.7) => self.with_sigil(v),
                    _ => self.take_fresh(),
                };
                self.emit(&v);
                return (Expression::Var(v), ValueType::Any);
            }
            (BareVars::Scope, false) => {
                if self.flip(self.w.expression.p_unbound_variable) {
                    let v = self.take_fresh();
                    self.emit(&v);
                    return (Expression::Var(v), ValueType::Any);
                }
                let own = self.group_variables();
                if own.is_empty() || self.flip(self.w.expression.p_outer_variable) {
                    let outer = self.outer_variables();
                    if outer.is_empty() { own } else { outer }
                } else {
                    own
                }
            }
        };
        let typed: Vec<Variable> = candidates
            .iter()
            .filter(|v| {
                ignore_types
                    || fits(
                        want,
                        self.vars.types.get(*v).copied().unwrap_or(ValueType::Any),
                    )
            })
            .cloned()
            .collect();
        match self.pick(&typed).cloned() {
            Some(v) => {
                let v = self.with_sigil(v);
                self.emit(&v);
                let ty = self.vars.types.get(&v).copied().unwrap_or(ValueType::Any);
                (Expression::Var(v), ty)
            }
            None => self.constant_expr(want, None),
        }
    }

    /// A constant from the provider.
    fn constant_expr(
        &mut self,
        want: ValueType,
        related: Option<&Variable>,
    ) -> (Expression, ValueType) {
        self.hit("Constant");
        let mut accepts = TermKinds::empty();
        if fits(want, ValueType::Iri) {
            accepts |= TermKinds::IRI;
        }
        if want != ValueType::Iri {
            accepts |= TermKinds::LITERAL;
        }
        let related = related.cloned().or_else(|| {
            // Values of some variable in scope make realistic constants.
            if matches!(self.env.bare, BareVars::Scope) && !self.env.planning && self.flip(0.5) {
                let vars = self.group_variables();
                self.pick(&vars).cloned()
            } else {
                None
            }
        });
        let value = self.value(
            Slot::ExpressionOperand,
            accepts,
            want,
            related.as_ref(),
            None,
        );
        let term = match value {
            Value::Term(t) => t,
            Value::Var(_) => super::terms::fallback_term(&mut self.rng, accepts, want),
        };
        let ty = ValueType::of_term(&term);
        let e = match &term {
            Term::NamedNode(n) => Expression::Iri(self.iri(n.as_ref())),
            Term::Literal(l) => Expression::Literal(self.literal(l)),
            Term::BlankNode(_) => Expression::Literal(Literal::Boolean(true)),
        };
        self.emit(&e);
        (e, ty)
    }

    fn signed_number(&mut self) -> NumericLiteral {
        use rand::RngExt;
        let sign = if self.flip(0.5) { "-" } else { "+" };
        let n: u32 = self.rng.random_range(0..10);
        let text = match self.choose(&[(0, 3.0), (1, 1.0), (2, 1.0)]).unwrap_or(0) {
            0 => format!("{sign}{n}"),
            1 => format!("{sign}{n}.5"),
            _ => format!("{sign}{n}e0"),
        };
        NumericLiteral::new(text).expect("valid signed number")
    }

    /// A simple string literal written into the output.
    fn string_constant(&mut self, value: &str) -> Expression {
        let string = self.string(value);
        let e = Expression::Literal(Literal::Rdf(RdfLiteral {
            value: string,
            annotation: None,
        }));
        self.emit(&e);
        e
    }

    fn string_from(&mut self, choices: &[&str]) -> Expression {
        let s = *self.pick(choices).unwrap_or(&"");
        self.string_constant(s)
    }

    fn function_weight(&self, f: BuiltInFunction) -> f64 {
        if f.is_nondeterministic() && !self.config.nondeterministic_functions {
            return 0.0;
        }
        self.config
            .functions
            .get(f.keyword())
            .copied()
            .unwrap_or(1.0)
    }

    fn builtin_expr(&mut self, want: ValueType) -> (Expression, ValueType) {
        let options: Vec<(BuiltInFunction, f64)> = BuiltInFunction::ALL
            .iter()
            .map(|&f| (f, self.function_weight(f)))
            .filter(|&(f, _)| fits(want, signature(f).returns))
            .collect();
        let Some(f) = self.choose(&options) else {
            return self.constant_expr(want, None);
        };
        self.hit(f.keyword());
        let sig = signature(f);
        self.out.token(f.keyword());
        let mut args = Vec::new();
        let variadic = matches!(f, BuiltInFunction::Concat | BuiltInFunction::Coalesce);
        let optional = sig.optional.iter().take_while(|_| self.flip(0.4)).count();
        let arg_types: Vec<ValueType> = if variadic {
            let n = if self.flip(0.1) {
                0
            } else {
                self.count(0.5, self.limits.max_list)
            };
            let ty = if f == BuiltInFunction::Concat {
                ValueType::String
            } else {
                want
            };
            vec![ty; n as usize]
        } else {
            sig.params
                .iter()
                .chain(&sig.optional[..optional])
                .copied()
                .collect()
        };
        if arg_types.is_empty() {
            self.out.glue("()");
        } else {
            self.out.glue_open("(");
            for (i, &ty) in arg_types.iter().enumerate() {
                if i > 0 {
                    self.out.glue(",");
                }
                let ty = if ty == POLY && i > 0 { want } else { ty };
                let arg = self.builtin_argument(f, i, ty);
                args.push(arg);
            }
            self.out.glue(")");
        }
        let returns = if sig.returns == POLY {
            want
        } else {
            sig.returns
        };
        (
            Expression::BuiltIn(BuiltInCall::Call { function: f, args }),
            returns,
        )
    }

    /// The `i`-th argument of `f`, with special cases for arguments that are
    /// usually constants of a particular form.
    fn builtin_argument(&mut self, f: BuiltInFunction, i: usize, ty: ValueType) -> Expression {
        use BuiltInFunction as F;
        let special = self.flip(0.8);
        match (f, i) {
            (F::Regex | F::Replace, 1) if special => self.string_from(&REGEX_PATTERNS),
            (F::Regex, 2) | (F::Replace, 3) => self.string_from(&REGEX_FLAGS),
            (F::Replace, 2) if special => self.string_from(&["", "x", "$0", "[$1]"]),
            (F::LangMatches, 0) if special => {
                self.out.token("LANG");
                self.out.glue_open("(");
                let inner = self.expr(ValueType::Literal, 1);
                self.out.glue(")");
                Expression::BuiltIn(BuiltInCall::Call {
                    function: F::Lang,
                    args: vec![inner],
                })
            }
            (F::LangMatches, 1) if special => self.string_from(&LANGUAGE_RANGES),
            (F::StrLang, 1) if special => self.string_from(&LANGUAGE_TAGS),
            (F::StrDt, 1) if special => {
                let local = *self.pick(&DATATYPES).unwrap_or(&"string");
                let dt = NamedNode::new_unchecked(format!(
                    "{}{local}",
                    xsd::INTEGER.as_str().trim_end_matches("integer")
                ));
                let e = Expression::Iri(self.iri(dt.as_ref()));
                self.emit(&e);
                e
            }
            (F::Substr, 1 | 2) if special => {
                use rand::RngExt;
                let n: i64 = self.rng.random_range(-1..5);
                let e = Expression::Literal(Self::integer(n));
                self.emit(&e);
                e
            }
            (F::If, 0) => self.expr(ValueType::Boolean, 1),
            _ => self.expr(ty, 1),
        }
    }

    fn cast_expr(&mut self, want: ValueType) -> (Expression, ValueType) {
        let options: Vec<(usize, f64)> = CASTS
            .iter()
            .enumerate()
            .filter(|(_, (_, ty))| fits(want, *ty))
            .map(|(i, _)| (i, 1.0))
            .collect();
        let Some(i) = self.choose(&options) else {
            return self.constant_expr(want, None);
        };
        self.hit("Cast");
        let (local, ty) = CASTS[i];
        let base = xsd::STRING.as_str().trim_end_matches("string");
        let iri = self.iri(NamedNode::new_unchecked(format!("{base}{local}")).as_ref());
        self.emit(&iri);
        self.out.glue_open("(");
        let arg = self.expr(ValueType::Any, 1);
        self.out.glue(")");
        (
            Expression::Function(FunctionCall {
                iri,
                distinct: false,
                args: vec![arg],
            }),
            ty,
        )
    }

    fn extension_call(&mut self, want: ValueType) -> (Expression, ValueType) {
        let candidates: Vec<FunctionSpec> = self
            .functions
            .iter()
            .filter(|f| fits(want, f.returns))
            .cloned()
            .collect();
        let options: Vec<(usize, f64)> = candidates
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.weight))
            .collect();
        let Some(i) = self.choose(&options) else {
            return self.constant_expr(want, None);
        };
        self.hit("ExtensionFunction");
        let spec = &candidates[i];
        let iri = self.iri(spec.iri.as_ref());
        self.emit(&iri);
        let mut params = spec.params.clone();
        if spec.variadic
            && let Some(last) = params.pop()
        {
            let n = self.count(0.5, self.limits.max_list) - 1;
            params.extend(std::iter::repeat_n(last, n as usize));
        }
        let mut args = Vec::new();
        if params.is_empty() {
            self.out.glue("()");
        } else {
            self.out.glue_open("(");
            for (i, ty) in params.iter().enumerate() {
                if i > 0 {
                    self.out.glue(",");
                }
                args.push(self.expr(*ty, 1));
            }
            self.out.glue(")");
        }
        (
            Expression::Function(FunctionCall {
                iri,
                distinct: false,
                args,
            }),
            spec.returns,
        )
    }

    /// An aggregate; its argument may use the aggregate variables of the
    /// environment, and aggregates do not nest.
    pub fn aggregate_expr(&mut self, want: ValueType) -> (Expression, ValueType) {
        let options = self.aggregate_options(want);
        let Some(kind) = self.choose(&options) else {
            return self.constant_expr(want, None);
        };
        self.hit("Aggregate");
        let keyword = [
            "COUNT",
            "COUNT",
            "SUM",
            "AVG",
            "MIN",
            "MAX",
            "SAMPLE",
            "GROUP_CONCAT",
        ][kind as usize];
        self.out.token(keyword);
        self.out.glue_open("(");
        let distinct = self.flip(self.w.expression.p_distinct_aggregate);
        if distinct {
            self.out.token("DISTINCT");
        }
        let saved = self.env.clone();
        self.env.bare = self.env.aggregate_vars.clone();
        self.env.aggregates = false;
        let arg_type = match kind {
            2 | 3 => ValueType::Numeric,
            7 => ValueType::String,
            _ => ValueType::Any,
        };
        let (aggregate, ty) = match kind {
            0 => {
                self.out.token("*");
                (
                    Aggregate::Count {
                        distinct,
                        expr: None,
                    },
                    ValueType::Numeric,
                )
            }
            1 => {
                let e = self.expr(ValueType::Any, 1);
                (
                    Aggregate::Count {
                        distinct,
                        expr: Some(Box::new(e)),
                    },
                    ValueType::Numeric,
                )
            }
            7 => {
                let e = self.expr(arg_type, 1);
                let separator = if self.flip(self.w.expression.p_separator) {
                    self.out.token(";");
                    self.out.token("SEPARATOR");
                    self.out.token("=");
                    let s = *self.pick(&[",", " ", "", "|", "\n"]).unwrap_or(&",");
                    let s = self.string(s);
                    self.emit(&s);
                    Some(s)
                } else {
                    None
                };
                (
                    Aggregate::GroupConcat {
                        distinct,
                        expr: Box::new(e),
                        separator,
                    },
                    ValueType::SimpleString,
                )
            }
            _ => {
                let function = [
                    SetFunction::Sum,
                    SetFunction::Avg,
                    SetFunction::Min,
                    SetFunction::Max,
                    SetFunction::Sample,
                ][kind as usize - 2];
                let (e, arg_ty) = self.expr_typed(arg_type, 1);
                let ty = if kind <= 3 {
                    ValueType::Numeric
                } else {
                    arg_ty
                };
                (
                    Aggregate::Set {
                        function,
                        distinct,
                        expr: Box::new(e),
                    },
                    ty,
                )
            }
        };
        self.env = saved;
        self.out.glue(")");
        (Expression::BuiltIn(BuiltInCall::Aggregate(aggregate)), ty)
    }
}
