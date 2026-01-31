//! Guard expression evaluation.
//!
//! Guards are boolean expressions that can reference the instance context.
//! The expression language supports:
//!
//! - `ctx.field` - context field access (truthy check)
//! - `ctx.field.nested` - nested field access
//! - `ctx.field == value` - equality (strings, numbers, booleans, null)
//! - `ctx.field != value` - inequality
//! - `ctx.field > value` - greater than (numbers)
//! - `ctx.field >= value` - greater or equal (numbers)
//! - `ctx.field < value` - less than (numbers)
//! - `ctx.field <= value` - less or equal (numbers)
//! - `!expr` - logical NOT
//! - `expr && expr` - logical AND (higher precedence than OR)
//! - `expr || expr` - logical OR
//! - `(expr)` - grouping for precedence control
//!
//! Examples:
//! - `ctx.enabled` - true if enabled is truthy
//! - `ctx.amount > 100 && ctx.approved` - compound condition
//! - `(ctx.a || ctx.b) && ctx.c` - grouping to change precedence
//! - `!ctx.disabled` - negation
//! - `ctx.status == "active"` - string comparison

use crate::error::CoreError;
use serde_json::Value;

/// A parsed guard expression.
#[derive(Debug, Clone)]
pub enum GuardExpr {
    /// Context field is truthy.
    Truthy(String),
    /// Context field is falsy.
    Falsy(String),
    /// Equality comparison.
    Eq(String, Value),
    /// Inequality comparison.
    Ne(String, Value),
    /// Greater than.
    Gt(String, f64),
    /// Greater or equal.
    Ge(String, f64),
    /// Less than.
    Lt(String, f64),
    /// Less or equal.
    Le(String, f64),
    /// Logical AND.
    And(Box<GuardExpr>, Box<GuardExpr>),
    /// Logical OR.
    Or(Box<GuardExpr>, Box<GuardExpr>),
    /// Logical NOT.
    Not(Box<GuardExpr>),
}

impl GuardExpr {
    /// Parses a guard expression from a string.
    pub fn parse(s: &str) -> Result<Self, CoreError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(CoreError::InvalidGuard {
                reason: "empty guard expression".to_string(),
            });
        }

        Parser::new(s).parse_expr()
    }

    /// Evaluates the guard against a context.
    pub fn evaluate(&self, ctx: &Value) -> bool {
        match self {
            GuardExpr::Truthy(field) => {
                let value = get_field(ctx, field);
                is_truthy(&value)
            }
            GuardExpr::Falsy(field) => {
                let value = get_field(ctx, field);
                !is_truthy(&value)
            }
            GuardExpr::Eq(field, expected) => {
                let value = get_field(ctx, field);
                values_equal(&value, expected)
            }
            GuardExpr::Ne(field, expected) => {
                let value = get_field(ctx, field);
                !values_equal(&value, expected)
            }
            GuardExpr::Gt(field, expected) => {
                let value = get_field(ctx, field);
                as_f64(&value).map(|v| v > *expected).unwrap_or(false)
            }
            GuardExpr::Ge(field, expected) => {
                let value = get_field(ctx, field);
                as_f64(&value).map(|v| v >= *expected).unwrap_or(false)
            }
            GuardExpr::Lt(field, expected) => {
                let value = get_field(ctx, field);
                as_f64(&value).map(|v| v < *expected).unwrap_or(false)
            }
            GuardExpr::Le(field, expected) => {
                let value = get_field(ctx, field);
                as_f64(&value).map(|v| v <= *expected).unwrap_or(false)
            }
            GuardExpr::And(left, right) => left.evaluate(ctx) && right.evaluate(ctx),
            GuardExpr::Or(left, right) => left.evaluate(ctx) || right.evaluate(ctx),
            GuardExpr::Not(inner) => !inner.evaluate(ctx),
        }
    }
}

fn get_field(ctx: &Value, field: &str) -> Value {
    let parts: Vec<&str> = field.split('.').collect();
    let mut current = ctx;

    for part in parts {
        match current {
            Value::Object(map) => {
                current = map.get(part).unwrap_or(&Value::Null);
            }
            _ => return Value::Null,
        }
    }

    current.clone()
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .map(|(a, b)| (a - b).abs() < f64::EPSILON)
            .unwrap_or(false),
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Simple recursive descent parser for guard expressions.
struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn parse_expr(&mut self) -> Result<GuardExpr, CoreError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<GuardExpr, CoreError> {
        let mut left = self.parse_and()?;
        self.skip_whitespace();

        while self.peek_str("||") {
            self.pos += 2;
            self.skip_whitespace();
            let right = self.parse_and()?;
            left = GuardExpr::Or(Box::new(left), Box::new(right));
            self.skip_whitespace();
        }

        Ok(left)
    }

    fn parse_and(&mut self) -> Result<GuardExpr, CoreError> {
        let mut left = self.parse_unary()?;
        self.skip_whitespace();

        while self.peek_str("&&") {
            self.pos += 2;
            self.skip_whitespace();
            let right = self.parse_unary()?;
            left = GuardExpr::And(Box::new(left), Box::new(right));
            self.skip_whitespace();
        }

        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<GuardExpr, CoreError> {
        self.skip_whitespace();

        if self.peek_char() == Some('!') {
            self.pos += 1;
            self.skip_whitespace();
            let inner = self.parse_unary()?; // Recursive to allow !!ctx.a
            return Ok(GuardExpr::Not(Box::new(inner)));
        }

        self.parse_primary()
    }

    fn parse_comparison(&mut self) -> Result<GuardExpr, CoreError> {
        self.skip_whitespace();
        let field = self.parse_field()?;
        self.skip_whitespace();

        // Check for comparison operator
        if self.peek_str("==") {
            self.pos += 2;
            self.skip_whitespace();
            let value = self.parse_value()?;
            return Ok(GuardExpr::Eq(field, value));
        }

        if self.peek_str("!=") {
            self.pos += 2;
            self.skip_whitespace();
            let value = self.parse_value()?;
            return Ok(GuardExpr::Ne(field, value));
        }

        if self.peek_str(">=") {
            self.pos += 2;
            self.skip_whitespace();
            let num = self.parse_number()?;
            return Ok(GuardExpr::Ge(field, num));
        }

        if self.peek_str("<=") {
            self.pos += 2;
            self.skip_whitespace();
            let num = self.parse_number()?;
            return Ok(GuardExpr::Le(field, num));
        }

        if self.peek_char() == Some('>') {
            self.pos += 1;
            self.skip_whitespace();
            let num = self.parse_number()?;
            return Ok(GuardExpr::Gt(field, num));
        }

        if self.peek_char() == Some('<') {
            self.pos += 1;
            self.skip_whitespace();
            let num = self.parse_number()?;
            return Ok(GuardExpr::Lt(field, num));
        }

        // No operator, just truthy check
        Ok(GuardExpr::Truthy(field))
    }

    fn parse_primary(&mut self) -> Result<GuardExpr, CoreError> {
        self.skip_whitespace();

        // Handle parenthesized expressions
        if self.peek_char() == Some('(') {
            self.pos += 1;
            let expr = self.parse_expr()?;
            self.skip_whitespace();
            if self.peek_char() != Some(')') {
                return Err(CoreError::InvalidGuard {
                    reason: "expected ')'".to_string(),
                });
            }
            self.pos += 1;
            return Ok(expr);
        }

        self.parse_comparison()
    }

    fn parse_field(&mut self) -> Result<String, CoreError> {
        let start = self.pos;

        // Expect "ctx." prefix
        if !self.peek_str("ctx.") {
            return Err(CoreError::InvalidGuard {
                reason: "field must start with 'ctx.'".to_string(),
            });
        }
        self.pos += 4;

        // Parse field name (including nested dots)
        while let Some(c) = self.peek_char() {
            if c.is_alphanumeric() || c == '_' || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }

        let field = &self.input[start + 4..self.pos];
        if field.is_empty() {
            return Err(CoreError::InvalidGuard {
                reason: "empty field name".to_string(),
            });
        }

        Ok(field.to_string())
    }

    fn parse_value(&mut self) -> Result<Value, CoreError> {
        self.skip_whitespace();

        // Try to parse as JSON value
        let rest = &self.input[self.pos..];

        // Boolean
        if rest.starts_with("true") {
            self.pos += 4;
            return Ok(Value::Bool(true));
        }
        if rest.starts_with("false") {
            self.pos += 5;
            return Ok(Value::Bool(false));
        }
        if rest.starts_with("null") {
            self.pos += 4;
            return Ok(Value::Null);
        }

        // String
        if rest.starts_with('"') {
            return self.parse_string_value();
        }

        // Number
        let num = self.parse_number()?;
        Ok(Value::Number(serde_json::Number::from_f64(num).unwrap()))
    }

    fn parse_string_value(&mut self) -> Result<Value, CoreError> {
        if self.peek_char() != Some('"') {
            return Err(CoreError::InvalidGuard {
                reason: "expected string".to_string(),
            });
        }
        self.pos += 1;

        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if c == '"' {
                let s = &self.input[start..self.pos];
                self.pos += 1;
                return Ok(Value::String(s.to_string()));
            }
            if c == '\\' {
                self.pos += 2; // Skip escape sequence
            } else {
                self.pos += 1;
            }
        }

        Err(CoreError::InvalidGuard {
            reason: "unterminated string".to_string(),
        })
    }

    fn parse_number(&mut self) -> Result<f64, CoreError> {
        self.skip_whitespace();
        let start = self.pos;

        // Optional negative sign
        if self.peek_char() == Some('-') {
            self.pos += 1;
        }

        // Integer part
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }

        // Optional decimal part
        if self.peek_char() == Some('.') {
            self.pos += 1;
            while let Some(c) = self.peek_char() {
                if c.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }

        let num_str = &self.input[start..self.pos];
        num_str.parse::<f64>().map_err(|_| CoreError::InvalidGuard {
            reason: format!("invalid number: '{}'", num_str),
        })
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek_char() {
            if c.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn peek_str(&self, s: &str) -> bool {
        self.input[self.pos..].starts_with(s)
    }
}

/// Guard evaluator with caching.
pub struct GuardEvaluator;

impl GuardEvaluator {
    /// Evaluates a guard expression against context.
    pub fn evaluate(guard: &GuardExpr, ctx: &Value) -> bool {
        guard.evaluate(ctx)
    }

    /// Evaluates an optional guard (None = always true).
    pub fn evaluate_opt(guard: Option<&GuardExpr>, ctx: &Value) -> bool {
        guard.map(|g| g.evaluate(ctx)).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_truthy_check() {
        let guard = GuardExpr::parse("ctx.enabled").unwrap();
        assert!(guard.evaluate(&json!({"enabled": true})));
        assert!(!guard.evaluate(&json!({"enabled": false})));
        assert!(!guard.evaluate(&json!({"enabled": null})));
        assert!(!guard.evaluate(&json!({})));
    }

    #[test]
    fn test_equality() {
        let guard = GuardExpr::parse("ctx.status == \"active\"").unwrap();
        assert!(guard.evaluate(&json!({"status": "active"})));
        assert!(!guard.evaluate(&json!({"status": "inactive"})));
    }

    #[test]
    fn test_numeric_comparison() {
        let guard = GuardExpr::parse("ctx.amount > 100").unwrap();
        assert!(guard.evaluate(&json!({"amount": 150})));
        assert!(!guard.evaluate(&json!({"amount": 50})));
        assert!(!guard.evaluate(&json!({"amount": 100})));

        let guard = GuardExpr::parse("ctx.amount >= 100").unwrap();
        assert!(guard.evaluate(&json!({"amount": 100})));
    }

    #[test]
    fn test_logical_and() {
        let guard = GuardExpr::parse("ctx.a && ctx.b").unwrap();
        assert!(guard.evaluate(&json!({"a": true, "b": true})));
        assert!(!guard.evaluate(&json!({"a": true, "b": false})));
        assert!(!guard.evaluate(&json!({"a": false, "b": true})));
    }

    #[test]
    fn test_logical_or() {
        let guard = GuardExpr::parse("ctx.a || ctx.b").unwrap();
        assert!(guard.evaluate(&json!({"a": true, "b": false})));
        assert!(guard.evaluate(&json!({"a": false, "b": true})));
        assert!(!guard.evaluate(&json!({"a": false, "b": false})));
    }

    #[test]
    fn test_nested_field() {
        let guard = GuardExpr::parse("ctx.order.paid").unwrap();
        assert!(guard.evaluate(&json!({"order": {"paid": true}})));
        assert!(!guard.evaluate(&json!({"order": {"paid": false}})));
        assert!(!guard.evaluate(&json!({"order": {}})));
    }

    #[test]
    fn test_complex_expression() {
        let guard = GuardExpr::parse("ctx.enabled && ctx.amount > 0 || ctx.override").unwrap();
        assert!(guard.evaluate(&json!({"enabled": true, "amount": 10, "override": false})));
        assert!(guard.evaluate(&json!({"enabled": false, "amount": 0, "override": true})));
        assert!(!guard.evaluate(&json!({"enabled": false, "amount": 0, "override": false})));
    }

    #[test]
    fn test_not() {
        let guard = GuardExpr::parse("!ctx.disabled").unwrap();
        assert!(guard.evaluate(&json!({"disabled": false})));
        assert!(!guard.evaluate(&json!({"disabled": true})));
    }

    #[test]
    fn test_inequality() {
        let guard = GuardExpr::parse("ctx.status != \"inactive\"").unwrap();
        assert!(guard.evaluate(&json!({"status": "active"})));
        assert!(!guard.evaluate(&json!({"status": "inactive"})));
    }

    #[test]
    fn test_less_than() {
        let guard = GuardExpr::parse("ctx.count < 10").unwrap();
        assert!(guard.evaluate(&json!({"count": 5})));
        assert!(!guard.evaluate(&json!({"count": 10})));
        assert!(!guard.evaluate(&json!({"count": 15})));
    }

    #[test]
    fn test_less_than_or_equal() {
        let guard = GuardExpr::parse("ctx.count <= 10").unwrap();
        assert!(guard.evaluate(&json!({"count": 5})));
        assert!(guard.evaluate(&json!({"count": 10})));
        assert!(!guard.evaluate(&json!({"count": 15})));
    }

    #[test]
    fn test_equality_with_number() {
        let guard = GuardExpr::parse("ctx.count == 42").unwrap();
        assert!(guard.evaluate(&json!({"count": 42})));
        assert!(!guard.evaluate(&json!({"count": 41})));
    }

    #[test]
    fn test_equality_with_boolean() {
        let guard = GuardExpr::parse("ctx.flag == true").unwrap();
        assert!(guard.evaluate(&json!({"flag": true})));
        assert!(!guard.evaluate(&json!({"flag": false})));

        let guard = GuardExpr::parse("ctx.flag == false").unwrap();
        assert!(guard.evaluate(&json!({"flag": false})));
        assert!(!guard.evaluate(&json!({"flag": true})));
    }

    #[test]
    fn test_equality_with_null() {
        let guard = GuardExpr::parse("ctx.value == null").unwrap();
        assert!(guard.evaluate(&json!({"value": null})));
        assert!(!guard.evaluate(&json!({"value": 123})));
    }

    #[test]
    fn test_negative_number() {
        let guard = GuardExpr::parse("ctx.temp > -10").unwrap();
        assert!(guard.evaluate(&json!({"temp": 5})));
        assert!(guard.evaluate(&json!({"temp": 0})));
        assert!(!guard.evaluate(&json!({"temp": -15})));
    }

    #[test]
    fn test_decimal_number() {
        let guard = GuardExpr::parse("ctx.rate >= 0.5").unwrap();
        assert!(guard.evaluate(&json!({"rate": 0.5})));
        assert!(guard.evaluate(&json!({"rate": 1.0})));
        assert!(!guard.evaluate(&json!({"rate": 0.3})));
    }

    #[test]
    fn test_deeply_nested_field() {
        let guard = GuardExpr::parse("ctx.order.customer.verified").unwrap();
        assert!(guard.evaluate(&json!({"order": {"customer": {"verified": true}}})));
        assert!(!guard.evaluate(&json!({"order": {"customer": {"verified": false}}})));
    }

    #[test]
    fn test_missing_nested_field() {
        let guard = GuardExpr::parse("ctx.order.customer.verified").unwrap();
        // Missing intermediate field should return null/false
        assert!(!guard.evaluate(&json!({"order": {}})));
        assert!(!guard.evaluate(&json!({})));
    }

    #[test]
    fn test_parentheses_with_not() {
        let guard = GuardExpr::parse("!(ctx.a && ctx.b)").unwrap();
        assert!(guard.evaluate(&json!({"a": true, "b": false})));
        assert!(guard.evaluate(&json!({"a": false, "b": true})));
        assert!(!guard.evaluate(&json!({"a": true, "b": true})));
    }

    #[test]
    fn test_general_parentheses() {
        // Without parentheses: ctx.a || ctx.b && ctx.c means ctx.a || (ctx.b && ctx.c)
        // With parentheses: (ctx.a || ctx.b) && ctx.c changes precedence
        let guard = GuardExpr::parse("(ctx.a || ctx.b) && ctx.c").unwrap();
        assert!(guard.evaluate(&json!({"a": true, "b": false, "c": true})));
        assert!(guard.evaluate(&json!({"a": false, "b": true, "c": true})));
        assert!(!guard.evaluate(&json!({"a": true, "b": true, "c": false}))); // c=false fails
        assert!(!guard.evaluate(&json!({"a": false, "b": false, "c": true}))); // both a,b false
    }

    #[test]
    fn test_nested_parentheses() {
        let guard = GuardExpr::parse("((ctx.a || ctx.b) && ctx.c) || ctx.d").unwrap();
        assert!(guard.evaluate(&json!({"a": true, "b": false, "c": true, "d": false})));
        assert!(guard.evaluate(&json!({"a": false, "b": false, "c": false, "d": true})));
        assert!(!guard.evaluate(&json!({"a": false, "b": false, "c": true, "d": false})));
    }

    #[test]
    fn test_parentheses_with_comparison() {
        let guard = GuardExpr::parse("(ctx.a > 10 || ctx.b < 5) && ctx.c").unwrap();
        assert!(guard.evaluate(&json!({"a": 15, "b": 10, "c": true})));
        assert!(guard.evaluate(&json!({"a": 5, "b": 3, "c": true})));
        assert!(!guard.evaluate(&json!({"a": 15, "b": 10, "c": false})));
    }

    #[test]
    fn test_double_not() {
        let guard = GuardExpr::parse("!!ctx.a").unwrap();
        assert!(guard.evaluate(&json!({"a": true})));
        assert!(!guard.evaluate(&json!({"a": false})));
    }

    #[test]
    fn test_complex_and_or_chain() {
        // && has higher precedence than ||
        // ctx.a && ctx.b || ctx.c is parsed as (ctx.a && ctx.b) || ctx.c
        // Use explicit parentheses to change: ctx.a && (ctx.b || ctx.c)
        let guard = GuardExpr::parse("ctx.a && ctx.b || ctx.c").unwrap();
        assert!(guard.evaluate(&json!({"a": true, "b": true, "c": false})));
        assert!(guard.evaluate(&json!({"a": false, "b": false, "c": true})));
        assert!(!guard.evaluate(&json!({"a": true, "b": false, "c": false})));
    }

    #[test]
    fn test_truthy_values() {
        let guard = GuardExpr::parse("ctx.value").unwrap();

        // Truthy
        assert!(guard.evaluate(&json!({"value": true})));
        assert!(guard.evaluate(&json!({"value": 1})));
        assert!(guard.evaluate(&json!({"value": "non-empty"})));
        assert!(guard.evaluate(&json!({"value": [1]})));
        assert!(guard.evaluate(&json!({"value": {"key": "val"}})));

        // Falsy
        assert!(!guard.evaluate(&json!({"value": false})));
        assert!(!guard.evaluate(&json!({"value": 0})));
        assert!(!guard.evaluate(&json!({"value": ""})));
        assert!(!guard.evaluate(&json!({"value": []})));
        assert!(!guard.evaluate(&json!({"value": {}})));
        assert!(!guard.evaluate(&json!({"value": null})));
    }

    #[test]
    fn test_parse_empty_expression() {
        let result = GuardExpr::parse("");
        assert!(result.is_err());

        let result = GuardExpr::parse("   ");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_field_prefix() {
        let result = GuardExpr::parse("foo.bar");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_field_name() {
        let result = GuardExpr::parse("ctx.");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_unclosed_parenthesis() {
        let result = GuardExpr::parse("!(ctx.a && ctx.b");
        assert!(result.is_err());

        let result = GuardExpr::parse("(ctx.a && ctx.b");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_unterminated_string() {
        let result = GuardExpr::parse("ctx.name == \"unclosed");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_number() {
        let result = GuardExpr::parse("ctx.value > abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_guard_evaluator_helper() {
        let guard = GuardExpr::parse("ctx.ok").unwrap();
        let ctx = json!({"ok": true});

        assert!(GuardEvaluator::evaluate(&guard, &ctx));
        assert!(GuardEvaluator::evaluate_opt(Some(&guard), &ctx));
        assert!(GuardEvaluator::evaluate_opt(None, &ctx)); // None means always true
    }

    #[test]
    fn test_comparison_with_non_numeric() {
        let guard = GuardExpr::parse("ctx.value > 10").unwrap();
        // String value should fail numeric comparison
        assert!(!guard.evaluate(&json!({"value": "not a number"})));
        assert!(!guard.evaluate(&json!({"value": null})));
    }

    #[test]
    fn test_not_with_comparison() {
        let guard = GuardExpr::parse("!(ctx.amount > 100)").unwrap();
        assert!(guard.evaluate(&json!({"amount": 50})));
        assert!(!guard.evaluate(&json!({"amount": 150})));
    }
}
