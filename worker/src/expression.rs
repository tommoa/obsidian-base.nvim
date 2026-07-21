//! Lexer and bounded recursive-descent parser for the supported Bases expression subset.

use crate::{
    error::{Result, WorkerError},
    limits::Limits,
};

#[derive(Clone, Debug, PartialEq)]
/// A scalar literal accepted in a Bases expression.
pub enum Literal {
    /// Quoted UTF-8 text.
    String(String),
    /// IEEE 754 numeric literal.
    Number(f64),
    /// Boolean literal.
    Boolean(bool),
    /// Null literal.
    Null,
}

#[derive(Clone, Debug, PartialEq)]
/// Abstract syntax tree for an expression evaluated by the query engine.
pub enum Expression {
    /// Literal value with no further evaluation.
    Literal(Literal),
    /// Named property, builtin, or special expression variable.
    Identifier(String),
    /// Property access on an evaluated receiver.
    Member(Box<Expression>, String),
    /// Numeric list indexing on an evaluated receiver.
    Index(Box<Expression>, Box<Expression>),
    /// Function or method invocation with expression arguments.
    Call(Box<Expression>, Vec<Expression>),
    /// Prefix logical-not or numeric-negation operation.
    Unary(String, Box<Expression>),
    /// Infix arithmetic, comparison, or logical operation.
    Binary(String, Box<Expression>, Box<Expression>),
}

#[derive(Clone, Debug)]
/// Lexical token consumed by the expression parser.
enum Token {
    /// Parsed numeric literal.
    Number(f64),
    /// Decoded JSON-style string literal.
    String(String),
    /// Identifier or keyword spelling.
    Identifier(String),
    /// Recognized punctuation or operator spelling.
    Operator(&'static str),
    /// Synthetic token marking the end of source input.
    Eof,
}

impl Token {
    fn spelling(&self) -> &str {
        match self {
            Self::Identifier(value) | Self::String(value) => value,
            Self::Operator(value) => value,
            _ => "",
        }
    }
}

pub fn parse_expression(source: &str, limits: Limits) -> Result<Expression> {
    if source.len() > limits.expression_bytes {
        return Err(WorkerError::new(
            "expression_too_large",
            "expression exceeds the configured size limit",
        ));
    }
    let tokens = lex(source)?;
    let mut parser = Parser {
        tokens,
        at: 0,
        nodes: 0,
        nesting: 0,
        limits,
    };
    let expression = parser.or()?;
    if !matches!(parser.peek(), Token::Eof) {
        return Err(WorkerError::new(
            "expression_parse",
            "unexpected trailing expression input",
        ));
    }
    Ok(expression)
}

fn lex(source: &str) -> Result<Vec<Token>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if at == bytes.len() {
            break;
        }
        let start = at;
        if bytes[at].is_ascii_digit() {
            at += 1;
            while at < bytes.len() && bytes[at].is_ascii_digit() {
                at += 1;
            }
            if at < bytes.len() && bytes[at] == b'.' {
                at += 1;
                let fraction = at;
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
                if at == fraction {
                    return unexpected(start);
                }
            }
            tokens.push(Token::Number(source[start..at].parse().map_err(|_| {
                WorkerError::new("expression_parse", "invalid number")
            })?));
            continue;
        }
        if bytes[at] == b'"' {
            at += 1;
            let mut escaped = false;
            while at < bytes.len() {
                let byte = bytes[at];
                at += 1;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    break;
                }
            }
            if bytes.get(at.wrapping_sub(1)) != Some(&b'"') {
                return unexpected(start);
            }
            let value: String = serde_json::from_str(&source[start..at])
                .map_err(|_| WorkerError::new("expression_parse", "invalid string"))?;
            tokens.push(Token::String(value));
            continue;
        }
        if bytes[at].is_ascii_alphabetic() || bytes[at] == b'_' {
            at += 1;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            tokens.push(Token::Identifier(source[start..at].to_owned()));
            continue;
        }
        let rest = &source[at..];
        let operator = [">=", "<=", "==", "!=", "&&", "||"]
            .into_iter()
            .find(|operator| rest.starts_with(operator))
            .or_else(|| {
                [
                    "-", "+", "*", "/", "<", ">", "!", ".", "(", ")", "[", "]", ",",
                ]
                .into_iter()
                .find(|operator| rest.starts_with(operator))
            })
            .ok_or_else(|| {
                WorkerError::new(
                    "expression_parse",
                    format!("unexpected token at character {start}"),
                )
            })?;
        at += operator.len();
        tokens.push(Token::Operator(operator));
    }
    tokens.push(Token::Eof);
    Ok(tokens)
}

fn unexpected<T>(at: usize) -> Result<T> {
    Err(WorkerError::new(
        "expression_parse",
        format!("unexpected token at character {at}"),
    ))
}

/// Stateful parser that tracks input position and configured complexity limits.
struct Parser {
    /// Complete token stream, including its final EOF marker.
    tokens: Vec<Token>,
    /// Index of the next token to inspect.
    at: usize,
    /// Number of AST nodes constructed so far.
    nodes: usize,
    /// Current parser-recursion depth for delimiters and prefix operators.
    nesting: usize,
    /// Resource limits enforced while parsing.
    limits: Limits,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.at]
    }

    fn accept(&mut self, value: &str) -> bool {
        if self.peek().spelling() == value {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, value: &str) -> Result<()> {
        if self.accept(value) {
            Ok(())
        } else {
            Err(WorkerError::new(
                "expression_parse",
                format!("expected {value}"),
            ))
        }
    }

    fn node(&mut self, value: Expression) -> Result<Expression> {
        self.nodes += 1;
        if self.nodes > self.limits.ast_nodes {
            return Err(WorkerError::new(
                "ast_too_large",
                "expression has too many nodes",
            ));
        }
        // Parentheses are limited while parsing; this also bounds left-associative trees built
        // iteratively by binary operators.
        if exceeds_depth(&value, self.limits.ast_depth) {
            return Err(WorkerError::new(
                "ast_too_deep",
                "expression nesting exceeds its limit",
            ));
        }
        Ok(value)
    }

    fn nested<T>(&mut self, parse: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.nesting += 1;
        if self.nesting > self.limits.ast_depth {
            self.nesting -= 1;
            return Err(WorkerError::new(
                "ast_too_deep",
                "expression nesting exceeds its limit",
            ));
        }
        let result = parse(self);
        self.nesting -= 1;
        result
    }

    fn primary(&mut self) -> Result<Expression> {
        let token = self.peek().clone();
        match token {
            Token::Number(value) => {
                self.at += 1;
                self.node(Expression::Literal(Literal::Number(value)))
            }
            Token::String(value) => {
                self.at += 1;
                self.node(Expression::Literal(Literal::String(value)))
            }
            Token::Identifier(value) => {
                self.at += 1;
                match value.as_str() {
                    "true" => self.node(Expression::Literal(Literal::Boolean(true))),
                    "false" => self.node(Expression::Literal(Literal::Boolean(false))),
                    "null" => self.node(Expression::Literal(Literal::Null)),
                    _ => {
                        safe_key(&value)?;
                        self.node(Expression::Identifier(value))
                    }
                }
            }
            _ if self.accept("(") => {
                let value = self.nested(Self::or)?;
                self.expect(")")?;
                Ok(value)
            }
            _ => Err(WorkerError::new("expression_parse", "expected a value")),
        }
    }

    fn postfix(&mut self) -> Result<Expression> {
        let mut value = self.primary()?;
        loop {
            if self.accept(".") {
                let Token::Identifier(property) = self.peek().clone() else {
                    return Err(WorkerError::new(
                        "expression_parse",
                        "expected property name",
                    ));
                };
                safe_key(&property)?;
                self.at += 1;
                value = self.node(Expression::Member(Box::new(value), property))?;
            } else if self.accept("[") {
                let index = self.nested(Self::or)?;
                self.expect("]")?;
                value = self.node(Expression::Index(Box::new(value), Box::new(index)))?;
            } else if self.accept("(") {
                let args = self.nested(|parser| {
                    let mut args = Vec::new();
                    if !parser.accept(")") {
                        loop {
                            args.push(parser.or()?);
                            if !parser.accept(",") {
                                break;
                            }
                        }
                        parser.expect(")")?;
                    }
                    Ok(args)
                })?;
                value = self.node(Expression::Call(Box::new(value), args))?;
            } else {
                return Ok(value);
            }
        }
    }

    fn unary(&mut self) -> Result<Expression> {
        if self.accept("!") || self.accept("not") {
            let operator = self.tokens[self.at - 1].spelling().to_owned();
            let argument = self.nested(Self::unary)?;
            self.node(Expression::Unary(operator, Box::new(argument)))
        } else if self.accept("-") {
            let argument = self.nested(Self::unary)?;
            self.node(Expression::Unary("-".into(), Box::new(argument)))
        } else {
            self.postfix()
        }
    }

    fn binary(
        &mut self,
        next: fn(&mut Self) -> Result<Expression>,
        operators: &[&str],
    ) -> Result<Expression> {
        let mut left = next(self)?;
        loop {
            let Some(operator) = operators
                .iter()
                .find(|operator| self.peek().spelling() == **operator)
            else {
                return Ok(left);
            };
            let operator = (*operator).to_owned();
            self.at += 1;
            let right = next(self)?;
            left = self.node(Expression::Binary(
                operator,
                Box::new(left),
                Box::new(right),
            ))?;
        }
    }

    fn multiplicative(&mut self) -> Result<Expression> {
        self.binary(Self::unary, &["*", "/"])
    }
    fn additive(&mut self) -> Result<Expression> {
        self.binary(Self::multiplicative, &["+", "-"])
    }
    fn comparison(&mut self) -> Result<Expression> {
        self.binary(Self::additive, &[">", ">=", "<", "<=", "==", "!="])
    }
    fn and(&mut self) -> Result<Expression> {
        self.binary(Self::comparison, &["&&", "and"])
    }
    fn or(&mut self) -> Result<Expression> {
        self.binary(Self::and, &["||", "or"])
    }
}

pub fn safe_key(key: &str) -> Result<&str> {
    if ["__proto__", "prototype", "constructor"].contains(&key) {
        Err(WorkerError::new(
            "unsafe_property",
            "unsafe property access",
        ))
    } else {
        Ok(key)
    }
}

fn exceeds_depth(expression: &Expression, maximum: usize) -> bool {
    let mut pending = vec![(expression, 1)];
    while let Some((expression, depth)) = pending.pop() {
        if depth > maximum {
            return true;
        }
        match expression {
            Expression::Literal(_) | Expression::Identifier(_) => {}
            Expression::Member(object, _) => pending.push((object, depth + 1)),
            Expression::Index(object, index) => {
                pending.push((object, depth + 1));
                pending.push((index, depth + 1));
            }
            Expression::Call(callee, args) => {
                pending.push((callee, depth + 1));
                pending.extend(args.iter().map(|argument| (argument, depth + 1)));
            }
            Expression::Unary(_, argument) => pending.push((argument, depth + 1)),
            Expression::Binary(_, left, right) => {
                pending.push((left, depth + 1));
                pending.push((right, depth + 1));
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_precedence_and_rejects_unsafe_members() {
        let expression = parse_expression("1 + 2 * 3 == 7 and !false", Limits::default());
        assert!(expression.is_ok());
        assert_eq!(
            parse_expression("file.__proto__", Limits::default())
                .unwrap_err()
                .code,
            "unsafe_property"
        );
    }

    #[test]
    fn enforces_depth_and_bytes() {
        assert_eq!(
            parse_expression(&format!("{}true", "!".repeat(65)), Limits::default())
                .unwrap_err()
                .code,
            "ast_too_deep"
        );
        let limits = Limits {
            expression_bytes: 1,
            ..Limits::default()
        };
        assert_eq!(
            parse_expression("true", limits).unwrap_err().code,
            "expression_too_large"
        );
    }

    #[test]
    fn rejects_adversarial_nesting_without_overflowing_the_stack() {
        let parentheses = format!("{}true{}", "(".repeat(10_000), ")".repeat(10_000));
        assert_eq!(
            parse_expression(&parentheses, Limits::default())
                .unwrap_err()
                .code,
            "ast_too_deep"
        );
        let binary = std::iter::repeat_n("true", 1_000)
            .collect::<Vec<_>>()
            .join(" + ");
        assert_eq!(
            parse_expression(&binary, Limits::default())
                .unwrap_err()
                .code,
            "ast_too_deep"
        );
    }
}
