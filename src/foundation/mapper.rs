#![allow(clippy::empty_docs)]

use crate::foundation::filter::ValueAccessor;
use crate::foundation::mapper::EvalResult::{AnyValue, Failure, Named, Number, Undefined, Value};
use crate::tuliprox_error::{create_tuliprox_error_result, info_err, TuliproxError, TuliproxErrorKind};
use crate::utils::Capitalize;
use log::{debug, error, trace};
use pest::iterators::Pair;
use pest::Parser;
use regex::Regex;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

#[derive(Parser)]
#[grammar_inline = r##"
WHITESPACE = _{ " " | "\t"}
regex_op =  _{ "~" }
null = { "null" }
identifier = @{ !null ~ (ASCII_ALPHANUMERIC | "_")+ }
var_access = { identifier ~ ("." ~ identifier)? }
string_literal = @{ "\"" ~ ( "\\\\" | "\\\"" | "\\n" | "\\t" | "\\r" | (!"\"" ~ ANY) )* ~ "\"" }
number = @{ "-"? ~ ASCII_DIGIT+ ~ ("." ~ ASCII_DIGIT+)? }
number_range_from = { number ~ ".." }
number_range_to = { ".." ~ number }
number_range_full = { number ~ ".." ~ number }
number_range_eq = { number }
number_range = _{ number_range_full | number_range_from | number_range_to | number_range_eq}
field = { ^"name" | ^"title" | ^"caption" | ^"group" | ^"id" | ^"chno" | ^"logo" | ^"logo_small" | ^"parent_code" | ^"audio_track" | ^"time_shift" | ^"rec" | ^"url" | ^"epg_channel_id" | ^"epg_id" }
field_access = _{ "@" ~ field }
regex_source = _{ field_access | identifier }
regex_expr = { regex_source ~ regex_op ~ string_literal }
expression = { map_block | match_block | function_call | regex_expr | string_literal | number | var_access | field_access | null }
function_name = { "concat" | "uppercase" | "lowercase" | "capitalize" | "trim" | "print" | "number" }
function_call = { function_name ~ "(" ~ (expression ~ ("," ~ expression)*)? ~ ")" }
any_match = { "_" }
match_case_key = { any_match | identifier }
match_case_key_list = { match_case_key ~ ("," ~ match_case_key)* }
match_case = { match_case_key_list ~ "=>" ~ expression | "(" ~ match_case_key_list ~ ")" ~ "=>" ~ expression }
match_block = { "match" ~  "{" ~ NEWLINE* ~ (match_case ~ ("," ~ NEWLINE* ~ match_case)*)? ~ ","? ~ NEWLINE* ~ "}" }
map_case_key_list = { string_literal ~ ("|" ~ string_literal)* }
map_case_key = { any_match | number_range | map_case_key_list }
map_case = { map_case_key ~ "=>" ~ expression }
map_key = { identifier }
map_block = { "map" ~ map_key ~ "{" ~ NEWLINE* ~ (map_case ~ ("," ~ NEWLINE* ~ map_case)*)? ~ ","? ~ NEWLINE* ~ "}" }
assignment = { (field_access | identifier) ~ "=" ~ expression }
statement = { assignment | expression }
comment = _{ "#" ~ (!NEWLINE ~ ANY)* }
statement_reparator = _{ ";" | NEWLINE }
statements = _{ (statement_reparator* ~ (statement | comment))* ~ statement_reparator* }
main = { SOI ~ statements? ~ EOI }
"##]
struct MapperParser;

#[derive(Debug, Clone)]
enum MatchCaseKey {
    Identifier(String),
    AnyMatch,
}

#[derive(Debug, Clone)]
struct MatchCase {
    pub keys: Vec<MatchCaseKey>,
    pub expression: Expression,
}

#[derive(Debug, Clone)]
enum MapCaseKey {
    Text(String),
    RangeFrom(f64),
    RangeTo(f64),
    RangeFull(f64, f64),
    RangeEq(f64),
    AnyMatch,
}

#[derive(Debug, Clone)]
struct MapCase {
    pub keys: Vec<MapCaseKey>,
    pub expression: Expression,
}

#[derive(Debug, Clone)]
enum MapKey {
    Identifier(String),
}


#[derive(Debug, Clone)]
enum BuiltInFunction {
    Concat,
    Uppercase,
    Lowercase,
    Capitalize,
    Trim,
    Print,
    ToNumber,
}

impl FromStr for BuiltInFunction {
    type Err = TuliproxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "concat" => Ok(Self::Concat),
            "capitalize" => Ok(Self::Capitalize),
            "lowercase" => Ok(Self::Lowercase),
            "uppercase" => Ok(Self::Uppercase),
            "trim" => Ok(Self::Trim),
            "print" => Ok(Self::Print),
            "number" => Ok(Self::ToNumber),
            _ => create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unknown function {}", s),
        }
    }
}

#[derive(Debug, Clone)]
enum RegexSource {
    Identifier(String),
    Field(String),
}

#[derive(Debug, Clone)]
enum Expression {
    Identifier(String),
    StringLiteral(String),
    NumberLiteral(f64),
    FieldAccess(String),
    VarAccess(String, String),
    RegexExpr { field: RegexSource, pattern: String, re_pattern: Regex },
    FunctionCall { name: BuiltInFunction, args: Vec<Expression> },
    MatchBlock(Vec<MatchCase>),
    MapBlock { key: MapKey, cases: Vec<MapCase> },
    NullValue,
}

#[derive(Debug, Clone)]
enum AssignmentTarget {
    Identifier(String),
    Field(String),
}

#[derive(Debug, Clone)]
enum Statement {
    Assignment { target: AssignmentTarget, expr: Expression },
    Expression(Expression),
    Comment, //(String),
}

#[derive(Debug, Clone)]
pub struct MapperScript {
    statements: Vec<Statement>,
}

impl MapperScript {
    pub fn eval(&self, setter: &mut ValueAccessor) -> Result<(), TuliproxError> {
        let ctx = &mut MapperContext::new();
        self.eval_with_context(ctx, setter)?;
        Ok(())
    }

    pub fn eval_with_context(&self, ctx: &mut MapperContext, setter: &mut ValueAccessor) -> Result<(), TuliproxError> {
        for stmt in &self.statements {
            stmt.eval(ctx, setter)?;
        }
        Ok(())
    }
}

impl Statement {
    pub fn eval(&self, ctx: &mut MapperContext, setter: &mut ValueAccessor) -> Result<(), TuliproxError> {
        match self {
            Statement::Assignment { target, expr } => {
                let val = expr.eval(ctx, setter);
                match target {
                    AssignmentTarget::Identifier(name) => {
                        ctx.set_var(name, val);
                    }
                    AssignmentTarget::Field(name) => {
                        match val {
                            Value(content) => {
                                setter.set(name, content.as_str());
                            }
                            Number(num) => {
                                setter.set(name, format_number(num).as_str());
                            }
                            Named(pairs) => {
                                let mut result = String::with_capacity(128);
                                for (i, (key, value)) in pairs.iter().enumerate() {
                                    result.push_str(key);
                                    result.push_str(": ");
                                    result.push_str(value);
                                    if i < pairs.len() - 1 {
                                        result.push_str(", ");
                                    }
                                }
                                setter.set(name, &result);
                            }
                            Undefined | AnyValue => {}
                            Failure(err) => {
                                return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Failed to set field {} value: {}", name, err);
                            }
                        }
                    }
                }
            }
            Statement::Expression(expr) => {
                let result = expr.eval(ctx, setter);
                if let Failure(err) = &result {
                    debug!("{err}");
                    // } else {
                    //     trace!("Ignoring result {result:?}");
                }
            }
            Statement::Comment => {}
        }
        Ok(())
    }
}

impl MapperScript {
    fn validate_expr(expr: &Expression, identifiers: &mut HashSet<&str>) -> Result<(), TuliproxError> {
        match expr {
            Expression::Identifier(ident)
            | Expression::VarAccess(ident, _) => {
                if !identifiers.contains(ident.as_str()) {
                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Identifier unknown {}", ident);
                }
            }
            Expression::NullValue
            | Expression::FieldAccess(_)
            | Expression::StringLiteral(_)
            | Expression::NumberLiteral(_) => {}
            Expression::RegexExpr { field, pattern: _pattern, re_pattern: _re_pattern } => {
                match field {
                    RegexSource::Identifier(ident) => {
                        if !identifiers.contains(ident.as_str()) {
                            return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Identifier unknown {}", ident);
                        }
                    }
                    RegexSource::Field(_) => {}
                }
            }
            Expression::FunctionCall { name: _name, args } => {
                for arg in args {
                    MapperScript::validate_expr(arg, identifiers)?;
                }
            }
            Expression::MatchBlock(cases) => {
                let mut case_keys = HashSet::new();
                for match_case in cases {
                    let mut any_match_count = 0;
                    let mut identifier_key = String::with_capacity(56);
                    for identifier in &match_case.keys {
                        match identifier {
                            MatchCaseKey::Identifier(ident) => {
                                if !identifiers.contains(ident.as_str()) {
                                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Identifier unknown {}", ident);
                                }
                                identifier_key.push_str(ident.as_str());
                                identifier_key.push_str(", ");
                            }
                            MatchCaseKey::AnyMatch => {
                                any_match_count += 1;
                                if any_match_count > 1 {
                                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Match case can only have one '_'");
                                }
                                identifier_key.push_str("_, ");
                            }
                        }
                    }
                    if case_keys.contains(&identifier_key) {
                        return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Duplicate case {}", identifier_key);
                    }
                    case_keys.insert(identifier_key);
                    MapperScript::validate_expr(&match_case.expression, identifiers)?;
                }
            }
            Expression::MapBlock { key, cases } => {
                match key {
                    MapKey::Identifier(ident) => {
                        if !identifiers.contains(ident.as_str()) {
                            return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Identifier unknown {}", ident);
                        }
                    }
                }
                let mut case_keys = HashSet::new();
                let mut any_match_count = 0;
                for map_case in cases {
                    for key in &map_case.keys {
                        match key {
                            MapCaseKey::Text(value) => {
                                if case_keys.contains(value.as_str()) {
                                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Duplicate case {}", value);
                                }
                                case_keys.insert(value.as_str());
                            }
                            MapCaseKey::RangeEq(_)
                            | MapCaseKey::RangeTo(_)
                            | MapCaseKey::RangeFrom(_) => {}
                            MapCaseKey::RangeFull(from, to) => {
                                if *from > *to {
                                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Invalid range {from}..{to}");
                                }
                            }
                            MapCaseKey::AnyMatch => {
                                any_match_count += 1;
                                if any_match_count > 1 {
                                    return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Map case can only have one '_'");
                                }
                            }
                        }
                    }
                    MapperScript::validate_expr(&map_case.expression, identifiers)?;
                }
            }
        }
        Ok(())
    }

    fn validate(statements: &Vec<Statement>) -> Result<(), TuliproxError> {
        let mut identifiers: HashSet<&str> = HashSet::new();
        for stmt in statements {
            match stmt {
                Statement::Assignment { target, expr: value } => {
                    match target {
                        AssignmentTarget::Identifier(ident) => {
                            identifiers.insert(ident.as_str());
                        }
                        AssignmentTarget::Field(_) => {}
                    }
                    MapperScript::validate_expr(value, &mut identifiers)?;
                }
                Statement::Expression(expr) => {
                    MapperScript::validate_expr(expr, &mut identifiers)?;
                }
                Statement::Comment => {}
            }
        }
        Ok(())
    }

    pub fn parse(input: &str) -> Result<Self, TuliproxError> {
        let mut parsed = MapperParser::parse(Rule::main, input).map_err(|e| info_err!(e.to_string()))?;
        let program_pair = parsed.next().unwrap();
        let mut statements = Vec::new();
        for stmt_pair in program_pair.into_inner() {
            if let Some(stmt) = Self::parse_statement(stmt_pair)? {
                statements.push(stmt);
            }
        }
        MapperScript::validate(&statements)?;
        Ok(Self { statements })
    }
    fn parse_statement(pair: Pair<Rule>) -> Result<Option<Statement>, TuliproxError> {
        match pair.as_rule() {
            Rule::statement => {
                let inner = pair.into_inner().next().unwrap();
                match inner.as_rule() {
                    Rule::assignment => Ok(Some(MapperScript::parse_assignment(inner)?)),
                    Rule::expression => Ok(Some(Statement::Expression(MapperScript::parse_expression(inner)?))),
                    _ => {
                        error!("Unknown statement rule: {:?}", inner.as_rule());
                        Ok(None)
                    }
                }
            }
            Rule::comment => Ok(Some(Statement::Comment /*(pair.as_str().trim().to_string())*/)),
            _ => Ok(None),
        }
    }

    fn parse_assignment(pair: Pair<Rule>) -> Result<Statement, TuliproxError> {
        let mut inner = pair.into_inner();
        let name = inner.next().unwrap();
        let target = match name.as_rule() {
            Rule::identifier => AssignmentTarget::Identifier(name.as_str().to_string()),
            Rule::field => AssignmentTarget::Field(name.as_str().to_string()),
            _ => return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Assignment target isn't supported {}", name.as_str()),
        };
        let next = inner.next().unwrap();
        let value = MapperScript::parse_expression(next)?;
        Ok(Statement::Assignment { target, expr: value })
    }

    fn parse_match_case_key(pair: Pair<Rule>) -> Result<MatchCaseKey, TuliproxError> {
        let inner = pair.into_inner().next().unwrap();
        match inner.as_rule() {
            Rule::identifier => Ok(MatchCaseKey::Identifier(inner.as_str().to_string())),
            Rule::any_match => Ok(MatchCaseKey::AnyMatch),
            _ => create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unexpected match_key: {:?}", inner.as_rule()),
        }
    }

    fn parse_match_case(pair: Pair<Rule>) -> Result<MatchCase, TuliproxError> {
        let mut inner = pair.into_inner();

        let first = inner.next().unwrap();

        let identifiers = match first.as_rule() {
            Rule::match_case_key => {
                vec![MapperScript::parse_match_case_key(first)?]
            }
            Rule::match_case_key_list => {
                let mut matches = vec![];
                for arm in first.into_inner() {
                    if arm.as_rule() != Rule::WHITESPACE {
                        match MapperScript::parse_match_case_key(arm)? {
                            MatchCaseKey::Identifier(ident) => matches.push(MatchCaseKey::Identifier(ident)),
                            MatchCaseKey::AnyMatch => matches.push(MatchCaseKey::AnyMatch),
                        }
                    }
                }
                // we don't allow inside multi match keys AnyMatch
                if matches.len() > 1 && matches.iter().filter(|&m| matches!(m, &MatchCaseKey::AnyMatch)).count() > 0 {
                    return Err(info_err!("Unexpected match case key: _".to_string()));
                }
                matches
            }
            _ => return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unexpected match arm input: {:?}", first.as_rule()),
        };

        let expr = MapperScript::parse_expression(inner.next().unwrap())?;

        Ok(MatchCase {
            keys: identifiers,
            expression: expr,
        })
    }

    fn parse_map_case_key(pair: Pair<Rule>) -> Result<Vec<MapCaseKey>, TuliproxError> {
        let inner = pair.into_inner().next().unwrap();
        match inner.as_rule() {
            Rule::map_case_key_list => {
                let mut matches = vec![];
                for arm in inner.into_inner() {
                    match arm.as_rule() {
                        Rule::string_literal => {
                            let raw = arm.as_str().to_string();
                            // remove quotes
                            let content = &raw[1..raw.len() - 1];
                            matches.push(MapCaseKey::Text(content.to_string()));
                        }
                        _ => return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unexpected map key: {:?}", arm.as_rule()),
                    }
                }
                Ok(matches)
            }
            Rule::number_range_full => {
                let mut inner = inner.into_inner();
                let start = inner.next().unwrap().as_str().parse::<f64>().unwrap();
                let end = inner.next().unwrap().as_str().parse::<f64>().unwrap();
                Ok(vec![MapCaseKey::RangeFull(start, end)])
            }
            Rule::number_range_from => {
                let mut inner = inner.into_inner();
                let start = inner.next().unwrap().as_str().parse::<f64>().unwrap();
                Ok(vec![MapCaseKey::RangeFrom(start)])
            }
            Rule::number_range_to => {
                let mut inner = inner.into_inner();
                let to = inner.next().unwrap().as_str().parse::<f64>().unwrap();
                Ok(vec![MapCaseKey::RangeTo(to)])
            }
            Rule::number_range_eq => {
                let mut inner = inner.into_inner();
                let num = inner.next().unwrap().as_str().parse::<f64>().unwrap();
                Ok(vec![MapCaseKey::RangeEq(num)])
            }
            Rule::any_match => Ok(vec![MapCaseKey::AnyMatch]),
            _ => create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unexpected map key: {:?}", inner.as_rule()),
        }
    }

    fn parse_map_case(pair: Pair<Rule>) -> Result<MapCase, TuliproxError> {
        let mut inner = pair.into_inner();

        let first = inner.next().unwrap();

        let identifier = match first.as_rule() {
            Rule::map_case_key => {
                MapperScript::parse_map_case_key(first)?
            }
            _ => return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unexpected match arm input: {:?}", first.as_rule()),
        };

        let expr = MapperScript::parse_expression(inner.next().unwrap())?;

        Ok(MapCase {
            keys: identifier,
            expression: expr,
        })
    }

    fn parse_expression(pair: Pair<Rule>) -> Result<Expression, TuliproxError> {
        match pair.as_rule() {
            Rule::field => {
                Ok(Expression::FieldAccess(pair.as_str().to_string()))
            }
            Rule::var_access => {
                let text = pair.as_str();
                if text.contains('.') {
                    let splitted: Vec<&str> = text.splitn(2, '.').collect();
                    Ok(Expression::VarAccess(splitted[0].to_string(), splitted[1].to_string()))
                } else {
                    Ok(Expression::Identifier(text.to_string()))
                }
            }

            Rule::string_literal => {
                let raw = pair.as_str();
                // remove quotes
                let content = &raw[1..raw.len() - 1];
                Ok(Expression::StringLiteral(content.to_string()))
            }

            Rule::number => {
                let raw = pair.as_str();
                if let Number(val) = to_number(raw) {
                    Ok(Expression::NumberLiteral(val))
                } else {
                    create_tuliprox_error_result!(TuliproxErrorKind::Info, "Invalid number {raw}")
                }
            }

            Rule::regex_expr => {
                let mut inner = pair.into_inner();
                let first = inner.next().unwrap();
                let field = match first.as_rule() {
                    Rule::identifier => RegexSource::Identifier(first.as_str().to_string()),
                    Rule::field => RegexSource::Field(first.as_str().to_string()),
                    _ => return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Invalid regex source {}", first.as_str().to_string()),
                };
                let pattern_raw = inner.next().unwrap().as_str();
                let pattern = &pattern_raw[1..pattern_raw.len() - 1]; // Strip quotes
                match Regex::new(pattern) {
                    Ok(re) => Ok(Expression::RegexExpr { field, pattern: pattern.to_string(), re_pattern: re }),
                    Err(_) => create_tuliprox_error_result!(TuliproxErrorKind::Info, "Invalid regex {}", pattern),
                }
            }

            Rule::function_call => {
                let mut inner = pair.into_inner();
                let fn_name = inner.next().unwrap().as_str().to_string();
                let mut args = vec![];
                for arg in inner {
                    args.push(MapperScript::parse_expression(arg)?);
                }
                let name = BuiltInFunction::from_str(&fn_name)?;
                Ok(Expression::FunctionCall { name, args })
            }

            Rule::match_block => {
                let case_pairs = pair.into_inner();
                let mut cases = vec![];
                for case in case_pairs {
                    cases.push(MapperScript::parse_match_case(case)?);
                }
                Ok(Expression::MatchBlock(cases))
            }

            Rule::map_block => {
                let mut inner = pair.into_inner();
                let first = inner.next().unwrap();
                let key = match first.as_rule() {
                    Rule::map_key => {
                        MapKey::Identifier(first.as_str().to_string())
                    }
                    _ => return create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unexpected map case key: {:?}", first.as_rule()),
                };
                let mut cases = vec![];
                for case in inner {
                    cases.push(MapperScript::parse_map_case(case)?);
                }
                Ok(Expression::MapBlock { key, cases })
            }
            Rule::null => {
                Ok(Expression::NullValue)
            }

            Rule::expression => {
                let inner = pair.into_inner().next().unwrap();
                MapperScript::parse_expression(inner)
            }

            _ => create_tuliprox_error_result!(TuliproxErrorKind::Info, "Unknown expression rule: {:?}", pair.as_rule()),
        }
    }
}

pub struct MapperContext {
    variables: HashMap<String, EvalResult>,
}

impl MapperContext {
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
        }
    }

    fn set_var(&mut self, name: &str, value: EvalResult) {
        self.variables.insert(name.to_string(), value);
    }

    fn has_var(&self, name: &str) -> bool {
        self.variables.contains_key(name)
    }

    fn get_var(&self, name: &str) -> &EvalResult {
        self.variables.get(name).unwrap_or(&Undefined)
    }
}

impl Default for MapperContext {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
enum EvalResult {
    Undefined,
    Value(String),
    Number(f64),
    Named(Vec<(String, String)>),
    AnyValue,
    Failure(String),
}

fn to_number(value: &str) -> EvalResult {
    match value.parse::<f64>() {
        Ok(num) => Number(num),
        Err(_) => Failure(format!("Invalid number: {value}")),
    }
}

fn compare_number(a: f64, b: f64) -> Ordering {
    let epsilon = 1e-3; // = 0.001

    if (a - b).abs() < epsilon {
        Ordering::Equal
    } else if a < b {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

#[allow(clippy::cast_possible_truncation)]
fn format_number(num: f64) -> String {
    let epsilon = 1e-3; // = 0.001

    if num.fract().abs() < epsilon {
        format!("{}", num as i64)
    } else {
        format!("{num}")
    }
}

fn compare_tuple_vec<'a>(
    a: &'a [(String, String)],
    b: &'a [(String, String)],
) -> bool {
    fn to_map(vec: &[(String, String)]) -> HashMap<&str, &str> {
        vec.iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    to_map(a) == to_map(b)
}

fn match_number(num: f64, s: &str) -> bool {
    if let Ok(val) = s.parse::<f64>() {
        return compare_number(num, val) == Ordering::Equal;
    }
    false
}

fn cmp_number(num: f64, s: &str) -> Option<Ordering> {
    if let Ok(val) = s.parse::<f64>() {
        return Some(compare_number(num, val));
    }
    None
}


impl EvalResult {
    fn matches(&self, other: &EvalResult) -> bool {
        match (self, other) {
            (AnyValue, _) | (_, AnyValue) => true,
            (Value(a), Value(b)) => a == b,
            (Number(a), Value(b)) => match_number(*a, b),
            (Value(a), Number(b)) => match_number(*b, a),
            (Number(a), Number(b)) => compare_number(*a, *b) == Ordering::Equal,
            (Named(a), Named(b)) => compare_tuple_vec(a, b),
            _ => false,
        }
    }

    fn compare(&self, other: &EvalResult) -> Option<Ordering> {
        match (self, other) {
            (AnyValue, _) | (_, AnyValue) => Some(Ordering::Equal),
            (Value(a), Value(b)) => Some(a.cmp(b)),
            (Number(a), Value(b)) => cmp_number(*a, b),
            (Value(a), Number(b)) => match cmp_number(*b, a) {
                None => None,
                Some(ord) => {
                    match ord {
                        Ordering::Less => Some(Ordering::Greater),
                        Ordering::Equal => Some(Ordering::Equal),
                        Ordering::Greater => Some(Ordering::Less),
                    }
                }
            },
            (Number(a), Number(b)) => Some(compare_number(*a, *b)),
            (Named(a), Named(b)) => if compare_tuple_vec(a, b) { Some(Ordering::Equal) } else { None },
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Failure(_))
    }
}

fn concat_args(args: &Vec<EvalResult>) -> Vec<Cow<str>> {
    let mut result = vec![];

    for arg in args {
        match arg {
            Value(value) => result.push(Cow::Borrowed(value.as_str())),
            Number(value) => result.push(Cow::Owned(format_number(*value))),
            Named(pairs) => {
                for (i, (key, value)) in pairs.iter().enumerate() {
                    result.push(Cow::Borrowed(key.as_str()));
                    result.push(Cow::Borrowed(": "));
                    result.push(Cow::Borrowed(value.as_str()));
                    if i < pairs.len() - 1 {
                        result.push(Cow::Borrowed(", "));
                    }
                }
            }
            Undefined | AnyValue | Failure(_) => {}
        }
    }

    result
}

impl Expression {
    #[allow(clippy::too_many_lines)]
    pub fn eval(&self, ctx: &mut MapperContext, accessor: &ValueAccessor) -> EvalResult {
        match self {
            Expression::NullValue => Undefined,
            Expression::Identifier(name) => {
                if ctx.has_var(name) {
                    ctx.get_var(name).clone()
                } else {
                    Failure(format!("Variable with name {name} not found."))
                }
            }
            Expression::FieldAccess(field) => {
                if let Some(val) = accessor.get(field) {
                    Value(val.to_string())
                } else {
                    Undefined
                }
            }
            Expression::VarAccess(name, field) => {
                match ctx.variables.get(name) {
                    None => Failure(format!("Variable with name {name} not found.")),
                    Some(value) => match value {
                        Undefined => Undefined,
                        Number(_) | Value(_) => Failure(format!("Variable with name {name} has no fields.")),
                        Named(values) => {
                            for (key, val) in values {
                                if key == field {
                                    return Value(val.to_string());
                                }
                            }
                            Failure(format!("Variable with name {name} has no field {field}."))
                        }
                        AnyValue | Failure(_) => value.clone(),
                    },
                }
            }
            Expression::StringLiteral(s) => Value(s.clone()),
            Expression::NumberLiteral(num) => Number(*num),
            Expression::RegexExpr { field, pattern: _pattern, re_pattern } => {
                let source = match field {
                    RegexSource::Identifier(ident) => {
                        match ctx.get_var(ident) {
                            Value(text) => Some(Cow::Borrowed(text.as_str())),
                            _ => None,
                        }
                    }
                    RegexSource::Field(field) => accessor.get(field),
                };
                if let Some(val) = source {
                    let mut values = vec![];
                    for caps in re_pattern.captures_iter(&val) {
                        // Positional groups
                        for i in 1..caps.len() {
                            if let Some(m) = caps.get(i) {
                                values.push((i.to_string(), m.as_str().to_string()));
                            }
                        }

                        // named groups
                        for name in re_pattern.capture_names().flatten() {
                            if let Some(m) = caps.name(name) {
                                values.push((name.to_string(), m.as_str().to_string()));
                            }
                        }
                    }
                    if values.is_empty() {
                        return Undefined;
                    } else if values.len() == 1 {
                        return Value(values[0].1.to_string());
                    }
                    return Named(values);
                }
                Undefined
            }
            Expression::FunctionCall { name, args } => {
                let mut evaluated_args: Vec<EvalResult> = args.iter().map(|a| a.eval(ctx, accessor)).collect();
                for arg in &evaluated_args {
                    if arg.is_error() {
                        return Failure(format!("Function '{name:?}' failed: {}", if let Failure(msg) = arg { msg } else { "Unknown error" }));
                    }
                }
                evaluated_args.retain(|er| !matches!(er, Undefined | Failure(_) | AnyValue));
                if evaluated_args.is_empty() {
                    if matches!(name, BuiltInFunction::Print) {
                        trace!("[MapperScript] undefined");
                    }
                    Undefined
                } else {
                    match name {
                        BuiltInFunction::Concat => Value(concat_args(&evaluated_args).join("")),
                        BuiltInFunction::Uppercase => Value(concat_args(&evaluated_args).join(" ").to_uppercase()),
                        BuiltInFunction::Trim => Value(concat_args(&evaluated_args).iter().map(|s| s.trim()).collect::<Vec<_>>().join(" ").trim().to_string()),
                        BuiltInFunction::Lowercase => Value(concat_args(&evaluated_args).join(" ").to_lowercase()),
                        BuiltInFunction::Capitalize => Value(concat_args(&evaluated_args).iter().map(Capitalize::capitalize).collect::<Vec<_>>().join(" ")),
                        BuiltInFunction::Print => {
                            trace!("[MapperScript] {}", concat_args(&evaluated_args).join(""));
                            Undefined
                        }
                        BuiltInFunction::ToNumber => {
                            let evaluated_arg = &evaluated_args[0];
                            match evaluated_arg {
                                Value(value) => {
                                    to_number(value)
                                }
                                _ => evaluated_arg.clone()
                            }
                        }
                    }
                }
            }
            Expression::MatchBlock(cases) => {
                for match_case in cases {
                    let mut case_keys = vec![];
                    for case_key in &match_case.keys {
                        match case_key {
                            MatchCaseKey::Identifier(ident) => {
                                if !ctx.has_var(ident) {
                                    return Failure(format!("Match case invalid! Variable with name {ident} not found."));
                                }
                                case_keys.push(ctx.get_var(ident).clone());
                            }
                            MatchCaseKey::AnyMatch => case_keys.push(AnyValue),
                        }
                    }

                    let mut match_count = 0;
                    let case_keys_len = case_keys.len();
                    for case_key in case_keys {
                        match case_key {
                            Value(_)
                            | Number(_)
                            | Named(_)
                            | AnyValue => match_count += 1,
                            Undefined | Failure(_) => {}
                        }
                    }
                    if match_count == case_keys_len {
                        return match_case.expression.eval(ctx, accessor);
                    }
                }
                Undefined
            }
            Expression::MapBlock { key, cases } => {
                let key_value = match key {
                    MapKey::Identifier(ident) => {
                        if !ctx.has_var(ident) {
                            return Failure(format!("Map expression invalid! Variable with name {ident} not found."));
                        }
                        ctx.get_var(ident)
                    }
                };

                for map_case in cases {
                    let mut matches = false;
                    for key in &map_case.keys {
                        if match key {
                            MapCaseKey::Text(value) => key_value.matches(&Value(value.to_string())),
                            MapCaseKey::AnyMatch => true,
                            MapCaseKey::RangeFrom(num) => {
                                match key_value.compare(&Number(*num)) {
                                    None => false,
                                    Some(ord) => match ord {
                                        Ordering::Less => false,
                                        Ordering::Equal | Ordering::Greater => true,
                                    }
                                }
                            }
                            MapCaseKey::RangeTo(num) => {
                                match key_value.compare(&Number(*num)) {
                                    None => false,
                                    Some(ord) => match ord {
                                        Ordering::Equal | Ordering::Less => true,
                                        Ordering::Greater => false,
                                    }
                                }
                            }
                            MapCaseKey::RangeFull(from, to) => {
                                match key_value.compare(&Number(*from)) {
                                    None => false,
                                    Some(ord) => match ord {
                                        Ordering::Less => false,
                                        Ordering::Equal | Ordering::Greater => {
                                            match key_value.compare(&Number(*to)) {
                                                None => false,
                                                Some(ord) => match ord {
                                                    Ordering::Equal | Ordering::Less => true,
                                                    Ordering::Greater => false,
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            MapCaseKey::RangeEq(num) => {
                                match key_value.compare(&Number(*num)) {
                                    None => false,
                                    Some(ord) => match ord {
                                        Ordering::Equal => true,
                                        Ordering::Less | Ordering::Greater => false,
                                    }
                                }
                            }
                        } {
                            matches = true;
                            break;
                        }
                    }

                    if matches {
                        return map_case.expression.eval(ctx, accessor);
                    }
                }
                Undefined
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PlaylistItem, PlaylistItemHeader};

    #[test]
    fn test_mapper_dsl_eval() {
        let dsl = r#"
            coast = Caption ~ "(?i)\b(EAST|WEST)\b"
            quality = Caption ~ "(?i)\b([FUSL]?HD|SD|4K|1080p|720p|3840p)\b"
            quality = uppercase(quality)
            quality = map quality {
                       "SHD" => "SD",
                       "LHD" => "HD",
                       "720p" => "HD",
                       "1080p" => "FHD",
                       "4K" => "UHD",
                       "3840p" => "UHD",
                        _ => quality,
            }
            coast_quality = match {
                (coast, quality) => concat(capitalize(coast), " ", uppercase(quality)),
                coast => concat(capitalize(coast), " HD"),
                quality => concat("East ", uppercase(quality)),
            }
            Caption = concat("US: TNT", " ", coast_quality)
            Group = "United States - Entertainment"
    "#;

        let mapper = MapperScript::parse(dsl).expect("Parsing failed");
        println!("Program: {mapper:?}");
        let mut channels: Vec<PlaylistItem> = vec![
            ("D", "HD"), ("A", "FHD"), ("Z", ""), ("K", "HD"), ("B", "HD"), ("A", "HD"),
            ("K", "SHD"), ("C", "LHD"), ("L", "FHD"), ("R", "UHD"), ("T", "SD"), ("A", "FHD"),
        ].into_iter().map(|(name, quality)| PlaylistItem { header: PlaylistItemHeader { title: format!("Chanel {name} [{quality}]"), ..Default::default() } }).collect::<Vec<PlaylistItem>>();

        for pli in &mut channels {
            let mut accessor = ValueAccessor {
                pli,
            };
            mapper.eval(&mut accessor).expect("TODO: panic message");
            println!("Result: {pli:?}");
        }


        // ctx.fields.insert("Caption".to_string(), "US: TNT East LHD bubble".to_string());
        //
        // for stmt in &program.statements {
        //     //let res = stmt.eval(&mut ctx);
        //     println!("Statement Result: {:?}", res);
        // }
        //
        // println!("Result variable: {:?}", ctx.variables.get("result"));
        // assert_eq!(ctx.variables.get("result").unwrap(), "US: TNT East HD");
    }
}
