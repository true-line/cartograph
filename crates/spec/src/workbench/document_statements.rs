//! Business statements for D1 rule rows (#1999, AC-0905): a stored guarded
//! exit restated as a sentence in domain terms, by rule, with the source
//! condition and outcome kept as evidence rather than as the statement.
//!
//! Only simple shapes are rendered, each by a fixed rule, so the statement
//! keeps the fact's tier (SPEC-16 rule 3):
//!
//! - a presence test (`api_key`, `not api_key`): "an API key is given";
//! - a flag or predicate on a subject (`user.disabled`, `g.user.is_active()`,
//!   `not user.can_create_new_alias()`): "the user is disabled", "the user
//!   cannot create new alias";
//! - a comparison with a literal, a configured constant or another simple
//!   value (`mode == "word"`, `count > config.MAX_FREE`): "the mode is
//!   “word”", "the count is more than the configured max free";
//! - a test for no value (`x is None`), membership of a literal set
//!   (`mode in ("word", "uuid")`) and an ownership test
//!   (`alias.user_id != user.id`: "the alias does not belong to the user");
//! - `and` / `or` of these, and `not` over one of them.
//!
//! The outcome is rendered from the exit: an error-status response ("the
//! request is refused with status 403 (“Forbidden”)"), another status, a
//! redirect, a raised error, or the answer of a predicate method (`return
//! False` in `User.can_create_num_aliases`: "a user cannot create num
//! aliases when …"). Anything else is not rendered: the caller states a Gap
//! and the source stays in the evidence. Identifiers become words (snake and
//! camel case split, known acronyms upper-cased); no expression text is
//! copied into a statement.

use core_graph::rules::{BranchPolarity, GuardedExitEvidence, LocalExit};

/// Words of an identifier: `can_create_new_alias` → `can create new alias`,
/// `apiKey` → `API key`, `MAX_NB_EMAIL_FREE_PLAN` → `max nb email free plan`.
pub(super) fn words(identifier: &str) -> String {
    const ACRONYMS: [&str; 14] = [
        "api", "id", "url", "mfa", "otp", "pgp", "jwt", "sso", "oidc", "fido", "dns", "smtp",
        "http", "ip",
    ];
    let mut parts: Vec<String> = Vec::new();
    for chunk in identifier.split(['_', '-']).filter(|c| !c.is_empty()) {
        let all_upper = chunk.chars().all(|c| !c.is_ascii_lowercase());
        if all_upper {
            parts.push(chunk.to_ascii_lowercase());
            continue;
        }
        let mut current = String::new();
        for c in chunk.chars() {
            if c.is_ascii_uppercase() && !current.is_empty() {
                parts.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
            current.push(c);
        }
        if !current.is_empty() {
            parts.push(current.to_ascii_lowercase());
        }
    }
    parts
        .into_iter()
        .map(|p| {
            if ACRONYMS.contains(&p.as_str()) {
                p.to_ascii_uppercase()
            } else {
                p
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq)]
enum Lit {
    Str(String),
    Num(String),
    Bool(bool),
    None,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    /// `a.b.c`, with `.get("k")` read as a member `k`.
    Ref(Vec<String>),
    /// `a.b.m()`: a call without arguments.
    Call(Vec<String>),
    /// `a.b.m(x, k=y)`: a call with arguments (#2028).
    Apply(Vec<String>, Vec<Arg>),
    /// `arrow.now().shift(days=-N)`: a time N units from now, or ago when
    /// negative (#2028).
    Shift(String, Box<Expr>, bool),
    Lit(Lit),
    Set(Vec<Lit>),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Cmp(Box<Expr>, String, Box<Expr>),
}

/// One argument of a call: positional, or `name=value`.
#[derive(Debug, Clone, PartialEq)]
enum Arg {
    Pos(Expr),
    Kw(String, Expr),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Op(String),
}

fn tokenize(text: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(chars[start..i].iter().collect()));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            out.push(Tok::Num(chars[start..i].iter().collect()));
        } else if c == '"' || c == '\'' {
            let quote = c;
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' {
                    return None;
                }
                i += 1;
            }
            if i >= chars.len() {
                return None;
            }
            out.push(Tok::Str(chars[start..i].iter().collect()));
            i += 1;
        } else {
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            let three: String = chars[i..(i + 3).min(chars.len())].iter().collect();
            if three == "===" || three == "!==" {
                out.push(Tok::Op(three[..2].to_owned()));
                i += 3;
            } else if ["==", "!=", ">=", "<=", "&&", "||"].contains(&two.as_str()) {
                out.push(Tok::Op(two));
                i += 2;
            } else if "()<>.,!=[]+-".contains(c) {
                out.push(Tok::Op(c.to_string()));
                i += 1;
            } else {
                return None;
            }
        }
    }
    Some(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn ident(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(w)) if w == word)
    }
    fn op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if o == op)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn or(&mut self) -> Option<Expr> {
        let mut terms = vec![self.and()?];
        while self.ident("or") || self.op("||") {
            self.bump();
            terms.push(self.and()?);
        }
        Some(if terms.len() == 1 {
            terms.remove(0)
        } else {
            Expr::Or(terms)
        })
    }
    fn and(&mut self) -> Option<Expr> {
        let mut terms = vec![self.not()?];
        while self.ident("and") || self.op("&&") {
            self.bump();
            terms.push(self.not()?);
        }
        Some(if terms.len() == 1 {
            terms.remove(0)
        } else {
            Expr::And(terms)
        })
    }
    fn not(&mut self) -> Option<Expr> {
        if self.ident("not") || self.op("!") {
            self.bump();
            return Some(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Option<Expr> {
        let left = self.operand()?;
        let op = match self.peek() {
            Some(Tok::Op(o)) if ["==", "!=", ">", ">=", "<", "<="].contains(&o.as_str()) => {
                let o = o.clone();
                self.bump();
                o
            }
            Some(Tok::Ident(w)) if w == "is" => {
                self.bump();
                if self.ident("not") {
                    self.bump();
                    "is not".to_owned()
                } else {
                    "is".to_owned()
                }
            }
            Some(Tok::Ident(w)) if w == "in" => {
                self.bump();
                "in".to_owned()
            }
            Some(Tok::Ident(w))
                if w == "not"
                    && matches!(self.toks.get(self.pos + 1), Some(Tok::Ident(n)) if n == "in") =>
            {
                self.pos += 2;
                "not in".to_owned()
            }
            _ => return Some(left),
        };
        let right = self.operand()?;
        Some(Expr::Cmp(Box::new(left), op, Box::new(right)))
    }
    fn literal(&mut self) -> Option<Lit> {
        match self.bump()? {
            Tok::Str(s) => Some(Lit::Str(s)),
            Tok::Num(n) => Some(Lit::Num(n)),
            Tok::Ident(w) if w == "True" || w == "true" => Some(Lit::Bool(true)),
            Tok::Ident(w) if w == "False" || w == "false" => Some(Lit::Bool(false)),
            Tok::Ident(w) if w == "None" || w == "null" || w == "undefined" => Some(Lit::None),
            _ => None,
        }
    }
    fn operand(&mut self) -> Option<Expr> {
        match self.peek()? {
            Tok::Str(_) | Tok::Num(_) => return Some(Expr::Lit(self.literal()?)),
            Tok::Ident(w)
                if [
                    "True",
                    "False",
                    "None",
                    "true",
                    "false",
                    "null",
                    "undefined",
                ]
                .contains(&w.as_str()) =>
            {
                return Some(Expr::Lit(self.literal()?));
            }
            Tok::Op(o) if o == "(" => {
                self.bump();
                // A literal set `("a", "b")`, or a parenthesised expression.
                let save = self.pos;
                let mut set = Vec::new();
                loop {
                    match self.literal() {
                        Some(l) => set.push(l),
                        None => {
                            set.clear();
                            break;
                        }
                    }
                    if self.op(",") {
                        self.bump();
                        if self.op(")") {
                            break;
                        }
                        continue;
                    }
                    break;
                }
                if set.len() > 1 && self.op(")") {
                    self.bump();
                    return Some(Expr::Set(set));
                }
                self.pos = save;
                let inner = self.or()?;
                if !self.op(")") {
                    return None;
                }
                self.bump();
                return Some(inner);
            }
            Tok::Ident(_) => {}
            _ => return None,
        }
        let Some(Tok::Ident(first)) = self.bump() else {
            return None;
        };
        if ["and", "or", "not", "is", "in"].contains(&first.as_str()) {
            return None;
        }
        let mut path = vec![first];
        loop {
            if self.op(".") {
                self.bump();
                let Some(Tok::Ident(seg)) = self.bump() else {
                    return None;
                };
                // `.get("k")` or `.get(KEY)` reads member `k` / `KEY`.
                if seg == "get" && self.op("(") {
                    self.bump();
                    let key = match self.bump()? {
                        Tok::Str(key) => key,
                        Tok::Ident(first) => {
                            let mut last = first;
                            while self.op(".") {
                                self.bump();
                                let Some(Tok::Ident(next)) = self.bump() else {
                                    return None;
                                };
                                last = next;
                            }
                            last
                        }
                        _ => return None,
                    };
                    // A default argument does not change what is read.
                    if self.op(",") {
                        self.bump();
                        self.operand()?;
                    }
                    if !self.op(")") {
                        return None;
                    }
                    self.bump();
                    path.push(key);
                    continue;
                }
                path.push(seg);
            } else if self.op("[") {
                // `data["k"]` reads member `k`.
                self.bump();
                let Some(Tok::Str(key)) = self.bump() else {
                    return None;
                };
                if !self.op("]") {
                    return None;
                }
                self.bump();
                path.push(key);
            } else if self.op("(") {
                self.bump();
                let args = self.arguments()?;
                if self.op(".") {
                    // `arrow.now().shift(days=-N)`, the one member of a
                    // call's result that is rendered.
                    return self.shift(&path, &args);
                }
                return Some(if args.is_empty() {
                    Expr::Call(path)
                } else {
                    Expr::Apply(path, args)
                });
            } else {
                break;
            }
        }
        Some(Expr::Ref(path))
    }

    /// The arguments after an opening `(`, through the closing `)`.
    fn arguments(&mut self) -> Option<Vec<Arg>> {
        let mut args = Vec::new();
        while !self.op(")") {
            let keyword = match (self.peek(), self.toks.get(self.pos + 1)) {
                (Some(Tok::Ident(name)), Some(Tok::Op(eq))) if eq == "=" => Some(name.clone()),
                _ => None,
            };
            if let Some(name) = keyword {
                self.pos += 2;
                args.push(Arg::Kw(name, self.signed()?));
            } else {
                args.push(Arg::Pos(self.or()?));
            }
            if self.op(",") {
                self.bump();
            } else if !self.op(")") {
                return None;
            }
        }
        self.bump();
        Some(args)
    }

    /// An operand with an optional sign: `-15`, `+DAYS`.
    fn signed(&mut self) -> Option<Expr> {
        if self.op("-") || self.op("+") {
            let minus = self.op("-");
            self.bump();
            let inner = self.operand()?;
            return Some(match (minus, inner) {
                (true, Expr::Lit(Lit::Num(n))) => Expr::Lit(Lit::Num(format!("-{n}"))),
                (true, _) => return None,
                (false, inner) => inner,
            });
        }
        self.or()
    }

    /// `.shift(unit=N)` after `arrow.now()` / `arrow.utcnow()`.
    fn shift(&mut self, path: &[String], args: &[Arg]) -> Option<Expr> {
        let names: Vec<&str> = path.iter().map(String::as_str).collect();
        if !args.is_empty() || !matches!(names[..], ["arrow", "now" | "utcnow"]) {
            return None;
        }
        self.bump();
        if !self.ident("shift") {
            return None;
        }
        self.bump();
        if !self.op("(") {
            return None;
        }
        self.bump();
        let args = self.arguments()?;
        if self.op(".") {
            return None;
        }
        let [Arg::Kw(unit, amount)] = &args[..] else {
            return None;
        };
        if !["days", "hours", "minutes", "weeks", "seconds"].contains(&unit.as_str()) {
            return None;
        }
        Some(match amount {
            Expr::Lit(Lit::Num(n)) if n.starts_with('-') => Expr::Shift(
                unit.clone(),
                Box::new(Expr::Lit(Lit::Num(n[1..].to_owned()))),
                true,
            ),
            other => Expr::Shift(unit.clone(), Box::new(other.clone()), false),
        })
    }
}

fn parse(text: &str) -> Option<Expr> {
    let mut parser = Parser {
        toks: tokenize(text)?,
        pos: 0,
    };
    let expr = parser.or()?;
    (parser.pos == parser.toks.len()).then_some(expr)
}

/// The renderer's view of whom `self` and bare names refer to.
pub(super) struct Context<'a> {
    /// The class of the owning method, for `self`.
    pub class: Option<&'a str>,
}

fn subject(path: &[String], cx: &Context<'_>) -> Option<String> {
    let names: Vec<&str> = path.iter().map(String::as_str).collect();
    let noun = match names[..] {
        ["self"] | ["cls"] => format!("the {}", words(cx.class?)),
        ["current_user"] | ["g", "user"] => "the user".to_owned(),
        ["request"] => "the request".to_owned(),
        // A flag named as a question (`is_sl_domain`) is a flag, not a thing.
        [one] if predicate_verb(one).is_some() => format!("the “{}” flag", words(one)),
        [one] => format!("the {}", words(one)),
        ["self" | "cls", .., last] => format!("the {}'s {}", words(cx.class?), words(last)),
        ["request", .., last] => format!("the {}", words(last)),
        // An enum member `UserAliasDeleteAction.DeleteImmediately` (#1998).
        [owner, last]
            if owner.starts_with(|c: char| c.is_ascii_uppercase())
                && owner.chars().any(|c| c.is_ascii_lowercase())
                && last.starts_with(|c: char| c.is_ascii_uppercase())
                && last.chars().any(|c| c.is_ascii_lowercase()) =>
        {
            format!("“{}”", words(last))
        }
        [.., owner, last] => format!("the {}'s {}", words(owner), words(last)),
        [] => return None,
    };
    Some(noun)
}

/// A value as a noun phrase.
fn noun(expr: &Expr, cx: &Context<'_>) -> Option<String> {
    match expr {
        Expr::Ref(path) => {
            let last = path.last()?;
            // A configured constant: `config.MAX_FREE`, `MAX_FREE`.
            if last.len() > 1
                && last
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
            {
                return Some(format!("the configured {}", words(last)));
            }
            subject(path, cx)
        }
        Expr::Call(path) => {
            let last = path.last()?;
            if predicate_verb(last).is_some() {
                return None;
            }
            let names: Vec<&str> = path.iter().map(String::as_str).collect();
            if matches!(
                names[..],
                ["arrow", "now" | "utcnow"]
                    | ["datetime", "now" | "utcnow" | "today"]
                    | ["datetime", "datetime", "now" | "utcnow"]
                    | ["time", "time"]
                    | ["timezone", "now"]
            ) {
                return Some("the current time".to_owned());
            }
            subject(path, cx)
        }
        Expr::Lit(lit) => Some(literal(lit)),
        Expr::Apply(path, args) => {
            let names: Vec<&str> = path.iter().map(String::as_str).collect();
            match (&names[..], &args[..]) {
                (["len"], [Arg::Pos(x)]) => Some(format!("the length of {}", noun(x, cx)?)),
                ([model, "get_by" | "get" | "first_by"], _) if is_class(model) => {
                    Some(format!("the {} {}", words(model), with_these(args)?))
                }
                ([function], _) if predicate_verb(function).is_none() => Some(format!(
                    "the “{}” result",
                    words(function.trim_start_matches('_'))
                )),
                _ => None,
            }
        }
        Expr::Shift(unit, amount, ago) => {
            let amount = noun(amount, cx)?;
            // `ALIAS_TRASH_DAYS` already names its unit.
            let amount = if amount.ends_with(unit.as_str()) {
                amount
            } else {
                format!("{amount} {unit}")
            };
            Some(if *ago {
                format!("{amount} ago")
            } else {
                format!("{amount} from now")
            })
        }
        _ => None,
    }
}

/// A CamelCase class name: `Alias`, `DeletedAlias`.
fn is_class(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().any(|c| c.is_ascii_lowercase())
}

/// `email=a, user_id=b` as "with this email and user ID"; `None` for a
/// positional argument.
fn with_these(args: &[Arg]) -> Option<String> {
    let keys: Option<Vec<String>> = args
        .iter()
        .map(|a| match a {
            Arg::Kw(k, _) => Some(words(k)),
            Arg::Pos(_) => None,
        })
        .collect();
    let keys = keys?;
    if keys.is_empty() {
        return None;
    }
    Some(format!("with this {}", keys.join(" and ")))
}

/// The type `isinstance(x, T)` tests, as a phrase.
fn type_phrase(name: &str) -> Option<&'static str> {
    Some(match name {
        "str" => "text",
        "int" => "a whole number",
        "float" => "a number",
        "bool" => "true or false",
        "dict" => "an object",
        "list" | "tuple" => "a list",
        _ => return None,
    })
}

/// A call with arguments used as a condition (#2028), or `None`.
fn applied(path: &[String], args: &[Arg], negated: bool, cx: &Context<'_>) -> Option<String> {
    let names: Vec<&str> = path.iter().map(String::as_str).collect();
    let (last, owner) = names.split_last()?;
    if predicate_verb(last).is_some() {
        return flag(path, negated, cx);
    }
    match (&names[..], args) {
        (["isinstance"], [Arg::Pos(x), Arg::Pos(Expr::Ref(t))]) if t.len() == 1 => {
            let what = noun(x, cx)?;
            let kind = type_phrase(&t[0])?;
            return Some(if negated {
                format!("{what} is not {kind}")
            } else {
                format!("{what} is {kind}")
            });
        }
        ([model, "get_by" | "get" | "first_by"], _) if is_class(model) => {
            let thing = words(model);
            let these = with_these(args)?;
            return Some(if negated {
                format!("no {thing} {these} exists")
            } else {
                let article = if thing.starts_with(['a', 'e', 'i', 'o', 'u']) {
                    "an"
                } else {
                    "a"
                };
                format!("{article} {thing} {these} exists")
            });
        }
        _ => {}
    }
    let rest = |prefix: &str| last.strip_prefix(prefix).map(|r| r.trim_start_matches('_'));
    if let ([_, ..], Some(test), [Arg::Pos(Expr::Lit(Lit::Str(text)))]) = (
        owner,
        match *last {
            "startswith" => Some("start"),
            "endswith" => Some("end"),
            _ => None,
        },
        args,
    ) {
        let what = noun(
            &Expr::Ref(owner.iter().map(|s| (*s).to_owned()).collect()),
            cx,
        )?;
        return Some(if negated {
            format!("{what} does not {test} with “{text}”")
        } else {
            format!("{what} {test}s with “{text}”")
        });
    }
    if let Some(how) = rest("validate") {
        let who = subject(&path[..path.len() - 1], cx)?;
        let how = if how.is_empty() {
            String::new()
        } else {
            format!(" {}", words(how))
        };
        return Some(if negated {
            format!("{who} is not valid{how}")
        } else {
            format!("{who} is valid{how}")
        });
    }
    if let Some(what) = rest("check").filter(|r| !r.is_empty()) {
        let what = words(what.trim_start_matches("if_"));
        let whose = if owner.is_empty() {
            "the".to_owned()
        } else {
            format!("{}'s", subject(&path[..path.len() - 1], cx)?)
        };
        return Some(if negated {
            format!("{whose} {what} check fails")
        } else {
            format!("{whose} {what} check passes")
        });
    }
    if owner.is_empty() && last.trim_start_matches('_').contains('_') {
        let name = words(last.trim_start_matches('_'));
        return Some(if negated {
            format!("the check “{name}” does not hold")
        } else {
            format!("the check “{name}” holds")
        });
    }
    None
}

/// Whether a value names where request input arrives: `data`,
/// `request.form`, `request.args`, `request.json`.
fn request_input(path: &[String]) -> bool {
    matches!(
        path.last().map(String::as_str),
        Some("data" | "form" | "args" | "json" | "values" | "files" | "payload" | "body")
    )
}

fn literal(lit: &Lit) -> String {
    match lit {
        Lit::Str(s) => format!("“{s}”"),
        Lit::Num(n) => n.clone(),
        Lit::Bool(true) => "true".into(),
        Lit::Bool(false) => "false".into(),
        Lit::None => "empty".into(),
    }
}

/// `is_x` / `has_x` / `can_x` / `should_x`: the verb and the rest.
fn predicate_verb(name: &str) -> Option<(&'static str, &str)> {
    for (prefix, verb) in [
        ("is_", "is"),
        ("has_", "has"),
        ("can_", "can"),
        ("should_", "should"),
        ("must_", "must"),
        ("needs_", "needs"),
    ] {
        if let Some(rest) = name.strip_prefix(prefix).filter(|r| !r.is_empty()) {
            return Some((verb, rest));
        }
    }
    None
}

fn negated_verb(verb: &str) -> &'static str {
    match verb {
        "is" => "is not",
        "has" => "has no",
        "can" => "cannot",
        "should" => "should not",
        "must" => "must not",
        _ => "does not need",
    }
}

/// A flag on a subject: `user.disabled`, `user.is_active()`.
fn flag(path: &[String], negated: bool, cx: &Context<'_>) -> Option<String> {
    let (last, owner) = path.split_last()?;
    if owner.is_empty() {
        // A bare name: a presence test, or a predicate function.
        if let Some((verb, rest)) = predicate_verb(last) {
            let verb = if negated { negated_verb(verb) } else { verb };
            return Some(format!("the check “{verb} {}” holds", words(rest)));
        }
        let phrase = words(last);
        let article = if phrase.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
            "an"
        } else {
            "a"
        };
        return Some(if negated {
            format!("no {phrase} is given")
        } else {
            format!("{article} {phrase} is given")
        });
    }
    let who = subject(owner, cx)?;
    if predicate_verb(last).is_none() && !state_word(last) {
        // A value tested for presence: `user.trial_end` reads "the user's
        // trial end is set".
        let what = subject(path, cx)?;
        return Some(if negated {
            format!("{what} is not set")
        } else {
            format!("{what} is set")
        });
    }
    let (verb, rest) = predicate_verb(last).unwrap_or(("is", last));
    let verb = if negated { negated_verb(verb) } else { verb };
    Some(format!("{who} {verb} {}", words(rest)))
}

/// Whether an attribute name reads as a state its owner is in
/// (`verified`, `admin_disabled`, `active`), not a value it holds.
fn state_word(name: &str) -> bool {
    let last = name.rsplit('_').next().unwrap_or(name).to_ascii_lowercase();
    last.ends_with("ed")
        || [
            "active", "valid", "invalid", "premium", "open", "closed", "pending", "lifetime",
            "free", "paid", "public", "private", "on", "off", "enabled", "online", "offline",
            "unique", "dirty", "new", "old", "trial", "admin", "partner", "spam", "deleted",
            "hidden", "visible", "required", "optional",
        ]
        .contains(&last.as_str())
}

fn render(expr: &Expr, negated: bool, cx: &Context<'_>) -> Option<String> {
    match expr {
        Expr::Not(inner) => render(inner, !negated, cx),
        Expr::Ref(path) => flag(path, negated, cx),
        Expr::Call(path) => applied(path, &[], negated, cx),
        Expr::And(terms) | Expr::Or(terms) => {
            // `not (a and b)` is `not a or not b`; `not (a or b)` is `not a
            // and not b`.
            let joiner = match (expr, negated) {
                (Expr::And(_), false) | (Expr::Or(_), true) => " and ",
                _ => " or ",
            };
            let parts: Option<Vec<String>> = terms.iter().map(|t| render(t, negated, cx)).collect();
            Some(parts?.join(joiner))
        }
        Expr::Cmp(left, op, right) => {
            // Ownership: `x.user_id != user.id`.
            if let (Expr::Ref(l), Expr::Ref(r)) = (left.as_ref(), right.as_ref())
                && l.len() >= 2
                && r.last().is_some_and(|s| s == "id")
                && let Some(owner_field) = l.last().and_then(|s| s.strip_suffix("_id"))
                && (r.len() == 1
                    || r[r.len() - 2] == owner_field
                    || r[..r.len() - 1] == ["g", "user"] && owner_field == "user"
                    || r[..r.len() - 1] == ["current_user"] && owner_field == "user")
                && (op == "==" || op == "!=")
            {
                let thing = subject(&l[..l.len() - 1], cx)?;
                let owner = format!("the {}", words(owner_field));
                let belongs = (op == "==") != negated;
                return Some(if belongs {
                    format!("{thing} belongs to {owner}")
                } else {
                    format!("{thing} does not belong to {owner}")
                });
            }
            // Membership of a literal key or text (#2028): `"name" in data`
            // is "a name is given"; `".." in alias` is "the alias contains
            // “..”".
            if let (Expr::Lit(Lit::Str(text)), "in" | "not in", Expr::Ref(r)) =
                (left.as_ref(), op.as_str(), right.as_ref())
            {
                let present = (op == "in") != negated;
                if request_input(r) {
                    let key = words(text);
                    return Some(if present {
                        format!("{key} is given")
                    } else {
                        format!("{key} is not given")
                    });
                }
                if r.len() == 1 && r[0] == "session" {
                    let key = words(text);
                    return Some(if present {
                        format!("the session holds {key}")
                    } else {
                        format!("the session does not hold {key}")
                    });
                }
                let whole = noun(right, cx)?;
                return Some(if present {
                    format!("{whole} contains “{text}”")
                } else {
                    format!("{whole} does not contain “{text}”")
                });
            }
            let left_np = noun(left, cx)?;
            let timed = matches!(right.as_ref(), Expr::Shift(..))
                || noun(right, cx).is_some_and(|r| r == "the current time");
            let positive = |op: &str| -> Option<String> {
                Some(match (op, right.as_ref()) {
                    (">", r) if timed => format!("{left_np} is after {}", noun(r, cx)?),
                    (">=", r) if timed => format!("{left_np} is at or after {}", noun(r, cx)?),
                    ("<", r) if timed => format!("{left_np} is before {}", noun(r, cx)?),
                    ("<=", r) if timed => format!("{left_np} is at or before {}", noun(r, cx)?),
                    ("in", r @ (Expr::Ref(_) | Expr::Apply(..) | Expr::Call(_))) => {
                        format!("{left_np} is one of {}", noun(r, cx)?)
                    }
                    ("not in", r @ (Expr::Ref(_) | Expr::Apply(..) | Expr::Call(_))) => {
                        format!("{left_np} is not one of {}", noun(r, cx)?)
                    }
                    ("is" | "==", Expr::Lit(Lit::None)) => format!("{left_np} is not set"),
                    ("is not" | "!=", Expr::Lit(Lit::None)) => format!("{left_np} is set"),
                    ("==" | "is", r) => format!("{left_np} is {}", noun(r, cx)?),
                    ("!=" | "is not", r) => format!("{left_np} is not {}", noun(r, cx)?),
                    (">", r) => format!("{left_np} is more than {}", noun(r, cx)?),
                    (">=", r) => format!("{left_np} is at least {}", noun(r, cx)?),
                    ("<", r) => format!("{left_np} is less than {}", noun(r, cx)?),
                    ("<=", r) => format!("{left_np} is at most {}", noun(r, cx)?),
                    ("in", Expr::Set(set)) => format!(
                        "{left_np} is one of {}",
                        set.iter().map(literal).collect::<Vec<_>>().join(", ")
                    ),
                    ("not in", Expr::Set(set)) => format!(
                        "{left_np} is not one of {}",
                        set.iter().map(literal).collect::<Vec<_>>().join(", ")
                    ),
                    _ => return None,
                })
            };
            let flipped = match op.as_str() {
                "==" => "!=",
                "!=" => "==",
                "is" => "is not",
                "is not" => "is",
                ">" => "<=",
                ">=" => "<",
                "<" => ">=",
                "<=" => ">",
                "in" => "not in",
                "not in" => "in",
                _ => return None,
            };
            positive(if negated { flipped } else { op })
        }
        Expr::Apply(path, args) => applied(path, args, negated, cx),
        Expr::Lit(_) | Expr::Set(_) | Expr::Shift(..) => None,
    }
}

/// One stored condition in domain terms, with its branch polarity.
pub(super) fn condition(
    display: &str,
    polarity: BranchPolarity,
    cx: &Context<'_>,
) -> Option<String> {
    let expr = parse(display)?;
    render(&expr, polarity == BranchPolarity::FalsyBranch, cx)
}

/// An HTTP status and a literal message from a response display:
/// `jsonify(error="Forbidden"), 403` gives `(403, Some("Forbidden"))`.
fn response(display: &str) -> Option<(u16, Option<String>)> {
    let display = display.trim();
    let display = display
        .strip_prefix('(')
        .and_then(|d| d.strip_suffix(')'))
        .unwrap_or(display)
        .trim()
        .trim_end_matches(',');
    let (body, status) = display.rsplit_once(',')?;
    let status: u16 = status.trim().trim_end_matches(')').trim().parse().ok()?;
    let body = body.trim().trim_start_matches('(').trim();
    let message = body
        .strip_prefix("jsonify(error=")
        .or_else(|| body.strip_prefix("jsonify(message="))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|lit| {
            let lit = lit.trim();
            let q = lit.chars().next()?;
            (matches!(q, '"' | '\'') && lit.ends_with(q) && lit.len() >= 2)
                .then(|| lit[1..lit.len() - 1].to_owned())
        })
        .filter(|m| !m.contains(['"', '\'', '{', '\\']));
    Some((status, message))
}

/// `x.field = value` as a clause (#1998): `g.user = api_key.user` is "the
/// request acts as the API key's user"; `alias.enabled = False` is "the
/// alias is no longer enabled"; `alias.delete_on = arrow.now().shift(days=+N)`
/// is "the alias's delete on is set to N days from now"; otherwise "the
/// alias's note is set to the note". `None` outside these rules.
fn assigned(target: &str, value: &str, cx: &Context<'_>) -> Option<String> {
    let Expr::Ref(path) = parse(target.trim())? else {
        return None;
    };
    let names: Vec<&str> = path.iter().map(String::as_str).collect();
    let value = value.trim();
    // The acting user of a request (Flask `g.user`, Django `request.user`).
    if matches!(names[..], ["g", "user"] | ["request", "user"]) {
        if value == "current_user" {
            return Some("the request acts as the signed-in user".to_owned());
        }
        return Some(format!("the request acts as {}", noun(&parse(value)?, cx)?));
    }
    let place = match names[..] {
        ["g", ref rest @ ..] if !rest.is_empty() => {
            format!("the request's {}", words(&rest.join("_")))
        }
        _ => subject(&path, cx)?,
    };
    // `arrow.now().shift(days=+N)`: N days from now.
    if let Some(rest) = value
        .strip_prefix("arrow.now().shift(")
        .or_else(|| value.strip_prefix("arrow.utcnow().shift("))
        .and_then(|r| r.strip_suffix(')'))
        && let Some((unit, amount)) = rest.split_once('=')
        && matches!(
            unit.trim(),
            "days" | "hours" | "minutes" | "weeks" | "seconds"
        )
    {
        let unit = unit.trim();
        let amount = noun(&parse(amount.trim().trim_start_matches('+'))?, cx)?;
        // `ALIAS_TRASH_DAYS` already names its unit.
        return Some(if amount.ends_with(unit) {
            format!("{place} is set to {amount} from now")
        } else {
            format!("{place} is set to {amount} {unit} from now")
        });
    }
    let parsed = parse(value)?;
    if let (Expr::Lit(Lit::Bool(on)), [owner @ .., last]) = (&parsed, &path[..])
        && !owner.is_empty()
    {
        let owner = subject(owner, cx)?;
        let state = words(last);
        return Some(if *on {
            format!("{owner} is now {state}")
        } else {
            format!("{owner} is no longer {state}")
        });
    }
    Some(format!("{place} is set to {}", noun(&parsed, cx)?))
}

/// A call statement as a clause (#1998): `perform_alias_deletion(alias)` is
/// "it runs “perform alias deletion”". `None` unless the callee is a dotted
/// name.
fn called(value: &str) -> Option<String> {
    let value = value.trim().strip_prefix("await ").unwrap_or(value.trim());
    let callee = &value[..value.find('(')?];
    let mut segments = callee.split('.');
    if !segments
        .clone()
        .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        return None;
    }
    let last = segments.next_back()?;
    // Flask `flash("Alias not found", "error")`: the message the user sees.
    if last == "flash"
        && let Some(message) = value[callee.len() + 1..]
            .trim_start()
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"'))
            .map(|(m, _)| m)
            .filter(|m| !m.is_empty() && !m.contains(['\\', '{', '\'']))
    {
        return Some(format!("the user is shown “{message}”"));
    }
    Some(format!("it runs “{}”", words(last.trim_start_matches('_'))))
}

/// The statement of one guarded exit in domain terms, or `None` when its
/// condition or outcome is outside the rendering rules. `owner` is the
/// owning callable's qualified name (`User.can_create_num_aliases`).
pub(super) fn statement(evidence: &GuardedExitEvidence, owner: &str) -> Option<String> {
    // A rendered statement that would still carry a code-shaped token is not
    // a statement in domain terms: the row states a Gap instead (#1999).
    rendered(evidence, owner)
        .filter(|s| crate::workbench::neutrality::projected_code_token(s).is_none())
}

fn rendered(evidence: &GuardedExitEvidence, owner: &str) -> Option<String> {
    let (class, method) = match owner.rsplit_once('.') {
        Some((class, method)) => (
            class
                .rsplit('.')
                .next()
                .filter(|c| c.starts_with(|f: char| f.is_ascii_uppercase())),
            method,
        ),
        None => (None, owner),
    };
    let cx = Context { class };
    let conditions: Option<Vec<String>> = evidence
        .conditions
        .iter()
        .map(|c| condition(c.expression.display.as_str(), c.polarity, &cx))
        .collect();
    let conditions = conditions?;
    let when = if conditions.is_empty() {
        "always".to_owned()
    } else {
        conditions.join(", and ")
    };
    // A state change or a call (#1998, AC-0910).
    let changed = match &evidence.effect {
        LocalExit::Assign { target, value } => Some(assigned(
            target.display.as_str(),
            value.display.as_str(),
            &cx,
        )?),
        LocalExit::Call { value } => Some(called(value.display.as_str())?),
        LocalExit::Return { .. } | LocalExit::Throw { .. } => None,
    };
    if let Some(outcome) = changed {
        return Some(if conditions.is_empty() {
            format!("{}{}.", outcome[..1].to_uppercase(), &outcome[1..])
        } else {
            format!("When {when}, {outcome}.")
        });
    }
    let value = match &evidence.effect {
        LocalExit::Assign { .. } | LocalExit::Call { .. } => return None,
        LocalExit::Throw { .. } => {
            return Some(if conditions.is_empty() {
                "It fails with an error.".to_owned()
            } else {
                format!("When {when}, it fails with an error.")
            });
        }
        LocalExit::Return { value: None } => return None,
        LocalExit::Return { value: Some(value) } if super::rules::constructs_error(value) => {
            return Some(if conditions.is_empty() {
                "It fails with an error.".to_owned()
            } else {
                format!("When {when}, it fails with an error.")
            });
        }
        LocalExit::Return { value: Some(value) } => value.display.as_str().trim().to_owned(),
    };
    let sentence = |outcome: String| {
        if conditions.is_empty() {
            format!("{}{}.", outcome[..1].to_uppercase(), &outcome[1..])
        } else {
            format!("When {when}, {outcome}.")
        }
    };
    if let Some((status, message)) = response(&value) {
        let tail = message.map(|m| format!(" (“{m}”)")).unwrap_or_default();
        let outcome = if (400..600).contains(&status) {
            format!("the request is refused with status {status}{tail}")
        } else {
            format!("it responds with status {status}{tail}")
        };
        return Some(sentence(outcome));
    }
    if value.starts_with("redirect(") {
        return Some(sentence("the caller is redirected".to_owned()));
    }
    let answer = match value.as_str() {
        "True" | "true" => Some(true),
        "False" | "false" => Some(false),
        _ => None,
    };
    let predicate = predicate_verb(method.trim_start_matches('_'));
    let (Some(answer), Some((verb, rest))) = (answer, predicate) else {
        // A value the method gives: `return config.MAX_OLD` in
        // `User.max_alias` reads "the user's max alias is the configured
        // max old"; a wrapper passing a value on returns it.
        let parsed = parse(&value)?;
        if class.is_none()
            && let Expr::Ref(path) = &parsed
            && let [one] = &path[..]
        {
            return Some(sentence(format!("it returns the {}", words(one))));
        }
        let given = noun(&parsed, &cx)?;
        // An accessor reads as what it gives: `get_active_subscription`
        // is "active subscription".
        let named = method.trim_start_matches('_');
        let named = named.strip_prefix("get_").unwrap_or(named);
        let what = match class {
            Some(class) => format!("the {}'s {}", words(class), words(named)),
            None => format!("the {}", words(named)),
        };
        return Some(sentence(format!("{what} is {given}")));
    };
    let verb = if answer { verb } else { negated_verb(verb) };
    let who = match class {
        Some(class) => format!("a {}", words(class)),
        None => "the caller".to_owned(),
    };
    Some(if conditions.is_empty() {
        format!("Otherwise {who} {verb} {}.", words(rest))
    } else {
        let s = format!("{who} {verb} {} when {when}.", words(rest));
        format!("{}{}", s[..1].to_uppercase(), &s[1..])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(display: &str, polarity: BranchPolarity, class: Option<&str>) -> Option<String> {
        condition(display, polarity, &Context { class })
    }

    // AC-0912: membership, calls with arguments, type tests, lengths,
    // lookups joined by `or`, times and header reads are restated by rule.
    #[test]
    fn common_rule_shapes_are_restated_in_domain_terms() {
        let t = BranchPolarity::TruthyBranch;
        let f = BranchPolarity::FalsyBranch;
        let cases = [
            ("\"..\" in full_alias", t, "the full alias contains “..”"),
            ("\"mailbox_id\" in data", t, "mailbox ID is given"),
            ("\"password\" not in data", t, "password is not given"),
            ("\"error\" in request.args", f, "error is not given"),
            (
                "SESSION_STATE_KEY not in session",
                t,
                "the configured session state key is not one of the session",
            ),
            (
                "\"mailbox_id\" in data and (not mailbox or mailbox.user_id != user.id or not mailbox.verified)",
                t,
                "mailbox ID is given and no mailbox is given or the mailbox does not belong to the user or the mailbox is not verified",
            ),
            (
                "not user or not user.check_password(password)",
                t,
                "no user is given or the user's password check fails",
            ),
            (
                "Alias.get_by(email=a) or DeletedAlias.get_by(email=a) or DomainDeletedAlias.get_by(email=a)",
                t,
                "an alias with this email exists or a deleted alias with this email exists or a domain deleted alias with this email exists",
            ),
            (
                "not isinstance(alias_prefix, str) or not alias_prefix",
                t,
                "the alias prefix is not text or no alias prefix is given",
            ),
            (
                "new_name and len(new_name) > 128",
                t,
                "a new name is given and the length of the new name is more than 128",
            ),
            (
                "activation.created_at < arrow.now().shift(minutes=-15)",
                t,
                "the activation's created at is before 15 minutes ago",
            ),
            (
                "request.headers.get(constants.HEADER_ALLOW_API_COOKIES)",
                t,
                "the header allow API cookies is set",
            ),
            (
                "not form.validate_on_submit()",
                t,
                "the form is not valid on submit",
            ),
            (
                "check_if_abuser_email(email)",
                t,
                "the abuser email check passes",
            ),
            (
                "subdomain.endswith(\"-\")",
                t,
                "the subdomain ends with “-”",
            ),
            (
                "detect_image_format(raw) == ImageFormat.Unknown",
                t,
                "the “detect image format” result is “unknown”",
            ),
            (
                "alias_domain in user_custom_domains",
                f,
                "the alias domain is not one of the user custom domains",
            ),
        ];
        for (display, polarity, expected) in cases {
            assert_eq!(
                said(display, polarity, None).as_deref(),
                Some(expected),
                "{display}"
            );
        }
        // Still outside the rules: a member of a call's result, arithmetic.
        assert_eq!(said("a.b(c).d", t, None), None);
        assert_eq!(said("(time() - int(t)) > config.GRACE", t, None), None);
    }

    // AC-0911: an assignment and a call statement are restated by rule.
    #[test]
    fn state_changes_and_calls_are_restated_in_domain_terms() {
        let cx = Context { class: None };
        let set = |t: &str, v: &str| assigned(t, v, &cx);
        assert_eq!(
            set("g.user", "api_key.user").unwrap(),
            "the request acts as the API key's user"
        );
        assert_eq!(
            set("g.user", "current_user").unwrap(),
            "the request acts as the signed-in user"
        );
        assert_eq!(
            set("g.api_key", "api_key").unwrap(),
            "the request's API key is set to the API key"
        );
        assert_eq!(
            set("alias.enabled", "False").unwrap(),
            "the alias is no longer enabled"
        );
        assert_eq!(
            set("user.verified", "True").unwrap(),
            "the user is now verified"
        );
        assert_eq!(
            set(
                "alias.delete_on",
                "arrow.now().shift(days=+ALIAS_TRASH_DAYS)"
            )
            .unwrap(),
            "the alias's delete on is set to the configured alias trash days from now"
        );
        assert_eq!(
            set("user.expires", "arrow.now().shift(hours=1)").unwrap(),
            "the user's expires is set to 1 hours from now"
        );
        assert_eq!(
            set("alias.note", "note").unwrap(),
            "the alias's note is set to the note"
        );
        assert_eq!(
            set("alias.note", "compute(alias)").unwrap(),
            "the alias's note is set to the “compute” result"
        );
        assert_eq!(set("alias.note", "compute(alias) + 1"), None);
        assert_eq!(set("items[0]", "1"), None);
        assert_eq!(
            called("perform_alias_deletion(alias, user)").unwrap(),
            "it runs “perform alias deletion”"
        );
        assert_eq!(
            called("await alias_utils.delete_alias(alias, user)").unwrap(),
            "it runs “delete alias”"
        );
        assert_eq!(called("handlers[0](x)"), None);
        assert_eq!(
            called("flash(\"Alias not found\", \"error\")").unwrap(),
            "the user is shown “Alias not found”"
        );
        assert_eq!(
            called("flash(f\"{n} deleted\", \"success\")").unwrap(),
            "it runs “flash”"
        );
        assert_eq!(
            condition(
                "user.alias_delete_action == UserAliasDeleteAction.DeleteImmediately",
                BranchPolarity::TruthyBranch,
                &cx
            )
            .unwrap(),
            "the user's alias delete action is “delete immediately”"
        );
    }

    // AC-0905: simple conditions are restated by rule; anything else is not.
    #[test]
    fn simple_conditions_are_restated_in_domain_terms() {
        let t = BranchPolarity::TruthyBranch;
        let f = BranchPolarity::FalsyBranch;
        assert_eq!(said("not api_key", t, None).unwrap(), "no API key is given");
        assert_eq!(
            said("g.user.disabled", t, None).unwrap(),
            "the user is disabled"
        );
        assert_eq!(
            said("not g.user.is_active()", t, None).unwrap(),
            "the user is not active"
        );
        assert_eq!(
            said("not user.can_create_new_alias()", t, None).unwrap(),
            "the user cannot create new alias"
        );
        assert_eq!(
            said("not alias or alias.user_id != user.id", t, None).unwrap(),
            "no alias is given or the alias does not belong to the user"
        );
        assert_eq!(
            said("mode not in (\"word\", \"uuid\")", t, None).unwrap(),
            "the mode is not one of “word”, “uuid”"
        );
        assert_eq!(
            said("self.delete_on is not None", t, Some("User")).unwrap(),
            "the user's delete on is set"
        );
        assert_eq!(
            said("count > config.MAX_NB_EMAIL_FREE_PLAN", t, None).unwrap(),
            "the count is more than the configured max nb email free plan"
        );
        assert_eq!(
            said("current_user.is_authenticated", f, None).unwrap(),
            "the user is not authenticated"
        );
        assert_eq!(
            said("not api_key and not current_user.is_authenticated", t, None).unwrap(),
            "no API key is given and the user is not authenticated"
        );
        assert_eq!(
            said("data.get(\"mode\") == \"word\"", t, None).unwrap(),
            "the data's mode is “word”"
        );
        assert_eq!(
            said("is_sl_domain is None", t, None).unwrap(),
            "the “is sl domain” flag is not set"
        );
        // AC-0908: a value tested for presence is "set"; the current time
        // is named as such.
        assert_eq!(
            said("self.trial_end", t, Some("User")).unwrap(),
            "the user's trial end is set"
        );
        assert_eq!(
            said("self.trial_end > arrow.now()", t, Some("User")).unwrap(),
            "the user's trial end is after the current time"
        );
        // Outside the rules: a one-word call, arithmetic, a member of a
        // call's result (lookups with arguments are AC-0912).
        assert_eq!(said("check(alias, user)", t, None), None);
        assert_eq!(said("(time() - int(t)) > config.X", t, None), None);
        assert_eq!(said("foo().bar", t, None), None);
        assert_eq!(words("MAX_NB_EMAIL_FREE_PLAN"), "max nb email free plan");
        assert_eq!(words("apiKeyId"), "API key ID");
    }
}
