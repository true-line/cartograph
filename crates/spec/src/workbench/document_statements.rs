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

use core_graph::rules::{
    BranchPolarity, GuardedExitEvidence, LocalExit, go_http_status, rust_http_status,
};

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
        // A run of capitals is one word (`ByID`, `HTTPServer`): a capital
        // starts a word after a lower-case letter, or before one.
        let chars: Vec<char> = chunk.chars().collect();
        let mut current = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let after_lower = i > 0 && !chars[i - 1].is_ascii_uppercase();
            let before_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if c.is_ascii_uppercase() && !current.is_empty() && (after_lower || before_lower) {
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
    /// `a + b`: a sum of values (AC-1030).
    Sum(Vec<Expr>),
    /// `a - b - c`: the first value less the others (AC-1052).
    Difference(Vec<Expr>),
    /// `flags & FLAG`: a bit mask (AC-1030).
    Mask(Box<Expr>, Box<Expr>),
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
        // Optional chaining reads as a member (AC-1032): `a?.b` is `a.b`.
        if c == '?' && chars.get(i + 1) == Some(&'.') {
            out.push(Tok::Op(".".to_owned()));
            i += 2;
        } else if c.is_whitespace() {
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
                // A string that is only a line break (`"\n"`) is kept as
                // `\n`; any other escape is outside the rules.
                if chars[i] == '\\' {
                    if i == start
                        && chars.get(i + 1) == Some(&'n')
                        && chars.get(i + 2) == Some(&quote)
                    {
                        i += 2;
                        break;
                    }
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
            } else if "()<>.,!=[]+-&".contains(c) {
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
        let left = self.sum()?;
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
        let right = self.sum()?;
        Some(Expr::Cmp(Box::new(left), op, Box::new(right)))
    }
    /// `a + b + c` and `a & b` over operands (AC-1030); one operand is
    /// itself.
    fn sum(&mut self) -> Option<Expr> {
        let first = self.operand()?;
        if self.op("&") {
            self.bump();
            let mask = self.operand()?;
            return Some(Expr::Mask(Box::new(first), Box::new(mask)));
        }
        let mut terms = vec![first];
        if self.op("-") {
            while self.op("-") {
                self.bump();
                terms.push(self.operand()?);
            }
            return (!self.op("+")).then_some(Expr::Difference(terms));
        }
        while self.op("+") {
            self.bump();
            terms.push(self.operand()?);
        }
        Some(if terms.len() == 1 {
            terms.remove(0)
        } else {
            Expr::Sum(terms)
        })
    }
    fn literal(&mut self) -> Option<Lit> {
        match self.bump()? {
            Tok::Str(s) => Some(Lit::Str(s)),
            Tok::Num(n) => Some(Lit::Num(n)),
            Tok::Ident(w) if w == "True" || w == "true" => Some(Lit::Bool(true)),
            Tok::Ident(w) if w == "False" || w == "false" => Some(Lit::Bool(false)),
            Tok::Ident(w) if w == "None" || w == "null" || w == "undefined" || w == "nil" => {
                Some(Lit::None)
            }
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
                    "nil",
                ]
                .contains(&w.as_str()) =>
            {
                return Some(Expr::Lit(self.literal()?));
            }
            // A literal list `["a", "b"]` (#2027).
            Tok::Op(o) if o == "[" => {
                let close = "]";
                self.bump();
                let mut set = Vec::new();
                loop {
                    set.push(self.literal()?);
                    if self.op(",") {
                        self.bump();
                        if self.op(close) {
                            break;
                        }
                        continue;
                    }
                    break;
                }
                if !self.op(close) {
                    return None;
                }
                self.bump();
                return Some(Expr::Set(set));
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
                if (seg == "get" || seg == "Get") && self.op("(") {
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
                // A text conversion of the result (`.decode()`) does not
                // change what is read (AC-1036).
                if self.op(".")
                    && matches!(
                        self.toks.get(self.pos + 1),
                        Some(Tok::Ident(m)) if matches!(m.as_str(), "decode" | "encode")
                    )
                    && matches!(self.toks.get(self.pos + 2), Some(Tok::Op(o)) if o == "(")
                    && matches!(self.toks.get(self.pos + 3), Some(Tok::Op(o)) if o == ")")
                    && !args.is_empty()
                {
                    self.pos += 4;
                    return Some(Expr::Apply(path, args));
                }
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
    /// The literal a module string constant holds, by name (#2027).
    pub constant: Constants<'a>,
    /// The call whose error this condition tests (`Store.Save`), when the
    /// adapter recorded it (#2009).
    pub failed: Option<&'a str>,
}

/// Looks up the literal of a module string constant by name: `Some` only
/// when exactly one module binds it.
pub(super) type Constants<'a> = Option<&'a dyn Fn(&str) -> Option<String>>;

fn subject(path: &[String], cx: &Context<'_>) -> Option<String> {
    let names: Vec<&str> = path.iter().map(String::as_str).collect();
    let noun = match names[..] {
        ["self"] | ["cls"] => format!("the {}", words(cx.class?)),
        [receiver] if go_receiver(receiver, cx) => format!("the {}", words(cx.class?)),
        [receiver, .., last] if go_receiver(receiver, cx) => {
            format!("the {}'s {}", words(cx.class?), words(last))
        }
        ["current_user"] | ["g", "user"] => "the user".to_owned(),
        ["request"] => "the request".to_owned(),
        // A flag named as a question (`is_sl_domain`) is a flag, not a thing.
        [one] if predicate_verb(one).is_some() => format!("the “{}” flag", words(one)),
        [one] => format!("the {}", words(one)),
        ["self" | "cls", .., last] => format!("the {}'s {}", words(cx.class?), words(last)),
        // A header read by a constant naming it: the header's own name.
        ["request", "headers", key] => match cx.constant.and_then(|c| c(key)) {
            Some(name) => format!("the “{name}” header"),
            None => format!("the {}", words(key)),
        },
        ["request", .., last] => format!("the {}", words(last)),
        // Go: `http.MethodPost` is “POST”; `r.Method` the request's method
        // (#2009).
        ["http", method] if method.len() > 6 && method.starts_with("Method") => {
            format!("“{}”", method[6..].to_uppercase())
        }
        ["r" | "req", .., last] => format!("the request's {}", words(last)),
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
            // JavaScript's integer bounds (AC-1052).
            if let [number, bound] = &path[..]
                && number == "Number"
            {
                match bound.as_str() {
                    "MAX_SAFE_INTEGER" => {
                        return Some("the largest safe integer (9007199254740991)".to_owned());
                    }
                    "MIN_SAFE_INTEGER" => {
                        return Some("the smallest safe integer (-9007199254740991)".to_owned());
                    }
                    _ => {}
                }
            }
            // An enum member: `GlobalFlag.FALSE` is “FALSE” (AC-1052).
            if let [.., owner, member] = &path[..]
                && owner.starts_with(|c: char| c.is_ascii_uppercase())
                && owner.chars().any(|c| c.is_ascii_lowercase())
                && !["Config", "Settings", "Constants", "Defaults", "Env"]
                    .iter()
                    .any(|n| owner.contains(n))
                && member.len() > 1
                && member.starts_with(|c: char| c.is_ascii_uppercase())
                && member
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
            {
                return Some(if member.contains('_') {
                    format!("“{}”", words(member))
                } else {
                    format!("“{member}”")
                });
            }
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
            subject(&accessor(path), cx)
        }
        Expr::Lit(lit) => Some(literal(lit)),
        Expr::Apply(path, args) => {
            let names: Vec<&str> = path.iter().map(String::as_str).collect();
            match (&names[..], &args[..]) {
                (["len"], [Arg::Pos(x)]) => Some(format!("the length of {}", noun(x, cx)?)),
                ([model, "get_by" | "get" | "first_by"], _) if is_class(model) => {
                    Some(format!("the {} {}", words(model), with_these(args)?))
                }
                // Go request input: `r.FormValue("email")` is the email.
                (
                    ["r" | "req" | "request", "FormValue" | "PostFormValue"],
                    [Arg::Pos(Expr::Lit(Lit::Str(key)))],
                ) => Some(format!("the {}", words(key))),
                // A signed value read back (`signer.unsign(s, max_age=600)`,
                // `serializer.loads(s, max_age=600)`): the value with its
                // signature checked, and its age when one is given. A
                // `loads` without `max_age` (`json.loads(s)`) checks nothing,
                // so only an age limit marks it as signed (AC-1036).
                ([_, .., method @ ("unsign" | "loads")], [Arg::Pos(value), rest @ ..])
                    if rest.len() <= 1
                        && (*method == "unsign" || rest.len() == 1)
                        && rest.iter().all(
                            |a| matches!(a, Arg::Kw(k, Expr::Lit(Lit::Num(_))) if k == "max_age"),
                        ) =>
                {
                    let value = noun(value, cx)?;
                    Some(match rest {
                        [Arg::Kw(_, Expr::Lit(age))] => format!(
                            "{value}, once its signature is checked and it is at most {} seconds old",
                            literal(age)
                        ),
                        _ => format!("{value}, once its signature is checked"),
                    })
                }
                ([function], _) if predicate_verb(function).is_none() => Some(format!(
                    "the “{}” result",
                    words(function.trim_start_matches('_'))
                )),
                // A method's value read off its receiver (AC-1030), with
                // what it is given: `user.available_alias_domains(alias_options=o)`
                // is "the user's available alias domains for the o", so
                // calls given different inputs stay apart.
                ([receiver, .., method], _)
                    if !is_class(receiver) && predicate_verb(method).is_none() =>
                {
                    let what = subject(&accessor(path), cx)?;
                    // A literal keyword input keeps its name (AC-1036):
                    // `timeout=30` is "timeout 30".
                    let inputs: Option<Vec<String>> = args
                        .iter()
                        .map(|a| match a {
                            Arg::Kw(k, Expr::Lit(lit @ (Lit::Num(_) | Lit::Bool(_)))) => {
                                Some(format!("{} {}", words(k), literal(lit)))
                            }
                            Arg::Pos(v) | Arg::Kw(_, v) => noun(v, cx),
                        })
                        .collect();
                    let inputs = inputs?;
                    Some(if inputs.is_empty() {
                        what
                    } else {
                        format!("{what} for {}", inputs.join(" and "))
                    })
                }
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

/// A compared value as a noun phrase: a sum only here, where a condition
/// compares it (AC-1030), "the active alias count plus the num aliases";
/// an assigned or returned sum stays outside the rendering rules.
fn compared(expr: &Expr, cx: &Context<'_>) -> Option<String> {
    match expr {
        Expr::Sum(terms) => {
            let parts: Option<Vec<String>> = terms.iter().map(|t| noun(t, cx)).collect();
            Some(parts?.join(" plus "))
        }
        other => noun(other, cx),
    }
}

/// A call's path with an accessor's `get_` dropped (AC-1030):
/// `self.get_active_subscription` reads "the user's active subscription".
fn accessor(path: &[String]) -> Vec<String> {
    let mut path = path.to_vec();
    if path.len() > 1
        && let Some(last) = path.last_mut()
        && let Some(rest) = last.strip_prefix("get_").filter(|r| !r.is_empty())
    {
        *last = rest.to_owned();
    }
    path
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
    // A receiver's predicate of one value is said of that value:
    // `s.isCatalogOwner(u)` is "the u is catalog owner" (#2009).
    if let (Some((verb, rest)), [receiver], [Arg::Pos(value)]) = (predicate_verb(last), owner, args)
        && (go_receiver(receiver, cx) || matches!(*receiver, "self" | "cls"))
    {
        let verb = if negated { negated_verb(verb) } else { verb };
        // A literal is how much, not whom (AC-1030):
        // `self.can_create_num_aliases(1)` is "the user can create num
        // aliases (1)".
        if let Expr::Lit(lit @ Lit::Num(_)) = value {
            let who = subject(&path[..1], cx)?;
            return Some(format!("{who} {verb} {} ({})", words(rest), literal(lit)));
        }
        return Some(format!("{} {verb} {}", noun(value, cx)?, words(rest)));
    }
    // A named receiver's predicate given a number keeps it (AC-1030):
    // `user.can_create_num_aliases(1)` is "the user can create num aliases
    // (1)", not the same rule without its quota.
    if let (Some((verb, rest)), [receiver], [Arg::Pos(Expr::Lit(lit @ Lit::Num(_)))]) =
        (predicate_verb(last), owner, args)
        && !is_class(receiver)
    {
        let verb = if negated { negated_verb(verb) } else { verb };
        let who = subject(&path[..1], cx)?;
        return Some(format!("{who} {verb} {} ({})", words(rest), literal(lit)));
    }
    if predicate_verb(last).is_some() {
        return flag(path, negated, cx);
    }
    match (&names[..], args) {
        // Go `errors.Is(err, store.ErrNotFound)`: the error is “not found”.
        (["errors", "Is"], [Arg::Pos(_), Arg::Pos(Expr::Ref(target))]) => {
            let name = target.last()?.strip_prefix("Err")?;
            let what = words(name);
            return Some(if negated {
                format!("the error is not “{what}”")
            } else {
                format!("the error is “{what}”")
            });
        }
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
    // A function named in more than one word, in snake or camel case
    // (`orderItemsAreShipped`, AC-1032).
    if owner.is_empty() && words(last.trim_start_matches('_')).contains(' ') {
        let name = words(last.trim_start_matches('_'));
        return Some(if negated {
            format!("the check “{name}” does not hold")
        } else {
            format!("the check “{name}” holds")
        });
    }
    // A check its receiver runs (AC-1030):
    // `self.lifetime_or_active_subscription()` in a User method is "the
    // check “lifetime or active subscription” holds for the user".
    if args.is_empty()
        && !owner.is_empty()
        && !owner.iter().any(|o| is_class(o))
        && last.trim_start_matches('_').contains('_')
    {
        let name = words(last.trim_start_matches('_'));
        let who = subject(&path[..path.len() - 1], cx)?;
        return Some(if negated {
            format!("the check “{name}” does not hold for {who}")
        } else {
            format!("the check “{name}” holds for {who}")
        });
    }
    None
}

/// Whether a value names where request input arrives: `data`,
/// `request.form`, `request.args`, `request.json`.
fn request_input(path: &[String]) -> bool {
    // The request's own inputs (`request.form`, `request.json`), or a bare
    // local holding them (`data`, `payload`); not a response's or a cache's
    // member of the same name.
    match path {
        [one] => matches!(
            one.as_str(),
            "data" | "form" | "json" | "payload" | "body" | "args" | "values"
        ),
        [root, .., last] => {
            root == "request"
                && matches!(
                    last.as_str(),
                    "data" | "form" | "args" | "json" | "values" | "files" | "payload" | "body"
                )
        }
        [] => false,
    }
}

fn literal(lit: &Lit) -> String {
    match lit {
        Lit::Str(s) if s == "\\n" => "a line break".to_owned(),
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
        ("applies_", "applies"),
    ] {
        if let Some(rest) = name.strip_prefix(prefix).filter(|r| !r.is_empty()) {
            return Some((verb, rest));
        }
        // Go and JavaScript camel case: `isCatalogOwner` (#2009).
        if let Some(rest) = name
            .strip_prefix(&prefix[..prefix.len() - 1])
            .filter(|r| r.starts_with(|c: char| c.is_ascii_uppercase()))
        {
            return Some((verb, rest));
        }
    }
    None
}

/// Whether `name` is a Go method's receiver: one or two lower-case letters
/// that start the owner's type name (`s` in a `Service` method), read as
/// that type the way `self` is (#2009).
fn go_receiver(name: &str, cx: &Context<'_>) -> bool {
    cx.class.is_some_and(|class| {
        (1..=2).contains(&name.len())
            && name.chars().all(|c| c.is_ascii_lowercase())
            && class.to_ascii_lowercase().starts_with(name)
    })
}

fn negated_verb(verb: &str) -> &'static str {
    match verb {
        "is" => "is not",
        "has" => "has no",
        "can" => "cannot",
        "should" => "should not",
        "must" => "must not",
        "applies" => "does not apply",
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
            let rest = words(rest);
            // "has no an authorized payment" drops its article (AC-1032).
            let rest = match verb {
                "has no" => rest
                    .strip_prefix("an ")
                    .or_else(|| rest.strip_prefix("a "))
                    .unwrap_or(&rest)
                    .to_owned(),
                _ => rest,
            };
            return Some(format!("the check “{verb} {rest}” holds"));
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
    if last == "length" && !owner.is_empty() {
        // A list's length tested for truth: `found.length` reads "the
        // found are not empty" (AC-1050).
        return Some(if negated {
            format!("{who} are empty")
        } else {
            format!("{who} are not empty")
        });
    }
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

/// The joiner a compound renders with under `negated`, or `None` for a
/// single term.
fn joined_by(expr: &Expr, negated: bool) -> Option<&'static str> {
    match (expr, negated) {
        (Expr::Not(inner), _) => joined_by(inner, !negated),
        (Expr::And(_), false) | (Expr::Or(_), true) => Some(" and "),
        (Expr::And(_), true) | (Expr::Or(_), false) => Some(" or "),
        _ => None,
    }
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
            // A nested group with the other joiner keeps its grouping:
            // `a and (b or c)` is "a and either b or c", never "(a and b)
            // or c".
            let parts: Option<Vec<String>> = terms
                .iter()
                .map(|t| {
                    let part = render(t, negated, cx)?;
                    Some(match joined_by(t, negated) {
                        Some(inner) if inner != joiner && inner == " or " => {
                            format!("either {part}")
                        }
                        Some(inner) if inner != joiner => format!("both {part}"),
                        _ => part,
                    })
                })
                .collect();
            Some(parts?.join(joiner))
        }
        Expr::Cmp(left, op, right) => {
            // A flag bit (AC-1030): `FLAG == self.flags & FLAG` is "the
            // user's flags include the “free old alias limit” flag".
            if let (bit, "==" | "!=", Expr::Mask(field, mask))
            | (Expr::Mask(field, mask), "==" | "!=", bit) =
                (left.as_ref(), op.as_str(), right.as_ref())
                && let (Expr::Ref(b), Expr::Ref(m)) = (bit, mask.as_ref())
                && b.last() == m.last()
                && let Some(name) = m.last()
            {
                let field = noun(field, cx)?;
                let name = words(name.strip_prefix("FLAG_").unwrap_or(name));
                let set = (op == "==") != negated;
                return Some(if set {
                    format!("{field} include the “{name}” flag")
                } else {
                    format!("{field} do not include the “{name}” flag")
                });
            }
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
                if text == "\\n" {
                    let whole = noun(right, cx)?;
                    return Some(if present {
                        format!("{whole} contains a line break")
                    } else {
                        format!("{whole} does not contain a line break")
                    });
                }
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
            let left_np = compared(left, cx)?;
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
                    ("==", Expr::Lit(Lit::Str(empty))) if empty.is_empty() => {
                        format!("{left_np} is empty")
                    }
                    ("!=", Expr::Lit(Lit::Str(empty))) if empty.is_empty() => {
                        format!("{left_np} is not empty")
                    }
                    // Go `err != nil`: an error occurred (#2009).
                    ("is" | "==", Expr::Lit(Lit::None)) if go_error_value(left) => {
                        match cx.failed {
                            Some(call) => {
                                format!("the “{}” call succeeds", words(&call.replace('.', "_")))
                            }
                            None => "no error occurred".to_owned(),
                        }
                    }
                    ("is not" | "!=", Expr::Lit(Lit::None)) if go_error_value(left) => {
                        match cx.failed {
                            Some(call) => {
                                format!("the “{}” call fails", words(&call.replace('.', "_")))
                            }
                            None => "an error occurred".to_owned(),
                        }
                    }
                    ("is" | "==", Expr::Lit(Lit::None)) => format!("{left_np} is not set"),
                    ("is not" | "!=", Expr::Lit(Lit::None)) => format!("{left_np} is set"),
                    ("==" | "is", r) => format!("{left_np} is {}", compared(r, cx)?),
                    ("!=" | "is not", r) => format!("{left_np} is not {}", compared(r, cx)?),
                    (">", r) => format!("{left_np} is more than {}", compared(r, cx)?),
                    (">=", r) => format!("{left_np} is at least {}", compared(r, cx)?),
                    ("<", r) => format!("{left_np} is less than {}", compared(r, cx)?),
                    ("<=", r) => format!("{left_np} is at most {}", compared(r, cx)?),
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
        Expr::Lit(_)
        | Expr::Set(_)
        | Expr::Shift(..)
        | Expr::Sum(_)
        | Expr::Difference(_)
        | Expr::Mask(..) => None,
    }
}

/// Which of an exit's conditions an `elif` chain implies (#2083, AC-1004):
/// a test that a value differs from a literal, when another condition pins
/// the same source expression to another literal. "the form name is not
/// “cancel”, and the form name is “change-yearly”" says only the second,
/// which a reader (and a small model) cannot misread as no option matching.
/// Values are matched by their source tokens, so two inputs that read alike
/// (a form field and a query parameter) are never taken for one.
fn implied_by_pin(conditions: &[(&str, BranchPolarity)]) -> Vec<bool> {
    // The value's identity is its source tokens, not its parsed form: the
    // parser reads `data.get("mode", "cancel")` as `data.mode` and drops the
    // default, so two getters with different defaults would look alike.
    let operand = |display: &str| -> Option<Vec<Tok>> {
        let mut toks = tokenize(display)?;
        let comparison = |tok: &Tok| matches!(tok, Tok::Op(op) if op == "==" || op == "!=");
        let literal = |tok: &Tok| match tok {
            Tok::Str(_) | Tok::Num(_) => true,
            Tok::Ident(word) => matches!(word.as_str(), "True" | "False" | "true" | "false"),
            Tok::Op(_) => false,
        };
        let n = toks.len();
        if n >= 3 && comparison(&toks[n - 2]) && literal(&toks[n - 1]) {
            toks.truncate(n - 2);
        } else if n >= 3 && comparison(&toks[1]) && literal(&toks[0]) {
            toks.drain(..2);
        } else {
            return None;
        }
        Some(toks)
    };
    let raised = |polarity: BranchPolarity| {
        matches!(
            polarity,
            BranchPolarity::RaisedBranch | BranchPolarity::UnraisedBranch
        )
    };
    let equality = |display: &str, polarity: BranchPolarity| -> Option<(Vec<Tok>, bool, Lit)> {
        if raised(polarity) {
            return None;
        }
        let source = operand(display)?;
        let mut expr = parse(display)?;
        let mut negated = polarity == BranchPolarity::FalsyBranch;
        while let Expr::Not(inner) = expr {
            expr = *inner;
            negated = !negated;
        }
        let Expr::Cmp(left, op, right) = expr else {
            return None;
        };
        let equal = match op.as_str() {
            "==" => !negated,
            "!=" => negated,
            _ => return None,
        };
        // Either operand order: `mode == "yearly"` and `"yearly" == mode`.
        match (*left, *right) {
            (Expr::Ref(_) | Expr::Call(_) | Expr::Apply(..), Expr::Lit(lit))
            | (Expr::Lit(lit), Expr::Ref(_) | Expr::Call(_) | Expr::Apply(..))
                if !matches!(lit, Lit::None) =>
            {
                Some((source, equal, lit))
            }
            _ => None,
        }
    };
    // Two literals proven to be different values: texts without escapes that
    // differ, numbers whose values differ (`1_000` is `1000`), or booleans.
    // Literals of different kinds are never proven distinct (`1 == True`).
    let distinct = |a: &Lit, b: &Lit| match (a, b) {
        (Lit::Str(a), Lit::Str(b)) => !a.contains('\\') && !b.contains('\\') && a != b,
        (Lit::Num(a), Lit::Num(b)) => {
            let value = |n: &str| n.replace('_', "").parse::<f64>().ok();
            matches!((value(a), value(b)), (Some(a), Some(b)) if a != b)
        }
        (Lit::Bool(a), Lit::Bool(b)) => a != b,
        _ => false,
    };
    let tests: Vec<Option<(Vec<Tok>, bool, Lit)>> = conditions
        .iter()
        .map(|&(display, polarity)| equality(display, polarity))
        .collect();
    // An elif chain's earlier tests (#1998): `not (alias and …)` says nothing
    // once another condition says `not alias`, for a plain local name.
    let tested = |display: &str, polarity: BranchPolarity| -> Option<(Expr, bool)> {
        if raised(polarity) {
            return None;
        }
        let mut expr = parse(display)?;
        let mut negated = polarity == BranchPolarity::FalsyBranch;
        while let Expr::Not(inner) = expr {
            expr = *inner;
            negated = !negated;
        }
        Some((expr, negated))
    };
    let absent: Vec<Option<String>> = conditions
        .iter()
        .map(|&(display, polarity)| match tested(display, polarity)? {
            (Expr::Ref(path), true) if path.len() == 1 => Some(path[0].clone()),
            _ => None,
        })
        .collect();
    let refuted_conjunction: Vec<Option<String>> = conditions
        .iter()
        .map(|&(display, polarity)| match tested(display, polarity)? {
            (Expr::And(parts), true) => match parts.first() {
                Some(Expr::Ref(path)) if path.len() == 1 => Some(path[0].clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    tests
        .iter()
        .enumerate()
        .map(|(i, test)| {
            let pinned = test.as_ref().is_some_and(|(value, equal, lit)| {
                !equal
                    && tests
                        .iter()
                        .flatten()
                        .any(|(other, pinned, to)| *pinned && other == value && distinct(to, lit))
            });
            let absorbed = refuted_conjunction[i].as_ref().is_some_and(|name| {
                absent
                    .iter()
                    .enumerate()
                    .any(|(j, other)| j != i && other.as_ref() == Some(name))
            });
            pinned || absorbed
        })
        .collect()
}

/// One stored condition in domain terms, with its branch polarity.
pub(super) fn condition(
    display: &str,
    polarity: BranchPolarity,
    cx: &Context<'_>,
) -> Option<String> {
    let expr = parse(display)?;
    match polarity {
        BranchPolarity::RaisedBranch => Some(format!("{} is raised", raised_error(&expr)?)),
        BranchPolarity::UnraisedBranch => {
            let error = raised_error(&expr)?;
            let error = error
                .strip_prefix("an ")
                .or(error.strip_prefix("a "))
                .unwrap_or(&error);
            Some(format!("no {error} is raised"))
        }
        _ => render(&expr, polarity == BranchPolarity::FalsyBranch, cx),
    }
}

/// The error type an `except` names, as a phrase (AC-1036):
/// `itsdangerous.BadSignature` is "a bad signature error", `ValueError` "a
/// value error", and `Exception` "an error".
fn raised_error(expr: &Expr) -> Option<String> {
    let Expr::Ref(path) = expr else {
        return None;
    };
    let last = path.last()?;
    if !is_class(last) {
        return None;
    }
    let mut what = words(last);
    for suffix in [" error", " exception"] {
        if let Some(rest) = what.strip_suffix(suffix) {
            what = rest.to_owned();
        }
    }
    if matches!(what.as_str(), "error" | "exception" | "base exception" | "") {
        return Some("an error".to_owned());
    }
    let article = if what.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    Some(format!("{article} {what} error"))
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

/// A Go error variable: `err`, `readErr`.
fn go_error_value(expr: &Expr) -> bool {
    matches!(expr, Expr::Ref(path) if path.len() == 1
        && (path[0] == "err" || path[0].ends_with("Err")))
}

/// The top-level comma-separated parts of `text`; brackets and quoted
/// literals keep their commas. `None` for an unterminated literal.
fn top_level_parts(text: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' && q != '`' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(text[start..].trim());
    Some(parts)
}

/// A plain double-quoted literal's text, without escapes or nested quotes.
fn plain_message(arg: &str) -> Option<String> {
    let inner = arg.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.is_empty() && !inner.contains(['"', '\'', '{', '\\'])).then(|| inner.to_owned())
}

/// A Java string literal's text with its escapes decoded (`"User's id"`,
/// `"Invalid\nID"`); line breaks and tabs read as spaces. `None` for
/// anything but one plain literal, an empty one, a unicode or octal escape,
/// or a `{placeholder}`.
fn java_message(arg: &str) -> Option<String> {
    let mut chars = arg.strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '\\' => out.push('\\'),
                'n' | 't' | 'r' | 'f' | 'b' | 's' => out.push(' '),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    let message = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (chars.as_str().is_empty() && !message.is_empty() && !message.contains('{')).then_some(message)
}

/// Whether a rule was recovered from Go source: the Go response and error
/// shapes are read only there, never in another language's display text.
pub(super) fn is_go(evidence: &GuardedExitEvidence) -> bool {
    evidence.exit_source.path.ends_with(".go")
}

/// Whether a rule was recovered from Java source (#2020).
pub(super) fn is_java(evidence: &GuardedExitEvidence) -> bool {
    evidence.exit_source.path.ends_with(".java")
}

/// Whether a rule was recovered from Kotlin source (#2021).
pub(super) fn is_kotlin(evidence: &GuardedExitEvidence) -> bool {
    kotlin_path(&evidence.exit_source.path)
}

/// A Kotlin source or script path.
fn kotlin_path(path: &str) -> bool {
    path.ends_with(".kt") || path.ends_with(".kts")
}

/// The error a Kotlin `throw` raises: Kotlin constructs without `new`, so
/// only a call of a capitalized class name is read, as a Java one is.
pub(super) fn kotlin_thrown(display: &str) -> Option<String> {
    let display = display.trim();
    // A precondition's message literal (`require(…) { "…" }`) or an
    // `error("…")` call is the message it fails with (AC-1108).
    if display.starts_with('"') {
        return java_thrown(&format!("new IllegalArgumentException({display})"));
    }
    if let Some(arguments) = display.strip_prefix("error(") {
        return java_thrown(&format!("new IllegalStateException({arguments}"));
    }
    let class = display.split('(').next()?;
    let simple = class.rsplit('.').next()?;
    if !simple.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    java_thrown(&format!("new {display}"))
}

/// `a “login taken” error`, `an “expired” error`.
fn an_error_named(name: &str) -> String {
    let article = if name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!("{article} “{name}” error")
}

/// Whether a rule was recovered from Rust source (#2018).
pub(super) fn is_rust(evidence: &GuardedExitEvidence) -> bool {
    evidence.exit_source.path.ends_with(".rs")
}

/// The status a Rust handler returns and its literal message (#2018):
/// `Err(StatusCode::NOT_FOUND)` gives `(404, None)`; `(StatusCode::FORBIDDEN,
/// "not yours")`, bare or in `Err(…)`, gives `(403, Some("not yours"))`.
/// `None` unless the value is an `http::StatusCode` constant or a tuple
/// holding one.
pub(super) fn rust_status(display: &str) -> Option<(u16, Option<String>)> {
    let mut text = display.trim();
    if let Some(inner) = text.strip_prefix("Err(").and_then(|t| t.strip_suffix(')')) {
        text = inner.trim();
    }
    let constant = |part: &str| {
        let part = part.trim();
        let (path, name) = part.rsplit_once("::")?;
        (path == "StatusCode" || path.ends_with("::StatusCode"))
            .then(|| rust_http_status(name))
            .flatten()
    };
    if let Some(status) = constant(text) {
        return Some((status, None));
    }
    let inner = text.strip_prefix('(')?.strip_suffix(')')?;
    let parts = top_level_parts(inner)?;
    let status = parts.iter().find_map(|part| constant(part))?;
    let messages: Vec<String> = parts.iter().filter_map(|part| rust_message(part)).collect();
    let message = match &messages[..] {
        [one] => Some(one.clone()),
        _ => None,
    };
    Some((status, message))
}

/// A Rust string literal's text, bare or made a `String` (`"x".into()`,
/// `.to_string()`, `.to_owned()`, `String::from("x")`).
fn rust_message(part: &str) -> Option<String> {
    let part = part.trim();
    let literal = [".into()", ".to_string()", ".to_owned()"]
        .iter()
        .find_map(|tail| part.strip_suffix(tail))
        .or_else(|| {
            part.strip_prefix("String::from(")
                .and_then(|p| p.strip_suffix(')'))
        })
        .unwrap_or(part);
    java_message(literal.trim())
}

/// The error a Rust `Err(…)` returns, in words (#2018): a literal message
/// (`anyhow!("too short")`, `"too short".into()`, `Error::new("too
/// short")`) gives `the error “too short”`; an enum variant
/// (`ApiError::LoginTaken`, `Error::NotFound(id)`) its name in words, `a
/// “login taken” error`; anything else, `an error`. `None` for a value
/// that is not `Err(…)`.
pub(super) fn rust_error(display: &str) -> Option<String> {
    let inner = display
        .trim()
        .strip_prefix("Err(")?
        .strip_suffix(')')?
        .trim();
    let message = |args: &str| {
        top_level_parts(args).and_then(|parts| parts.first().and_then(|first| rust_message(first)))
    };
    for maker in [
        "anyhow!(",
        "anyhow::anyhow!(",
        "eyre!(",
        "eyre::eyre!(",
        "format_err!(",
    ] {
        if let Some(args) = inner.strip_prefix(maker).and_then(|a| a.strip_suffix(')')) {
            return Some(match message(args) {
                Some(m) => format!("the error “{m}”"),
                None => "an error".to_owned(),
            });
        }
    }
    if let Some(m) = rust_message(inner) {
        return Some(format!("the error “{m}”"));
    }
    // `Error::new("…")`, `Error::msg("…")`, `ApiError::NotFound(id)`.
    let head_len = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(inner.len());
    let (head, rest) = inner.split_at(head_len);
    let rest = rest.trim();
    let segments: Vec<&str> = head.split("::").collect();
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
        return Some("an error".to_owned());
    }
    let last = *segments.last()?;
    if matches!(last, "new" | "msg" | "custom" | "from")
        && let Some(args) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')'))
    {
        return Some(match message(args) {
            Some(m) => format!("the error “{m}”"),
            None => "an error".to_owned(),
        });
    }
    // The variant's own fields, nothing after them (`.context(…)`).
    let wraps = |open: char, close: char| {
        let mut depth = 0usize;
        let mut quote = false;
        let mut escaped = false;
        for (at, c) in rest.char_indices() {
            if quote {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => quote = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => quote = true,
                _ if c == open => depth += 1,
                _ if c == close => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(at + c.len_utf8() == rest.len());
                    }
                }
                _ => {}
            }
        }
        None
    };
    let wraps = |open, close| rest.starts_with(open) && wraps(open, close) == Some(true);
    let shaped = rest.is_empty() || wraps('(', ')') || wraps('{', '}');
    if !shaped || !last.starts_with(|c: char| c.is_ascii_uppercase()) {
        return Some("an error".to_owned());
    }
    let name = last
        .strip_suffix("Error")
        .filter(|n| !n.is_empty())
        .unwrap_or(last);
    Some(an_error_named(&words(name)))
}

/// The error a Java `throw new X(…)` raises, in words (#2020): its literal
/// message when its first argument is one (`new BadRequestAlertException("Invalid
/// id", ENTITY_NAME, "idnull")` gives `the error “Invalid id”`), else the
/// exception's name without its `Exception`/`Error` suffix
/// (`new LoginAlreadyUsedException()` gives `a “login already used” error`).
/// `None` for any other thrown expression.
pub(super) fn java_thrown(display: &str) -> Option<String> {
    let rest = display.trim().strip_prefix("new ")?;
    let open = rest.find('(')?;
    let class = rest[..open].trim();
    if class.is_empty()
        || !class
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    let inner = rest[open + 1..].strip_suffix(')')?;
    let parts = top_level_parts(inner)?;
    if let Some(message) = parts.first().and_then(|first| java_message(first.trim())) {
        return Some(format!("the error “{message}”"));
    }
    let name = class.rsplit('.').next().unwrap_or(class);
    let name = name
        .strip_suffix("Exception")
        .or_else(|| name.strip_suffix("Error"))
        .unwrap_or(name);
    // `new RuntimeException(e)` names no rule of its own.
    if name.is_empty() || matches!(name, "Runtime" | "IllegalState" | "IllegalArgument") {
        return Some("an error".to_owned());
    }
    Some(an_error_named(&words(name)))
}

/// The status a Go response call writes and its literal message (#2009):
/// `http.Error(w, "forbidden", http.StatusForbidden)` gives `(403,
/// Some("forbidden"))`; `w.WriteHeader(400)` gives `(400, None)`. `None`
/// unless an argument is a `net/http` status constant or a status number.
pub(super) fn go_response(display: &str) -> Option<(u16, Option<String>)> {
    let display = display.trim();
    let open = display.find('(')?;
    let callee = &display[..open];
    if callee.is_empty()
        || !callee
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    let inner = display[open + 1..].strip_suffix(')')?;
    let parts = top_level_parts(inner)?;
    let status = parts.iter().find_map(|arg| {
        arg.strip_prefix("http.")
            .and_then(go_http_status)
            .or_else(|| arg.parse::<u16>().ok().filter(|s| (100..600).contains(s)))
    })?;
    // An error code (`"vault_locked"`) reads as words.
    let messages: Vec<String> = parts
        .iter()
        .filter_map(|arg| plain_message(arg))
        .map(|m| {
            if m.contains('_')
                && m.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                words(&m)
            } else {
                m
            }
        })
        .collect();
    let message = match &messages[..] {
        [one] => Some(one.clone()),
        _ => None,
    };
    Some((status, message))
}

/// A Go error value a guarded return passes back as its last result
/// (#2009): `errors.New("reserved")` or `fmt.Errorf("…")` gives its
/// message when plain, `ErrNotFound` or `store.ErrNotFound` its name in
/// words ("not found"). `None` for any other value, `err` included.
pub(super) fn go_error(display: &str) -> Option<String> {
    let parts = top_level_parts(display.trim())?;
    let last = *parts.last()?;
    for constructor in ["errors.New(", "fmt.Errorf("] {
        if let Some(args) = last
            .strip_prefix(constructor)
            .and_then(|a| a.strip_suffix(')'))
        {
            let first = top_level_parts(args)?.first().copied()?;
            // A formatted message (`"load %s: %w"`) is not stated.
            return Some(
                plain_message(first)
                    .filter(|m| !m.contains('%'))
                    .unwrap_or_default(),
            );
        }
    }
    let name = last.rsplit('.').next()?;
    let all_ident = last
        .split('.')
        .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    let rest = name.strip_prefix("Err")?;
    (all_ident && rest.starts_with(|c: char| c.is_ascii_uppercase())).then(|| words(rest))
}

/// Python `alias.name = alias.name.replace("\n", "")` as "line breaks are
/// removed from the alias's name" (#1998). Python's `str.replace` replaces
/// every occurrence; JavaScript's replaces only the first, so callers apply
/// this to Python evidence only. `None` for any other assignment.
fn line_breaks_removed(target: &str, value: &str, cx: &Context<'_>) -> Option<String> {
    let args = value
        .strip_prefix(target.trim())?
        .strip_prefix(".replace(")?
        .strip_suffix(')')?;
    let (from, to) = args.split_once(',')?;
    if !matches!(
        (from.trim(), to.trim()),
        ("\"\\n\"" | "'\\n'", "\"\"" | "''")
    ) {
        return None;
    }
    let Expr::Ref(path) = parse(target.trim())? else {
        return None;
    };
    Some(format!(
        "line breaks are removed from {}",
        subject(&path, cx)?
    ))
}

/// `x.field = value` as a clause (#1998): `g.user = api_key.user` is "the
/// request acts as the API key's user"; `alias.enabled = False` is "the
/// alias is no longer enabled"; `alias.delete_on = arrow.now().shift(days=+N)`
/// is "the alias's delete on is set to N days from now"; otherwise "the
/// alias's note is set to the note". `None` outside these rules.
/// `found.push(line.productVariant)`, which the adapter admits only for a
/// list the callable starts empty (AC-1050): "the line's product variant
/// is added to the found".
fn pushed(call: &str, cx: &Context<'_>) -> Option<String> {
    let Expr::Apply(path, args) = parse(call.trim())? else {
        return None;
    };
    let ([list, push], [Arg::Pos(value)]) = (&path[..], &args[..]) else {
        return None;
    };
    if push != "push" {
        return None;
    }
    Some(format!(
        "{} is added to {}",
        noun(value, cx)?,
        noun(&Expr::Ref(vec![list.clone()]), cx)?
    ))
}

fn assigned(target: &str, value: &str, cx: &Context<'_>) -> Option<String> {
    let Expr::Ref(path) = parse(target.trim())? else {
        return None;
    };
    // An accumulated total (AC-1054): the adapter keeps the whole
    // `total += x`, so "x is added to the total".
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    for (operator, verb) in [("+=", "added to"), ("-=", "taken from")] {
        if let Some(rest) = compact
            .strip_prefix(target.trim())
            .and_then(|r| r.trim_start().strip_prefix(operator))
        {
            return Some(format!(
                "{} is {verb} {}",
                noun(&parse(rest.trim().trim_end_matches(';'))?, cx)?,
                subject(&path, cx)?
            ));
        }
    }
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
    // A local set to `None` under a branch: the value found is discarded.
    if matches!(parsed, Expr::Lit(Lit::None)) && path.len() == 1 {
        return Some(format!("{place} is discarded"));
    }
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
    // TypeScript repository writes and published events (#1998).
    if let Some(clause) = repository_write(value).or_else(|| published(value)) {
        return Some(clause);
    }
    // `this.historyService.createEntry(…)`: the method is the action.
    let value = value.strip_prefix("this.").unwrap_or(value);
    let callee = &value[..value.find('(')?];
    let mut segments = callee.split('.');
    if !segments
        .clone()
        .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        return None;
    }
    let last = segments.next_back()?;
    // `AliasUsedOn.create(…)` / `DeletedAlias.delete(…)` on a class: a
    // record created or deleted (#1998).
    let receiver: Vec<&str> = segments.clone().collect();
    // `DeletedAlias(email=…)` a Python adapter admits only as the record
    // `Session.add` adds (a SQLAlchemy constructor takes keywords, which a
    // Go call never has); `Alias.filter(…).delete()` deletes the matches.
    let keywords = || {
        let bytes = value.as_bytes();
        (callee.len()..bytes.len()).any(|i| {
            bytes[i] == b'='
                && bytes.get(i + 1) != Some(&b'=')
                && !matches!(bytes[i - 1], b'=' | b'!' | b'<' | b'>')
        })
    };
    if receiver.is_empty() && last.starts_with(|c: char| c.is_ascii_uppercase()) && keywords() {
        return Some(format!("a new “{}” record is created", words(last)));
    }
    if let [model] = receiver[..]
        && model.starts_with(|c: char| c.is_ascii_uppercase())
        && matches!(last, "filter" | "filter_by")
        && value.trim_end().ends_with(".delete()")
    {
        return Some(format!(
            "the matching “{}” records are deleted",
            words(model)
        ));
    }
    if let [model] = receiver[..]
        && model.starts_with(|c: char| c.is_ascii_uppercase())
        && model != "Session"
    {
        match last {
            "create" => return Some(format!("a new “{}” record is created", words(model))),
            "delete" => return Some(format!("the “{}” record is deleted", words(model))),
            _ => {}
        }
    }
    // Flask `flash("Alias not found", "error")`: the message the user sees;
    // an f-string's replacement fields read as "…" (#1998).
    if last == "flash"
        && let Some(message) = flashed(&value[callee.len() + 1..])
    {
        return Some(format!("the user is shown “{message}”"));
    }
    Some(format!("it runs “{}”", words(last.trim_start_matches('_'))))
}

/// The message of `flash("…", …)` or `flash(f"Deleted {count} aliases", …)`,
/// with each replacement field as "…". `None` for an escape, a quote inside,
/// a doubled brace or no literal text.
fn flashed(arguments: &str) -> Option<String> {
    let rest = arguments.trim_start();
    let (formatted, rest) = match rest.strip_prefix(['f', 'F']) {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let (message, _) = rest[1..].split_once(quote)?;
    if message.contains(['\\', '"', '\'']) || message.contains("{{") || message.contains("}}") {
        return None;
    }
    if !formatted {
        return (!message.is_empty() && !message.contains(['{', '}'])).then(|| message.to_owned());
    }
    let mut out = String::new();
    let mut rest = message;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}')? + open;
        if rest[open + 1..close].contains('{') {
            return None;
        }
        out.push_str(&rest[..open]);
        out.push('…');
        rest = &rest[close + 1..];
    }
    if rest.contains('}') {
        return None;
    }
    out.push_str(rest);
    out.chars().any(|c| c.is_alphabetic()).then_some(out)
}

/// A TypeORM repository write as a clause (#1998):
/// `this.connection.getRepository(ctx, Order).save(order)` is "the “order”
/// record is saved", `this.orderItemRepository.delete(id)` is "the “order
/// item” record is deleted". `None` for another call.
fn repository_write(value: &str) -> Option<String> {
    // A chain split over lines (`this.connection\n  .getRepository(…)`).
    let mut compact = String::with_capacity(value.len());
    for part in value.split('.') {
        if !compact.is_empty() {
            compact.truncate(compact.trim_end().len());
            compact.push('.');
        }
        compact.push_str(if compact.is_empty() {
            part
        } else {
            part.trim_start()
        });
    }
    let value = compact.as_str();
    let (entity, rest) = if let Some(at) = value.find(".getRepository(") {
        let args = &value[at + ".getRepository(".len()..];
        let close = args.find(')')?;
        let entity = args[..close].rsplit(',').next()?.trim();
        (entity, &args[close + 1..])
    } else {
        // A `this.…Repository` field: only TypeScript evidence has `this`.
        let callee = &value[..value.find('(')?];
        let (object, _) = callee.rsplit_once('.')?;
        if !object.starts_with("this.") {
            return None;
        }
        let entity = object.rsplit('.').next()?.strip_suffix("Repository")?;
        (entity, &value[object.len()..])
    };
    if entity.is_empty()
        || !entity
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let verb = rest.strip_prefix('.')?.split('(').next()?;
    let noun = words(entity);
    Some(match verb {
        "save" | "upsert" => format!("the “{noun}” record is saved"),
        "insert" => format!("a new “{noun}” record is created"),
        "update" | "increment" | "decrement" => format!("the “{noun}” record is updated"),
        "delete" | "remove" | "softDelete" | "softRemove" => {
            format!("the “{noun}” record is deleted")
        }
        _ => return None,
    })
}

/// `this.eventBus.publish(new OrderStateTransitionEvent(…))` is "the “order
/// state transition” event is published" (#1998).
fn published(value: &str) -> Option<String> {
    let args = value
        .strip_prefix("this.eventBus.publish(")?
        .trim_start()
        .strip_prefix("new ")?;
    let class = &args[..args.find('(')?];
    if class.is_empty() || !class.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let name = class
        .strip_suffix("Event")
        .filter(|n| !n.is_empty())
        .unwrap_or(class);
    Some(format!("the “{}” event is published", words(name)))
}

/// A guarded exit's conditions with a given outcome clause (#2027): "When
/// the alias is not enabled, the mail is accepted with “250 …”". `None`
/// when a condition is outside the rendering rules or the sentence would
/// carry a code token.
pub(super) fn with_outcome(
    evidence: &GuardedExitEvidence,
    owner: &str,
    outcome: &str,
    constant: Constants<'_>,
    failed: &[Option<String>],
) -> Option<String> {
    let class = owner
        .rsplit_once('.')
        .and_then(|(class, _)| class.rsplit('.').next())
        .filter(|c| c.starts_with(|f: char| f.is_ascii_uppercase()));
    let implied = implied_by_pin(
        &evidence
            .conditions
            .iter()
            .map(|c| (c.expression.display.as_str(), c.polarity))
            .collect::<Vec<_>>(),
    );
    let conditions: Option<Vec<String>> = evidence
        .conditions
        .iter()
        .enumerate()
        .filter(|(i, _)| !implied[*i])
        .map(|(i, c)| {
            let cx = Context {
                class,
                constant,
                failed: failed.get(i).and_then(Option::as_deref),
            };
            condition(&tested_text(evidence, c), c.polarity, &cx)
        })
        .collect();
    let conditions = said_once(conditions?);
    let sentence = if conditions.is_empty() {
        format!("{}{}.", outcome[..1].to_uppercase(), &outcome[1..])
    } else {
        format!("When {}, {outcome}.", conditions.join(", and "))
    };
    Some(sentence)
        .filter(|s| crate::workbench::neutrality::projected_code_token(s).is_none())
        .map(|said| with_definitions(said, evidence, constant))
}

/// What a condition tests, as text to render: a bare local name whose one
/// recorded definition is itself a test (`const inventoryNotTracked = a
/// === X || b`) reads as that test (AC-1052), so "When the variant's track
/// inventory is “FALSE”, …" rather than "When an inventory not tracked is
/// given". Any other condition reads as written.
fn tested_text<'e>(
    evidence: &'e GuardedExitEvidence,
    c: &'e core_graph::rules::BranchCondition,
) -> std::borrow::Cow<'e, str> {
    let shown = c.expression.display.as_str();
    let bare = shown
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if bare.is_empty()
        || !bare
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return shown.into();
    }
    let within = |u: &core_prov::EvidenceRef| {
        c.expression.source.byte_start <= u.byte_start && u.byte_end <= c.expression.source.byte_end
    };
    let mut found = evidence
        .local_definitions
        .iter()
        .flatten()
        .filter(|d| d.uses.iter().any(within));
    let (Some(definition), None) = (found.next(), found.next()) else {
        return shown.into();
    };
    // The recorded root must itself be a test: `a || b`, `a && b` or a
    // comparison, through parentheses; a string or call that merely
    // spells one is not.
    use core_graph::rules::{BinaryOperator as B, DefinitionExpressionKind as K, LogicalOperator};
    let nodes = &definition.expression.nodes;
    let mut at = definition.expression.root as usize;
    if let Some(tested) = predicate_method_test(definition) {
        return tested.into();
    }
    let is_test = loop {
        match nodes.get(at).map(|n| &n.kind) {
            Some(K::Parenthesized { value } | K::Await { value }) => at = *value as usize,
            Some(K::Logical { operator, .. }) => {
                break matches!(operator, LogicalOperator::And | LogicalOperator::Or);
            }
            Some(K::Binary { operator, .. }) => {
                break matches!(
                    operator,
                    B::LooseEqual
                        | B::LooseNotEqual
                        | B::StrictEqual
                        | B::StrictNotEqual
                        | B::LessThan
                        | B::LessThanOrEqual
                        | B::GreaterThan
                        | B::GreaterThanOrEqual
                );
            }
            _ => break false,
        }
    };
    if definition.initializer.capture != core_graph::rules::ExpressionCapture::CompleteSyntax
        || !is_test
    {
        return shown.into();
    }
    format!("({})", one_line(definition.initializer.display.as_str())).into()
}

/// A local set from a predicate method of one of its arguments, `const
/// applies = await this.stockLevelAppliesToActiveChannel(ctx, stockLevel)`,
/// read as that test of the argument, `stockLevel.appliesToActiveChannel`:
/// "the stock level applies to active channel" (AC-1054). Only a complete
/// `this.<method>(<names>)` initializer, optionally awaited, qualifies; what
/// the method returns is not established.
fn predicate_method_test(definition: &core_graph::rules::LocalDefinition) -> Option<String> {
    if definition.initializer.capture != core_graph::rules::ExpressionCapture::CompleteSyntax {
        return None;
    }
    let text = one_line(definition.initializer.display.as_str());
    let call = text.strip_prefix("await ").unwrap_or(&text);
    let (method, args) = call
        .strip_prefix("this.")?
        .strip_suffix(')')?
        .split_once('(')?;
    let name = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !name(method) {
        return None;
    }
    let args: Vec<&str> = args
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .collect();
    if !args.iter().all(|a| name(a)) {
        return None;
    }
    args.iter().find_map(|arg| {
        let rest = method.strip_prefix(arg)?;
        let mut chars = rest.chars();
        let first = chars.next().filter(char::is_ascii_uppercase)?;
        let rest = format!("{}{}", first.to_ascii_lowercase(), chars.as_str());
        predicate_verb(&rest)
            .is_some()
            .then(|| format!("{arg}.{rest}"))
    })
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Conditions with repeats dropped: nested guards may test the same thing
/// twice, and a later `A and B` whose `A` is already said keeps only `B`.
fn said_once(conditions: Vec<String>) -> Vec<String> {
    let mut said: Vec<String> = Vec::new();
    for condition in conditions {
        let mut rest = condition.as_str();
        while let Some(stripped) = said
            .iter()
            .find_map(|s| rest.strip_prefix(s.as_str())?.strip_prefix(" and "))
        {
            rest = stripped;
        }
        if !said.iter().any(|s| s == rest) {
            said.push(rest.to_owned());
        }
    }
    said
}

/// The statement of one guarded exit in domain terms, or `None` when its
/// condition or outcome is outside the rendering rules. `owner` is the
/// owning callable's qualified name (`User.can_create_num_aliases`).
pub(super) fn statement(
    evidence: &GuardedExitEvidence,
    owner: &str,
    constant: Constants<'_>,
    failed: &[Option<String>],
) -> Option<String> {
    // A rendered statement that would still carry a code-shaped token is not
    // a statement in domain terms: the row states a Gap instead (#1999).
    rendered(evidence, owner, constant, failed)
        .filter(|s| crate::workbench::neutrality::projected_code_token(s).is_none())
        .map(|said| with_definitions(said, evidence, constant))
}

/// A statement followed by what each local name its conditions test was
/// assigned, when the adapter recorded it (AC-1034): "Here the API key is
/// the API key whose code is the “Authentication” header." A definition
/// outside the rendering rules adds nothing.
fn with_definitions(
    said: String,
    evidence: &GuardedExitEvidence,
    constant: Constants<'_>,
) -> String {
    let Some(definitions) = evidence.local_definitions.as_deref() else {
        return said;
    };
    // A value a callable returns with no guard (AC-1052) is said with
    // the locals it reads, as a tested condition's are.
    let returned = match &evidence.effect {
        LocalExit::Return { value: Some(value) } if evidence.conditions.is_empty() => Some(value),
        _ => None,
    };
    // Each definition's name, read off a use inside a stored expression.
    let spelled = |at: &core_prov::EvidenceRef| -> Option<String> {
        let within = evidence
            .conditions
            .iter()
            .map(|c| &c.expression)
            .chain(returned)
            .chain(definitions.iter().map(|d| &d.initializer))
            .find(|e| {
                e.capture == core_graph::rules::ExpressionCapture::CompleteSyntax
                    && e.source.byte_start <= at.byte_start
                    && at.byte_end <= e.source.byte_end
            })?;
        let start = usize::try_from(at.byte_start - within.source.byte_start).ok()?;
        let end = usize::try_from(at.byte_end - within.source.byte_start).ok()?;
        let name = within.display.as_str().get(start..end)?;
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            .then(|| name.to_owned())
    };
    let named: Vec<(String, &str, bool)> = definitions
        .iter()
        .filter_map(|d| {
            let name = d.uses.iter().find_map(&spelled)?;
            let tested = d.uses.iter().any(|u| {
                evidence.conditions.iter().any(|c| {
                    c.expression.source.byte_start <= u.byte_start
                        && u.byte_end <= c.expression.source.byte_end
                }) || returned.is_some_and(|r| {
                    r.source.byte_start <= u.byte_start && u.byte_end <= r.source.byte_end
                })
            });
            Some((name, d.initializer.display.as_str(), tested))
        })
        .collect();
    let cx = Context {
        class: None,
        constant,
        failed: None,
    };
    let mut out = said;
    let mut said_names = std::collections::BTreeSet::new();
    let mut said_bindings = std::collections::BTreeSet::new();
    let mut sentences = 0;
    let mut say = |out: &mut String, what: &str, is: &str| {
        let sentence = possessive(&format!(" Here {what} {} {is}.", agreeing(what)));
        if is != what
            && sentences < MAX_DEFINITION_SENTENCES
            && crate::workbench::neutrality::projected_code_token(&sentence).is_none()
        {
            out.push_str(&sentence);
            sentences += 1;
            true
        } else {
            false
        }
    };
    for (index, (name, _, tested)) in named.iter().enumerate() {
        if !tested || !said_names.insert(name.clone()) {
            continue;
        }
        // Two values of one name reach the conditions (`if x:` … `x = y` …
        // `if x:`): which one a sentence means would be a guess, so neither
        // is said.
        if named.iter().filter(|(n, _, t)| *t && n == name).count() > 1 {
            continue;
        }
        let Some(what) = noun(&Expr::Ref(vec![name.clone()]), &cx) else {
            continue;
        };
        if let Some(is) = defined_noun(&Expr::Ref(vec![name.clone()]), &named, &cx, 0) {
            say(&mut out, &what, &is);
            continue;
        }
        // `cond ? a : b` (AC-1052): "a when cond, and otherwise b".
        if let Some((_, init, _)) = named.iter().find(|(n, ..)| n == name)
            && let Some(is) = chosen_noun(init, &cx)
        {
            say(&mut out, &what, &is);
            continue;
        }
        // Outside the textual rules, the recorded expression is read node by
        // node, and each local name it reads is said in a sentence of its
        // own (AC-1040).
        let Some(definition) = definitions
            .iter()
            .filter(|d| d.uses.iter().find_map(&spelled).as_deref() == Some(name.as_str()))
            .nth(named[..index].iter().filter(|(n, ..)| n == name).count())
        else {
            continue;
        };
        let mut queue = std::collections::VecDeque::from([(name.clone(), what, definition)]);
        while let Some((spelling, what, definition)) = queue.pop_front() {
            if !said_bindings.insert(definition.binding_id.as_str())
                || (spelling != *name && !said_names.insert(spelling.clone()))
            {
                continue;
            }
            let mut later = Vec::new();
            let root = definition.expression.root as usize;
            let Some(is) = node_noun(definitions, definition, &spelling, root, &cx, &mut later, 0)
            else {
                continue;
            };
            if say(&mut out, &what, &is) {
                queue.extend(later);
            }
        }
    }
    out
}

/// A conditional value as a noun phrase (AC-1052): `variant.useGlobal ?
/// threshold : variant.threshold` is "the threshold when the variant's use
/// global is set, and otherwise the variant's threshold". A nested
/// conditional, or a part outside the rendering rules, says nothing.
fn chosen_noun(init: &str, cx: &Context<'_>) -> Option<String> {
    let text = one_line(init);
    let bytes = text.as_bytes();
    let (mut depth, mut quote, mut ask, mut colon) = (0usize, None::<u8>, None, None);
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = quote {
            if b == q && bytes.get(i.wrapping_sub(1)) != Some(&b'\\') {
                quote = None;
            }
            continue;
        }
        match b {
            b'\'' | b'"' | b'`' => quote = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'?' if depth == 0
                && bytes.get(i + 1) != Some(&b'?')
                && bytes.get(i + 1) != Some(&b'.')
                && i.checked_sub(1).and_then(|p| bytes.get(p)) != Some(&b'?') =>
            {
                if ask.is_some() {
                    return None;
                }
                ask = Some(i);
            }
            b':' if depth == 0 && ask.is_some() => {
                if colon.is_some() {
                    return None;
                }
                colon = Some(i);
            }
            _ => {}
        }
    }
    let (ask, colon) = (ask?, colon?);
    let test = condition(text[..ask].trim(), BranchPolarity::TruthyBranch, cx)?;
    let yes = noun(&parse(text[ask + 1..colon].trim())?, cx)?;
    let no = noun(&parse(text[colon + 1..].trim())?, cx)?;
    Some(format!("{yes} when {test}, and otherwise {no}"))
}

/// The most "Here … is …" sentences one statement carries.
const MAX_DEFINITION_SENTENCES: usize = 6;

/// One recorded initializer node as a noun phrase (AC-1040): a sum reads
/// "A plus B", an index "the first of X", a member "X's y", a plain
/// function's call "the “f” result for A", and a record lookup "the alias
/// whose email is …"; any other method call is not said. A local name it
/// reads is said by name and queued in `later` to be stated next, unless
/// it is the name being defined (`x = f(x)`), whose earlier value is read
/// in place.
fn node_noun<'d>(
    definitions: &'d [core_graph::rules::LocalDefinition],
    definition: &'d core_graph::rules::LocalDefinition,
    defining: &str,
    index: usize,
    cx: &Context<'_>,
    later: &mut Vec<Queued<'d>>,
    depth: usize,
) -> Option<String> {
    use core_graph::rules::{BinaryOperator, DefinitionExpressionKind as K, DependencyResolution};
    if depth > 6 {
        return None;
    }
    let nodes = &definition.expression.nodes;
    let node = nodes.get(index)?;
    let display =
        |i: u32| -> Option<&str> { Some(nodes.get(i as usize)?.expression.display.as_str()) };
    let plain = |text: &str| -> bool {
        !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    // The recorded local an identifier node reads, with its spelling.
    let local = |i: u32| -> Option<(&'d str, &'d core_graph::rules::LocalDefinition)> {
        let n = nodes.get(i as usize)?;
        let K::Identifier { dependency } = n.kind else {
            return None;
        };
        let DependencyResolution::Binding { binding_id, .. } =
            &definition.dependencies.get(dependency as usize)?.resolution
        else {
            return None;
        };
        let inner = definitions.iter().find(|d| &d.binding_id == binding_id)?;
        Some((n.expression.display.as_str(), inner))
    };
    let name = |i: u32, later: &mut Vec<Queued<'d>>| -> Option<String> {
        let n = nodes.get(i as usize)?;
        if !matches!(n.kind, K::Identifier { .. }) {
            return node_noun(
                definitions,
                definition,
                defining,
                i as usize,
                cx,
                later,
                depth + 1,
            );
        }
        let text = n.expression.display.as_str();
        if !plain(text) {
            return None;
        }
        let what = noun(&Expr::Ref(vec![text.to_owned()]), cx)?;
        let Some((_, inner)) = local(i) else {
            return Some(what);
        };
        if let Some(read) = request_read(inner.initializer.display.as_str()) {
            return Some(read);
        }
        if text == defining {
            let root = inner.expression.root as usize;
            return node_noun(definitions, inner, text, root, cx, later, depth + 1);
        }
        later.push((text.to_owned(), what.clone(), inner));
        Some(what)
    };
    match &node.kind {
        K::Identifier { .. } => name(index as u32, later),
        K::Literal => noun(&parse(node.expression.display.as_str())?, cx),
        K::Parenthesized { value } | K::Await { value } => node_noun(
            definitions,
            definition,
            defining,
            *value as usize,
            cx,
            later,
            depth + 1,
        ),
        K::Binary {
            operator: BinaryOperator::Add,
            left,
            right,
        } => Some(format!(
            "{} plus {}",
            name(*left, later)?,
            name(*right, later)?
        )),
        K::Member {
            object,
            property,
            computed: false,
            ..
        } => {
            let property = display(*property)?;
            if !plain(property) {
                return None;
            }
            let object_node = nodes.get(*object as usize)?;
            // `form.code.data`: the “code” form field.
            if property == "data"
                && let K::Member {
                    object: form,
                    property: field,
                    computed: false,
                    ..
                } = object_node.kind
                && let (Some(form), Some(field)) = (display(form), display(field))
                && let Some(read) = field_read(
                    &[form.to_owned(), field.to_owned(), "data".to_owned()],
                    &|_| false,
                )
            {
                return Some(read);
            }
            if let Some((text, inner)) = local(*object)
                && json_body(inner.initializer.display.as_str())
            {
                return field_read(&[text.to_owned(), property.to_owned()], &|_| true);
            }
            if matches!(object_node.kind, K::Identifier { .. }) && local(*object).is_none() {
                let text = object_node.expression.display.as_str();
                return noun(&Expr::Ref(vec![text.to_owned(), property.to_owned()]), cx);
            }
            Some(format!(
                "the {} of {}",
                words(property),
                name(*object, later)?
            ))
        }
        K::Member {
            object,
            property,
            computed: true,
            ..
        } => {
            let ordinal = match display(*property)? {
                "0" => "first",
                "1" => "second",
                "2" => "third",
                _ => return None,
            };
            Some(format!("the {ordinal} of {}", name(*object, later)?))
        }
        K::Call {
            callee, arguments, ..
        } => {
            let callee_node = nodes.get(*callee as usize)?;
            let called = match &callee_node.kind {
                // `len(x)` counts; `int(x)` and the like read x as it is.
                K::Identifier { .. }
                    if matches!(
                        callee_node.expression.display.as_str(),
                        "len" | "int" | "str" | "float"
                    ) =>
                {
                    let [one] = arguments[..] else {
                        return None;
                    };
                    let value = name(one, later)?;
                    if callee_node.expression.display.as_str() != "len" {
                        return Some(value);
                    }
                    return Some(format!(
                        "the number of {}",
                        value.strip_prefix("the ").unwrap_or(&value)
                    ));
                }
                K::Identifier { .. } => callee_node.expression.display.as_str(),
                K::Member {
                    object,
                    property,
                    computed: false,
                    ..
                } => {
                    let model = display(*object)?;
                    let method = display(*property)?;
                    // `Alias.get_by(email=x)`: the alias whose email is x.
                    if is_class(model) && plain(model) && matches!(method, "get_by" | "first_by") {
                        let mut parts = Vec::new();
                        for argument in arguments {
                            let K::KeywordArgument { name: key, value } =
                                nodes.get(*argument as usize)?.kind
                            else {
                                return None;
                            };
                            let key = display(key)?;
                            if !plain(key) {
                                return None;
                            }
                            parts.push(format!("{} is {}", words(key), name(value, later)?));
                        }
                        if parts.is_empty() {
                            return None;
                        }
                        return Some(format!(
                            "the {} whose {}",
                            words(model),
                            parts.join(" and ")
                        ));
                    }
                    // `await qb.getMany()`: the records a TypeORM query
                    // builder selects (AC-1046).
                    if let Some(query) =
                        query_noun(definitions, definition, *object, method, cx, depth + 1)
                    {
                        return Some(query);
                    }
                    // Any other method's result (`cache.get(k)`) is not said.
                    return None;
                }
                _ => return None,
            };
            if !plain(called) || predicate_verb(called).is_some() {
                return None;
            }
            let called = format!("the “{}” result", words(called.trim_start_matches('_')));
            match &arguments[..] {
                [] => Some(called),
                [one] if !matches!(nodes.get(*one as usize)?.kind, K::KeywordArgument { .. }) => {
                    Some(format!("{called} for {}", name(*one, later)?))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The records a TypeORM query builder selects, read off its recorded call
/// chain (AC-1046): `getRepository(ctx, ProductVariant)
/// .createQueryBuilder('variant').leftJoin('variant.product', 'product')
/// .where('variant.deletedAt IS NULL').andWhere('product.deletedAt IS NULL')
/// .andWhere('variant.id IN (:...variantIds)', { variantIds }).getMany()` is
/// "the product variants that are not deleted and whose product is not
/// deleted, among the variant ids". The builder may be a local name
/// recorded in the same rule. Any step or condition outside these shapes
/// says nothing.
fn query_noun(
    definitions: &[core_graph::rules::LocalDefinition],
    definition: &core_graph::rules::LocalDefinition,
    receiver: u32,
    terminal: &str,
    cx: &Context<'_>,
    depth: usize,
) -> Option<String> {
    use core_graph::rules::{
        DefinitionExpressionKind as K, DependencyResolution, KnownLiteral, LiteralEvidence,
    };
    let count = match terminal {
        "getMany" => false,
        "getCount" => true,
        _ => return None,
    };
    // Steps from the terminal back to the repository, as (method, string
    // arguments).
    let mut steps: Vec<(String, Vec<String>)> = Vec::new();
    let mut definition = definition;
    let mut at = receiver;
    let mut hops = 0;
    let entity = loop {
        hops += 1;
        if hops > 16 || depth > 6 {
            return None;
        }
        let node = definition.expression.nodes.get(at as usize)?;
        match &node.kind {
            // A builder held in a local name: follow its recorded initializer.
            K::Identifier { dependency } => {
                let DependencyResolution::Binding { binding_id, .. } = &definition
                    .dependencies
                    .get(*dependency as usize)?
                    .resolution
                else {
                    return None;
                };
                definition = definitions.iter().find(|d| &d.binding_id == binding_id)?;
                at = definition.expression.root;
            }
            K::Await { value } | K::Parenthesized { value } => at = *value,
            K::Call {
                callee, arguments, ..
            } => {
                let nodes = &definition.expression.nodes;
                let K::Member {
                    object,
                    property,
                    computed: false,
                    optional: false,
                    ..
                } = nodes.get(*callee as usize)?.kind
                else {
                    return None;
                };
                let method = nodes.get(property as usize)?.expression.display.as_str();
                if method == "getRepository" {
                    // `getRepository(ctx, Entity)` or `getRepository(Entity)`.
                    let last = nodes.get(*arguments.last()? as usize)?;
                    let name = last.expression.display.as_str();
                    if !matches!(last.kind, K::Identifier { .. })
                        || !is_class(name)
                        || !name.chars().all(|c| c.is_ascii_alphanumeric())
                        || arguments.len() > 2
                    {
                        return None;
                    }
                    break name.to_owned();
                }
                let texts: Vec<String> = arguments
                    .iter()
                    .filter_map(|a| match &nodes.get(*a as usize)?.expression.literal {
                        Some(LiteralEvidence::Known {
                            value: KnownLiteral::String(text),
                        }) => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
                steps.push((method.to_owned(), texts));
                at = object;
            }
            _ => return None,
        }
    };
    steps.reverse();
    let mut alias: Option<String> = None;
    let mut joins: Vec<(String, String)> = Vec::new();
    let mut own: Vec<String> = Vec::new();
    let mut joined: Vec<String> = Vec::new();
    let mut among: Vec<String> = Vec::new();
    let mut predicates = 0;
    for (method, texts) in &steps {
        match (method.as_str(), &texts[..]) {
            ("createQueryBuilder", [name]) if alias.is_none() && plain_word(name) => {
                alias = Some(name.clone());
            }
            // An inner join also drops the records without the relation,
            // which this reading does not state, so only left joins are read.
            ("leftJoin" | "leftJoinAndSelect", [path, name]) => {
                let (from, relation) = path.split_once('.')?;
                if Some(from) != alias.as_deref() || !plain_word(relation) || !plain_word(name) {
                    return None;
                }
                joins.push((name.clone(), relation.to_owned()));
            }
            // A later `where` replaces every earlier condition, so only a
            // first `where` followed by `andWhere`s is read.
            ("where" | "andWhere", [condition, ..]) => {
                if (method == "where") != (predicates == 0) {
                    return None;
                }
                predicates += 1;
                let (target, column, test) = query_condition(condition)?;
                if Some(target.as_str()) == alias.as_deref() {
                    match test {
                        QueryTest::Deleted(false) => own.push("that are not deleted".to_owned()),
                        QueryTest::Deleted(true) => own.push("that are deleted".to_owned()),
                        QueryTest::Unset => own.push(format!("with no {}", words(&column))),
                        QueryTest::Set => own.push(format!("with a {}", words(&column))),
                        QueryTest::Among(param) if column == "id" => {
                            among.push(noun(&Expr::Ref(vec![param]), cx)?);
                        }
                        QueryTest::Among(param) => own.push(format!(
                            "whose {} is one of {}",
                            words(&column),
                            noun(&Expr::Ref(vec![param]), cx)?
                        )),
                        QueryTest::Equals(param) => own.push(format!(
                            "whose {} is {}",
                            words(&column),
                            noun(&Expr::Ref(vec![param]), cx)?
                        )),
                    }
                } else {
                    let (_, relation) = joins.iter().find(|(name, _)| *name == target)?;
                    let relation = words(relation);
                    joined.push(match test {
                        QueryTest::Deleted(false) => format!("whose {relation} is not deleted"),
                        QueryTest::Deleted(true) => format!("whose {relation} is deleted"),
                        QueryTest::Equals(param) => format!(
                            "whose {relation}'s {} is {}",
                            words(&column),
                            noun(&Expr::Ref(vec![param]), cx)?
                        ),
                        _ => return None,
                    });
                }
            }
            _ => return None,
        }
    }
    alias.as_ref()?;
    let entity = words(&entity);
    let plural = if entity.ends_with('s') || entity.ends_with('x') || entity.ends_with("ch") {
        format!("{entity}es")
    } else if let Some(stem) = entity.strip_suffix('y')
        && !stem.ends_with(['a', 'e', 'o', 'u'])
    {
        format!("{stem}ies")
    } else {
        format!("{entity}s")
    };
    let mut said = format!("the {plural}");
    own.extend(joined);
    if !own.is_empty() {
        said = format!("{said} {}", own.join(" and "));
    }
    for set in among {
        said = format!("{said}, among {set}");
    }
    Some(if count {
        format!(
            "the number of {}",
            said.strip_prefix("the ").unwrap_or(&said)
        )
    } else {
        said
    })
}

/// A query-builder alias, relation or column: letters, digits and `_`.
fn plain_word(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One query-builder condition's test (AC-1046).
enum QueryTest {
    /// `x.deletedAt IS NULL` (`false`) or `IS NOT NULL` (`true`).
    Deleted(bool),
    /// `x.col IS NULL`.
    Unset,
    /// `x.col IS NOT NULL`.
    Set,
    /// `x.col IN (:...param)`.
    Among(String),
    /// `x.col = :param`.
    Equals(String),
}

/// A single-predicate query-builder condition as (alias, column, test);
/// `None` for any other shape, including `AND`/`OR` within one string.
fn query_condition(condition: &str) -> Option<(String, String, QueryTest)> {
    let tokens: Vec<&str> = condition.split_whitespace().collect();
    let (left, rest) = tokens.split_first()?;
    let (alias, column) = left.split_once('.')?;
    if !plain_word(alias) || !plain_word(column) {
        return None;
    }
    let deleted = column == "deletedAt";
    let test = match rest {
        ["IS", "NULL"] if deleted => QueryTest::Deleted(false),
        ["IS", "NOT", "NULL"] if deleted => QueryTest::Deleted(true),
        ["IS", "NULL"] => QueryTest::Unset,
        ["IS", "NOT", "NULL"] => QueryTest::Set,
        ["IN", list] => {
            let param = list.strip_prefix("(:...")?.strip_suffix(')')?;
            if !plain_word(param) {
                return None;
            }
            QueryTest::Among(param.to_owned())
        }
        ["=", param] => {
            let param = param.strip_prefix(':')?;
            if !plain_word(param) {
                return None;
            }
            QueryTest::Equals(param.to_owned())
        }
        _ => return None,
    };
    Some((alias.to_owned(), column.to_owned(), test))
}

/// A local name queued to be stated: its spelling, its noun and its
/// definition.
type Queued<'d> = (String, String, &'d core_graph::rules::LocalDefinition);

/// Whether an initializer is the request's JSON body.
fn json_body(initializer: &str) -> bool {
    let initializer = initializer.trim();
    initializer.starts_with("request.get_json(") || initializer == "request.json"
}

/// A form field's or JSON body field's value by its name (AC-1040):
/// `form.code.data` is the “code” form field, and `data.get("name")`, with
/// `data` the request's JSON body, the “name” field of the JSON body.
fn field_read(path: &[String], body: &dyn Fn(&str) -> bool) -> Option<String> {
    let names: Vec<&str> = path.iter().map(String::as_str).collect();
    match names[..] {
        [.., form, field, "data"] if form.ends_with("form") => {
            Some(format!("the “{field}” form field"))
        }
        [local, key] if body(local) => Some(format!("the “{key}” field of the JSON body")),
        _ => None,
    }
}

/// "is" or "are" for a noun phrase: "the suffixes are", "the alias is".
fn agreeing(what: &str) -> &'static str {
    let last = what.rsplit(' ').next().unwrap_or(what);
    let plural = last.len() > 2
        && last.ends_with('s')
        && !["ss", "us", "is", "as", "os"]
            .iter()
            .any(|e| last.ends_with(e));
    if plural { "are" } else { "is" }
}

/// A plural possessive written as such: "the invalid domains' intersection".
fn possessive(text: &str) -> String {
    text.split(' ')
        .map(|word| match word.strip_suffix("s's") {
            Some(stem) if agreeing(&format!("{stem}s")) == "are" => format!("{stem}s'"),
            _ => word.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A request input read by its literal name: `request.headers.get("X")` is
/// the “X” header, `request.args.get("x")` the “x” query parameter and
/// `request.form.get("x")` the “x” form field (AC-1034).
fn request_read(initializer: &str) -> Option<String> {
    for (prefix, what) in [
        ("request.headers.get(", "header"),
        ("request.args.get(", "query parameter"),
        ("request.form.get(", "form field"),
    ] {
        let Some(rest) = initializer.trim().strip_prefix(prefix) else {
            continue;
        };
        let rest = rest.strip_suffix(')')?.trim();
        let key = rest.split(',').next()?.trim();
        let key = key
            .strip_prefix('"')
            .and_then(|k| k.strip_suffix('"'))
            .or_else(|| key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))?;
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }
        return Some(format!("the “{key}” {what}"));
    }
    None
}

/// A value as a noun phrase, with each defined local name read as what it
/// was assigned (AC-1034).
fn defined_noun(
    expr: &Expr,
    named: &[(String, &str, bool)],
    cx: &Context<'_>,
    depth: usize,
) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match expr {
        Expr::Ref(path) if path.len() == 1 => {
            match named.iter().find(|(name, ..)| *name == path[0]) {
                Some((_, initializer, _)) => {
                    if let Some(read) = request_read(initializer) {
                        return Some(read);
                    }
                    // `Alias.get(alias_id)` looks a record up by its key; a
                    // `.get(name)` of any other receiver reads an entry the
                    // parser would take for a member, so it is not said.
                    if let Some((receiver, rest)) = initializer.trim().split_once(".get(") {
                        let key = rest.strip_suffix(')')?.trim();
                        if is_class(receiver) && receiver.chars().all(|c| c.is_ascii_alphanumeric())
                        {
                            let key = defined_noun(&parse(key)?, named, cx, depth + 1)?;
                            return Some(format!("the {} found by {key}", words(receiver)));
                        }
                        if !key.starts_with(['"', '\'']) {
                            return None;
                        }
                    }
                    defined_noun(&parse(initializer)?, named, cx, depth + 1)
                }
                None => noun(expr, cx),
            }
        }
        Expr::Ref(path) => {
            let body = |local: &str| {
                named
                    .iter()
                    .any(|(name, initializer, _)| name == local && json_body(initializer))
            };
            field_read(path, &body).or_else(|| noun(expr, cx))
        }
        // `len(x)` counts; `int(x)`, `str(x)` and `float(x)` are x read as
        // a number or text (AC-1040).
        Expr::Apply(path, args)
            if path.len() == 1
                && matches!(path[0].as_str(), "len" | "int" | "str" | "float")
                && matches!(args[..], [Arg::Pos(_)]) =>
        {
            let [Arg::Pos(value)] = &args[..] else {
                return None;
            };
            let value = defined_noun(value, named, cx, depth + 1)?;
            Some(if path[0] == "len" {
                format!(
                    "the number of {}",
                    value.strip_prefix("the ").unwrap_or(&value)
                )
            } else {
                value
            })
        }
        // A plain function's result: `authorize_request()`.
        Expr::Call(path) if path.len() == 1 && predicate_verb(&path[0]).is_none() => Some(format!(
            "the “{}” result",
            words(path[0].trim_start_matches('_'))
        )),
        // A method's result with no arguments (`parser.parse_args()`) reads
        // as code unless it names a value such as the current time.
        Expr::Call(path) if path.len() > 1 => noun(expr, cx).filter(|n| !n.contains("'s ")),
        Expr::Apply(path, args) => {
            let names: Vec<&str> = path.iter().map(String::as_str).collect();
            match (&names[..], &args[..]) {
                // `request.headers.get("Authentication")`.
                (["request", "headers", "get"], [Arg::Pos(Expr::Lit(Lit::Str(key))), ..]) => {
                    Some(format!("the “{key}” header"))
                }
                (["request", "args", "get"], [Arg::Pos(Expr::Lit(Lit::Str(key))), ..]) => {
                    Some(format!("the “{key}” query parameter"))
                }
                (["request", "form", "get"], [Arg::Pos(Expr::Lit(Lit::Str(key))), ..]) => {
                    Some(format!("the “{key}” form field"))
                }
                // `ApiKey.get_by(code=api_code)`: the API key whose code is …
                ([model, "get_by" | "first_by"], _) if is_class(model) && !args.is_empty() => {
                    let parts: Option<Vec<String>> = args
                        .iter()
                        .map(|a| match a {
                            Arg::Kw(key, value) => Some(format!(
                                "{} is {}",
                                words(key),
                                defined_noun(value, named, cx, depth + 1)?
                            )),
                            Arg::Pos(_) => None,
                        })
                        .collect();
                    Some(format!(
                        "the {} whose {}",
                        words(model),
                        parts?.join(" and ")
                    ))
                }
                // `email.split("@")`: the email split at “@” (AC-1040).
                ([.., "split"], [Arg::Pos(Expr::Lit(Lit::Str(at)))]) if names.len() > 1 => {
                    let whole = noun(&Expr::Ref(path[..path.len() - 1].to_vec()), cx)?;
                    Some(format!("{whole} split at “{at}”"))
                }
                // Any other method's result (`parser.parse_args()`) reads as
                // code, not as a value, so it is not said (AC-1040).
                ([_, _, ..], _) => None,
                _ => noun(expr, cx),
            }
        }
        _ => noun(expr, cx),
    }
}

fn rendered(
    evidence: &GuardedExitEvidence,
    owner: &str,
    constant: Constants<'_>,
    failed: &[Option<String>],
) -> Option<String> {
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
    let cx = Context {
        class,
        constant,
        failed: None,
    };
    if evidence.conditions.is_empty()
        && let LocalExit::Return { value: Some(value) } = &evidence.effect
        && let Some(said) = ts_choice(evidence, value.display.as_str(), class, method)
    {
        return Some(said);
    }
    let implied = implied_by_pin(
        &evidence
            .conditions
            .iter()
            .map(|c| (c.expression.display.as_str(), c.polarity))
            .collect::<Vec<_>>(),
    );
    let conditions: Option<Vec<String>> = evidence
        .conditions
        .iter()
        .enumerate()
        .filter(|(i, _)| !implied[*i])
        .map(|(i, c)| {
            let cx = Context {
                class,
                constant,
                failed: failed.get(i).and_then(Option::as_deref),
            };
            condition(&tested_text(evidence, c), c.polarity, &cx)
        })
        .collect();
    let conditions = said_once(conditions?);
    let when = if conditions.is_empty() {
        "always".to_owned()
    } else {
        conditions.join(", and ")
    };
    let sentence = |outcome: String| {
        if conditions.is_empty() {
            format!("{}{}.", outcome[..1].to_uppercase(), &outcome[1..])
        } else {
            format!("When {when}, {outcome}.")
        }
    };
    let refused = |status: u16, message: Option<String>| {
        let tail = message.map(|m| format!(" (“{m}”)")).unwrap_or_default();
        if (400..600).contains(&status) {
            format!("the request is refused with status {status}{tail}")
        } else {
            format!("it responds with status {status}{tail}")
        }
    };
    // A Go response written before a bare return (#2009).
    if let LocalExit::Call { value } = &evidence.effect
        && is_go(evidence)
        && let Some((status, message)) = go_response(value.display.as_str())
    {
        return Some(sentence(refused(status, message)));
    }
    // A Go error passed back (#2009).
    if let LocalExit::Return { value: Some(value) } = &evidence.effect
        && is_go(evidence)
        && let Some(message) = go_error(value.display.as_str())
    {
        return Some(sentence(if message.is_empty() {
            "it fails with an error".to_owned()
        } else {
            format!("it fails with the error “{message}”")
        }));
    }
    // A Rust status or error returned (#2018).
    if let LocalExit::Return { value: Some(value) } = &evidence.effect
        && is_rust(evidence)
    {
        if let Some((status, message)) = rust_status(value.display.as_str()) {
            return Some(sentence(refused(status, message)));
        }
        if let Some(error) = rust_error(value.display.as_str()) {
            return Some(sentence(format!("it fails with {error}")));
        }
    }
    // A state change or a call (#1998, AC-0910).
    let changed = match &evidence.effect {
        LocalExit::Assign { target, value }
            if evidence.exit_source.path.ends_with(".py")
                && let Some(removed) =
                    line_breaks_removed(target.display.as_str(), value.display.as_str(), &cx) =>
        {
            Some(removed)
        }
        LocalExit::Assign { target, value } => Some(assigned(
            target.display.as_str(),
            value.display.as_str(),
            &cx,
        )?),
        LocalExit::Call { value } => match pushed(value.display.as_str(), &cx) {
            Some(added) => Some(added),
            None => Some(called(value.display.as_str())?),
        },
        LocalExit::Return { .. } | LocalExit::Throw { .. } => None,
    };
    if let Some(outcome) = changed {
        return Some(sentence(outcome));
    }
    let value = match &evidence.effect {
        LocalExit::Assign { .. } | LocalExit::Call { .. } => return None,
        // A throw in a transition guard refuses the move (AC-1032).
        LocalExit::Throw { .. } if method == "onTransitionStart" => {
            return Some(sentence("the move is refused with an error".to_owned()));
        }
        LocalExit::Throw { value } => {
            // A Java or Kotlin exception names the error it raises (#2020,
            // #2021).
            let error = if is_java(evidence) {
                java_thrown(value.display.as_str())
            } else if is_kotlin(evidence) {
                kotlin_thrown(value.display.as_str())
            } else {
                None
            }
            .unwrap_or_else(|| "an error".to_owned());
            return Some(sentence(format!("it fails with {error}")));
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
    // A state machine's `onTransitionStart` refuses a move by what it
    // returns (AC-1032): a message, `false`, or a message the export
    // withholds.
    if method == "onTransitionStart" {
        let refusal = match value.as_str() {
            "false" => Some("the move is refused".to_owned()),
            "[REDACTED]" => {
                Some("the move is refused with a message the export withholds".to_owned())
            }
            // `true` lets the move happen; any other value refuses it.
            "true" => Some("the move is allowed".to_owned()),
            other => match parse(other) {
                Some(Expr::Lit(Lit::Str(message))) => Some(format!(
                    "the move is refused with the message “{}”",
                    message_words(&message)
                )),
                // `ctx.translate('message.x', {...})`: the message it names.
                Some(Expr::Apply(path, args))
                    if path.last().is_some_and(|m| m == "translate")
                        && matches!(args.first(), Some(Arg::Pos(Expr::Lit(Lit::Str(_))))) =>
                {
                    let Some(Arg::Pos(Expr::Lit(Lit::Str(message)))) = args.first() else {
                        return None;
                    };
                    Some(format!(
                        "the move is refused with the message “{}”",
                        message_words(message)
                    ))
                }
                _ => Some("the move is refused".to_owned()),
            },
        };
        if let Some(refusal) = refusal {
            return Some(sentence(refusal));
        }
    }
    if let Some((status, message)) = response(&value) {
        return Some(sentence(refused(status, message)));
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
    // A predicate answering with a condition (#2029): `return
    // self.delete_on is None` in `User.is_active` reads "A user is active
    // when the user's delete on is not set".
    if answer.is_none()
        && let Some((verb, rest)) = predicate
        && let Some(test) = parse(&value).and_then(|e| render(&e, false, &cx))
    {
        let who = match class {
            Some(class) => format!("a {}", words(class)),
            None => "the caller".to_owned(),
        };
        let s = if conditions.is_empty() {
            format!("{who} {verb} {} when {test}.", words(rest))
        } else {
            format!("{who} {verb} {} when {when}, and {test}.", words(rest))
        };
        return Some(format!("{}{}", s[..1].to_uppercase(), &s[1..]));
    }
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
        // An accessor reads as what it gives: `get_active_subscription`
        // is "active subscription".
        let named = method.trim_start_matches('_');
        let named = named.strip_prefix("get_").unwrap_or(named);
        // `getSaleableStockLevel` is "saleable stock level" (AC-1052).
        let named = named
            .strip_prefix("get")
            .filter(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
            .unwrap_or(named);
        // A service's value is its entity's: `ProductVariantService` reads
        // "the product variant's".
        let class = class.map(|c| {
            c.strip_suffix("Service")
                .filter(|entity| !entity.is_empty())
                .unwrap_or(c)
        });
        let what = match class {
            Some(class) => format!("the {}'s {}", words(class), words(named)),
            None => format!("the {}", words(named)),
        };
        // A returned difference (AC-1052): `return stockOnHand -
        // stockAllocated` is "the stock on hand minus the stock allocated".
        // A compared one stays outside the rules.
        let given = match &parsed {
            Expr::Difference(terms) => terms
                .iter()
                .map(|t| noun(t, &cx))
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.join(" minus ")),
            other => noun(other, &cx),
        };
        let Some(given) = given else {
            // A test the method answers with (AC-1030): `return
            // self.get_active_subscription() is not None` after `if
            // self.lifetime: return True` reads "Otherwise, the user's
            // lifetime or active subscription is true when the user's
            // active subscription is set".
            let test = render(&parsed, false, &cx)?;
            let s = if conditions.is_empty() {
                format!("{what} is true when {test}.")
            } else {
                format!("{what} is true when {when}, and {test}.")
            };
            let s = format!("{}{}", s[..1].to_uppercase(), &s[1..]);
            return Some(if conditions.is_empty() && evidence.source_order > 0 {
                format!("Otherwise, {}{}", s[..1].to_lowercase(), &s[1..])
            } else {
                s
            });
        };
        // The last answer after earlier exits holds otherwise (AC-1030).
        if conditions.is_empty() && evidence.source_order > 0 {
            return Some(format!("Otherwise, {what} is {given}."));
        }
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

/// A TypeScript answer stating how a value is chosen (AC-0956):
/// `rate || this.defaultTaxRate`, where `rate` is a `.find(r => …)`, reads
/// "It picks the first of the active tax rates whose test with the zone and
/// tax category holds; when none does, the default tax rate", and a
/// boolean answer of comparisons (`idsAreEqual(a, this.b) && x === y`)
/// reads "A tax rate's test holds when the a equals the b, and …".
fn ts_choice(
    evidence: &GuardedExitEvidence,
    value: &str,
    class: Option<&str>,
    method: &str,
) -> Option<String> {
    let path = evidence.exit_source.path.as_str();
    if ![".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"]
        .iter()
        .any(|ext| path.ends_with(ext))
    {
        return None;
    }
    let value = value.trim();
    // A pick with a fallback.
    for operator in [" || ", " ?? "] {
        if let Some((found, fallback)) = value.split_once(operator) {
            let found = found.trim();
            let initializer = if found.contains(".find(") {
                Some(found.to_owned())
            } else {
                evidence
                    .local_definitions
                    .iter()
                    .flatten()
                    .filter(|_| found.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                    .find(|d| {
                        let exit = &evidence.exit_source;
                        d.initializer.display.as_str().contains(".find(")
                            && d.uses.iter().any(|u| {
                                u.byte_start >= exit.byte_start
                                    && u.byte_end <= exit.byte_end
                                    && u.byte_end - u.byte_start == found.len() as u64
                            })
                    })
                    .map(|d| d.initializer.display.as_str().to_owned())
            }?;
            let (collection, predicate) = initializer.rsplit_once(".find(")?;
            let predicate = predicate.strip_suffix(')')?;
            let (parameter, body) = predicate.split_once("=>")?;
            let parameter = parameter.trim().trim_matches(['(', ')']).trim();
            let what = ts_noun(collection)?;
            let test = ts_member_test(body.trim(), parameter)?;
            let otherwise = ts_noun(fallback)?;
            return Some(format!(
                "It picks the first of {what} {test}; when none does, {otherwise}."
            ));
        }
    }
    // A boolean answer of comparisons.
    let clauses: Option<Vec<String>> = value.split(" && ").map(ts_equality).collect();
    let clauses = clauses?;
    let who = match class {
        Some(class) => format!("a {}'s", words(class)),
        None => "the".to_owned(),
    };
    let s = format!(
        "{who} {} holds when {}.",
        words(method),
        clauses.join(", and ")
    );
    Some(format!("{}{}", s[..1].to_uppercase(), &s[1..]))
}

/// `(await this.getActiveTaxRates(ctx))` → "the active tax rates";
/// `this.defaultTaxRate` → "the default tax rate"; `channel.defaultTaxZone`
/// → "the channel's default tax zone".
fn ts_noun(text: &str) -> Option<String> {
    let mut text = text.trim();
    loop {
        let inner = text
            .strip_prefix('(')
            .and_then(|t| t.strip_suffix(')'))
            .map(str::trim)
            .or_else(|| text.strip_prefix("await ").map(str::trim));
        match inner {
            Some(inner) => text = inner,
            None => break,
        }
    }
    let path = match text.split_once('(') {
        Some((callee, args)) if !args.contains('(') && args.ends_with(')') => callee,
        Some(_) => return None,
        None => text,
    };
    let segments: Vec<&str> = path.split('.').collect();
    if !segments
        .iter()
        .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        return None;
    }
    let last = segments.last()?;
    let last = last
        .strip_prefix("get")
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
        .unwrap_or(last);
    Some(match &segments[..segments.len() - 1] {
        [] | ["this"] => format!("the {}", words(last)),
        [.., owner] => format!("the {}'s {}", words(owner), words(last)),
    })
}

/// `r.test(zone, taxCategory)` with `r` → "whose test with the zone and tax
/// category holds"; `r.enabled` → "whose enabled is set".
fn ts_member_test(body: &str, parameter: &str) -> Option<String> {
    let rest = body.strip_prefix(parameter)?.strip_prefix('.')?;
    let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if let Some((name, args)) = rest.split_once('(') {
        let args = args.strip_suffix(')')?;
        let args: Vec<&str> = args.split(',').map(str::trim).collect();
        if !ident(name) || !args.iter().all(|a| ident(a)) {
            return None;
        }
        let with = args
            .iter()
            .map(|a| format!("the {}", words(a)))
            .collect::<Vec<_>>()
            .join(" and ");
        return Some(format!("whose {} with {with} holds", words(name)));
    }
    ident(rest).then(|| format!("whose {} is set", words(rest)))
}

/// `idsAreEqual(a, this.b)`, `a === b`, `a == b` → "the a equals the b".
fn ts_equality(clause: &str) -> Option<String> {
    let clause = clause.trim();
    let (left, right) = if let Some(args) = clause
        .strip_prefix("idsAreEqual(")
        .and_then(|a| a.strip_suffix(')'))
    {
        args.split_once(',')?
    } else if let Some(pair) = clause.split_once(" === ") {
        pair
    } else {
        clause.split_once(" == ")?
    };
    Some(format!("{} equals {}", ts_noun(left)?, ts_noun(right)?))
}

/// A dotted lowercase message key read as its words (AC-1044):
/// `message.cannot-transition-to-payment-when-order-is-empty` is “cannot
/// transition to payment when order is empty”. Any other message is kept as
/// written.
fn message_words(message: &str) -> String {
    let key = message.contains('.')
        && message
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'));
    match message.rsplit('.').next() {
        Some(last) if key && !last.is_empty() => last.replace('-', " "),
        _ => message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(display: &str, polarity: BranchPolarity, class: Option<&str>) -> Option<String> {
        condition(
            display,
            polarity,
            &Context {
                class,
                constant: None,
                failed: None,
            },
        )
    }

    // AC-0917: a header read through a module constant is named by the
    // header the constant holds; a condition already said is not repeated.
    #[test]
    fn a_header_constant_is_named_and_a_repeat_is_said_once() {
        let lookup = |name: &str| {
            (name == "HEADER_ALLOW_API_COOKIES").then(|| "X-Sl-Allowcookies".to_owned())
        };
        let cx = Context {
            class: None,
            constant: Some(&lookup),
            failed: None,
        };
        let t = BranchPolarity::TruthyBranch;
        assert_eq!(
            condition(
                "current_user.is_authenticated and request.headers.get(constants.HEADER_ALLOW_API_COOKIES)",
                t,
                &cx,
            )
            .as_deref(),
            Some("the user is authenticated and the “X-Sl-Allowcookies” header is set"),
        );
        // Unknown to the lookup: the constant's words, as before.
        assert_eq!(
            condition("request.headers.get(OTHER_HEADER)", t, &cx).as_deref(),
            Some("the other header is set"),
        );
        assert_eq!(
            said_once(vec![
                "no API key is given".into(),
                "the user is authenticated".into(),
                "the user is authenticated and the “X-Sl-Allowcookies” header is set".into(),
                "no API key is given".into(),
            ]),
            vec![
                "no API key is given".to_owned(),
                "the user is authenticated".to_owned(),
                "the “X-Sl-Allowcookies” header is set".to_owned(),
            ],
        );
    }

    // AC-1004 (#2083): in an `elif` chain the value the branch takes is said
    // alone: a test that a value differs from a literal is dropped when the
    // same source value is pinned to another literal; one on another source
    // that reads alike, or with no pin, is kept.
    #[test]
    fn a_value_pinned_by_its_branch_is_not_also_said_to_differ() {
        let (t, f) = (BranchPolarity::TruthyBranch, BranchPolarity::FalsyBranch);
        let form = |value: &str| format!("request.form.get(\"form-name\") == \"{value}\"");
        let (cancel, monthly, yearly) = (
            form("cancel"),
            form("change-monthly"),
            form("change-yearly"),
        );
        assert_eq!(
            implied_by_pin(&[
                ("request.method == \"POST\"", t),
                (cancel.as_str(), f),
                (monthly.as_str(), f),
                (yearly.as_str(), t),
                ("success", t),
            ]),
            vec![false, true, true, false, false]
        );
        // An elif chain: the first two tests say nothing once `not alias`.
        assert_eq!(
            implied_by_pin(&[
                ("alias and not alias.user_id == user.id", f),
                ("alias and alias.user_id == user.id", f),
                ("not alias", t),
                ("hostname and user.include", t),
            ]),
            vec![true, true, false, false]
        );
        assert_eq!(
            implied_by_pin(&[("alias and alias.enabled", f), ("not other", t)]),
            vec![false, false]
        );
        assert_eq!(
            implied_by_pin(&[("count != 0", t), ("count == 3", t)]),
            vec![true, false]
        );
        // The literal may be written first.
        assert_eq!(
            implied_by_pin(&[("mode != \"cancel\"", t), ("\"yearly\" == mode", t)]),
            vec![true, false]
        );
        for kept in [
            // Another source that reads alike pins nothing here.
            vec![
                ("request.form.get(\"mode\") != \"cancel\"", t),
                ("request.args.get(\"mode\") == \"yearly\"", t),
            ],
            vec![(cancel.as_str(), f), ("plan == \"yearly\"", t)],
            vec![(cancel.as_str(), f), ("request.form.get(\"form-name\")", t)],
            vec![(cancel.as_str(), f), (form("cancel").as_str(), t)],
            // One value spelled two ways is not two values.
            vec![("count != 1_000", t), ("count == 1000", t)],
            vec![("count != 1", t), ("count == True", t)],
            // A getter's default is part of the value it reads.
            vec![
                ("data.get(\"mode\", \"cancel\") == \"cancel\"", f),
                ("data.get(\"mode\", \"yearly\") == \"yearly\"", t),
            ],
        ] {
            assert_eq!(implied_by_pin(&kept), vec![false; kept.len()], "{kept:?}");
        }
    }

    // AC-0912: membership, calls with arguments, type tests, lengths,
    // lookups joined by `or`, times and header reads are restated by rule.
    // AC-0923 (#2009): Go conditions, responses and errors in domain terms.
    #[test]
    fn go_rule_shapes_are_restated_in_domain_terms() {
        let truthy = BranchPolarity::TruthyBranch;
        for (display, class, expected) in [
            ("err != nil", Some("Service"), "an error occurred"),
            (
                "s.Store == nil",
                Some("Service"),
                "the service's store is not set",
            ),
            (
                "!s.isCatalogOwner(u)",
                Some("Service"),
                "the u is not catalog owner",
            ),
            (
                "errors.Is(err, vault.ErrLocked)",
                None,
                "the error is “locked”",
            ),
            (
                "r.Method != http.MethodPost",
                None,
                "the request's method is not “POST”",
            ),
            ("r.FormValue(\"email\") == \"\"", None, "the email is empty"),
        ] {
            assert_eq!(
                said(display, truthy, class).as_deref(),
                Some(expected),
                "{display}"
            );
        }
        assert_eq!(
            said(
                "s.Store == nil",
                BranchPolarity::FalsyBranch,
                Some("Service")
            )
            .as_deref(),
            Some("the service's store is set")
        );
        assert_eq!(
            go_response("jsonErr(w, 423, \"vault_locked\")"),
            Some((423, Some("vault locked".to_owned())))
        );
        assert_eq!(
            go_response("http.Error(w, \"forbidden\", http.StatusForbidden)"),
            Some((403, Some("forbidden".to_owned())))
        );
        assert_eq!(go_response("w.Write(body)"), None);
        assert_eq!(
            go_error("nil, store.ErrNotFound").as_deref(),
            Some("not found")
        );
        assert_eq!(
            go_error("errors.New(\"too long\")").as_deref(),
            Some("too long")
        );
        assert_eq!(
            go_error("fmt.Errorf(\"load %s: %w\", id, e)").as_deref(),
            Some("")
        );
        assert_eq!(go_error("nil, err"), None);
        // AC-0967: a Java exception names its message, else its kind.
        assert_eq!(
            java_thrown("new BadRequestAlertException(\"Invalid id\", ENTITY_NAME, \"idnull\")")
                .as_deref(),
            Some("the error “Invalid id”")
        );
        assert_eq!(
            java_thrown("new LoginAlreadyUsedException()").as_deref(),
            Some("a “login already used” error")
        );
        assert_eq!(
            java_thrown("new IllegalStateException(cause)").as_deref(),
            Some("an error")
        );
        assert_eq!(java_thrown("error"), None);
        // AC-1106: a Kotlin construction has no `new`; a lowercase call is
        // no construction.
        assert_eq!(
            kotlin_thrown("BadRequestException(\"Invalid id\")").as_deref(),
            Some("the error “Invalid id”")
        );
        assert_eq!(
            kotlin_thrown("LoginAlreadyUsedException()").as_deref(),
            Some("a “login already used” error")
        );
        assert_eq!(kotlin_thrown("failure(\"x\")"), None);
        // AC-1108: a precondition message and `error("…")` read as the error.
        assert_eq!(
            kotlin_thrown("\"too many\"").as_deref(),
            Some("the error “too many”")
        );
        assert_eq!(
            kotlin_thrown("error(\"locked\")").as_deref(),
            Some("the error “locked”")
        );
        assert_eq!(kotlin_thrown("check(order.open)"), None);
        assert!(kotlin_path("build.gradle.kts") && kotlin_path("src/A.kt"));
        assert!(!kotlin_path("src/A.java"));
        // A literal with an apostrophe or escapes is still the message.
        assert_eq!(
            java_thrown("new ValidationException(\"User's id is \\\"bad\\\"\\nnow\")").as_deref(),
            Some("the error “User's id is \"bad\" now”")
        );
        assert_eq!(
            java_thrown("new ValidationException(\"a\" + b)").as_deref(),
            Some("a “validation” error")
        );
        // The call whose error is tested names the condition.
        let failed = |display: &str, polarity, call| {
            condition(
                display,
                polarity,
                &Context {
                    class: Some("Service"),
                    constant: None,
                    failed: Some(call),
                },
            )
        };
        assert_eq!(
            failed("err != nil", truthy, "Store.Save").as_deref(),
            Some("the “store save” call fails")
        );
        assert_eq!(
            failed("err == nil", truthy, "strconv.Atoi").as_deref(),
            Some("the “strconv atoi” call succeeds")
        );
        assert_eq!(
            failed("err != nil", BranchPolarity::FalsyBranch, "Decode").as_deref(),
            Some("the “decode” call succeeds")
        );
        assert_eq!(
            failed("err != nil", truthy, "pendingByID").as_deref(),
            Some("the “pending by ID” call fails")
        );
        assert_eq!(words("HTTPServerURL"), "HTTP server URL");
    }

    #[test]
    fn common_rule_shapes_are_restated_in_domain_terms() {
        let t = BranchPolarity::TruthyBranch;
        let f = BranchPolarity::FalsyBranch;
        let cases = [
            ("\"..\" in full_alias", t, "the full alias contains “..”"),
            ("\"mailbox_id\" in data", t, "mailbox ID is given"),
            ("\"password\" not in data", t, "password is not given"),
            ("\"error\" in request.args", f, "error is not given"),
            // A response's member is not a request input.
            (
                "\"error\" in response.json",
                t,
                "the response's json contains “error”",
            ),
            (
                "not (a.x and (b.y or c.z))",
                t,
                "the a's x is not set or both the b's y is not set and the c's z is not set",
            ),
            (
                "alias_generator not in [\"word\", \"uuid\"]",
                t,
                "the alias generator is not one of “word”, “uuid”",
            ),
            (
                "SESSION_STATE_KEY not in session",
                t,
                "the configured session state key is not one of the session",
            ),
            (
                "\"mailbox_id\" in data and (not mailbox or mailbox.user_id != user.id or not mailbox.verified)",
                t,
                "mailbox ID is given and either no mailbox is given or the mailbox does not belong to the user or the mailbox is not verified",
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
        let cx = Context {
            class: None,
            constant: None,
            failed: None,
        };
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
        assert_eq!(set("alias.note", "compute(alias) * 2"), None);
        assert_eq!(set("alias", "None").unwrap(), "the alias is discarded");
        assert_eq!(
            set("user.expires", "None").unwrap(),
            "the user's expires is set to empty"
        );
        let removed = |t: &str, v: &str| line_breaks_removed(t, v, &cx);
        assert_eq!(
            removed("alias.name", r#"alias.name.replace("\n", "")"#).unwrap(),
            "line breaks are removed from the alias's name"
        );
        assert_eq!(
            removed("alias.name", r#"alias.name.replace('\n', '')"#).unwrap(),
            "line breaks are removed from the alias's name"
        );
        assert_eq!(
            removed("alias.name", r#"other.name.replace("\n", "")"#),
            None
        );
        assert_eq!(
            removed("new_name", r#"new_name.replace("\n", "")"#).unwrap(),
            "line breaks are removed from the new name"
        );
        assert_eq!(
            condition(r#""\n" in data"#, BranchPolarity::TruthyBranch, &cx).unwrap(),
            "the data contains a line break"
        );
        assert_eq!(
            condition(
                r#"alias.name and "\n" in alias.name"#,
                BranchPolarity::TruthyBranch,
                &cx
            )
            .unwrap(),
            "the alias's name is set and the alias's name contains a line break"
        );
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
            called("AliasUsedOn.create(alias_id=alias.id, hostname=hostname, commit=True)")
                .unwrap(),
            "a new “alias used on” record is created"
        );
        assert_eq!(
            called("DeletedAlias.delete(deleted.id)").unwrap(),
            "the “deleted alias” record is deleted"
        );
        assert_eq!(called("Session.delete(alias)").unwrap(), "it runs “delete”");
        assert_eq!(
            called("DeletedAlias(email=alias.email, alias_id=alias.id)").unwrap(),
            "a new “deleted alias” record is created"
        );
        assert_eq!(
            called("RespondError(w, code == 400)").unwrap(),
            "it runs “respond error”"
        );
        assert_eq!(
            called("Alias.filter(Alias.id == alias.id).delete()").unwrap(),
            "the matching “alias” records are deleted"
        );
        // TypeScript (#1998).
        assert_eq!(
            called("await this.connection.getRepository(ctx, OrderLine).save(line)").unwrap(),
            "the “order line” record is saved"
        );
        assert_eq!(
            called("this.connection\n   .getRepository(ctx, User)\n   .update(user.id, x)")
                .unwrap(),
            "the “user” record is updated"
        );
        assert_eq!(
            called("this.orderItemRepository.delete(item.id)").unwrap(),
            "the “order item” record is deleted"
        );
        // A Python `userRepository` alias is not a TypeORM field.
        assert_eq!(
            called("userRepository.delete(x)").unwrap(),
            "it runs “delete”"
        );
        assert_eq!(
            called("this.connection.getRepository(ctx, Payment).insert(p)").unwrap(),
            "a new “payment” record is created"
        );
        assert_eq!(
            called("this.eventBus.publish(new OrderStateTransitionEvent(from, to, ctx, order))")
                .unwrap(),
            "the “order state transition” event is published"
        );
        assert_eq!(
            called("this.historyService.createHistoryEntryForOrder({ ctx })").unwrap(),
            "it runs “create history entry for order”"
        );
        assert_eq!(
            called("flash(\"Alias not found\", \"error\")").unwrap(),
            "the user is shown “Alias not found”"
        );
        assert_eq!(
            called("flash(f\"Deleted {count} aliases\", \"success\")").unwrap(),
            "the user is shown “Deleted … aliases”"
        );
        assert_eq!(
            called("flash(F\"Deleted {count} aliases\", \"success\")").unwrap(),
            "the user is shown “Deleted … aliases”"
        );
        assert_eq!(
            called("flash(f'Deleted {count} aliases', 'success')").unwrap(),
            "the user is shown “Deleted … aliases”"
        );
        assert_eq!(
            called("flash('Alias not found', 'error')").unwrap(),
            "the user is shown “Alias not found”"
        );
        assert_eq!(
            called("flash(f\"{name}\", \"success\")").unwrap(),
            "it runs “flash”"
        );
        assert_eq!(
            called("flash(f\"Use {{x}}\", \"error\")").unwrap(),
            "it runs “flash”"
        );
        assert_eq!(
            called("flash(f\"{n} deleted\", \"success\")").unwrap(),
            "the user is shown “… deleted”"
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

    // AC-1017 (#2018): a Rust guarded return reads as the status it refuses
    // with or the error it fails with.
    #[test]
    fn rust_guarded_returns_are_said_in_words() {
        assert_eq!(rust_status("Err(StatusCode::NOT_FOUND)"), Some((404, None)));
        assert_eq!(
            rust_status("Err(StatusCode::IM_A_TEAPOT)"),
            Some((418, None))
        );
        assert_eq!(
            rust_status("(StatusCode::FORBIDDEN, \"not yours\")"),
            Some((403, Some("not yours".to_owned())))
        );
        assert_eq!(
            rust_status("Err((http::StatusCode::BAD_REQUEST, \"bad id\".to_string()))"),
            Some((400, Some("bad id".to_owned())))
        );
        assert_eq!(rust_status("Ok(StatusCode::OK).map(f)"), None);
        assert_eq!(rust_status("Err(ApiError::NotFound)"), None);
        for (display, expected) in [
            ("Err(ApiError::LoginTaken)", "a “login taken” error"),
            ("Err(Error::NotFound(id))", "a “not found” error"),
            ("Err(AuthError::Expired { at })", "an “expired” error"),
            (
                "Err(anyhow!(\"password too short\"))",
                "the error “password too short”",
            ),
            ("Err(anyhow::anyhow!(\"bad {id}\", id))", "an error"),
            (
                "Err(\"can't be empty\".into())",
                "the error “can't be empty”",
            ),
            ("Err(Error::new(\"locked\"))", "the error “locked”"),
            ("Err(Error::NotFound(id).context(x))", "an error"),
            ("Err(e)", "an error"),
        ] {
            assert_eq!(rust_error(display).as_deref(), Some(expected), "{display}");
        }
        assert_eq!(rust_error("Ok(1)"), None);

        let out = adapters_lang_rust::extract_source(
            br#"use axum::http::StatusCode;
pub struct Accounts;
impl Accounts {
    pub fn register(&self, password: &str) -> Result<u64, ApiError> {
        if password.is_empty() {
            return Err(ApiError::PasswordMissing);
        }
        Ok(1)
    }
}
pub async fn remove(id: u64) -> Result<(), StatusCode> {
    if id == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(())
}
"#,
            "src/accounts.rs",
            &adapters_lang_rust::SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let said = |owner: &str| {
            let rule = out
                .nodes
                .iter()
                .filter(|n| n.label == "BusinessRule")
                .map(|n| GuardedExitEvidence::from_value(n.props["rule"].clone()).unwrap())
                .find(|r| r.owner_id.ends_with(owner))
                .unwrap();
            statement(&rule, owner, None, &[])
        };
        assert_eq!(
            said("Accounts.register").as_deref(),
            Some("When the password is empty, it fails with a “password missing” error.")
        );
        assert_eq!(
            said("remove").as_deref(),
            Some("When the ID is 0, the request is refused with status 404.")
        );
    }

    // AC-0956: a TypeScript answer stating how a value is chosen is said in
    // words: a pick with a fallback and a boolean answer of comparisons.
    #[test]
    fn a_typescript_choice_is_said_in_words() {
        let out = adapters_lang_ts::extract_source(
            b"export class TaxRate {
    test(zone: Zone, taxCategory: TaxCategory): boolean {
        return idsAreEqual(taxCategory.id, this.categoryId) && zone.id === this.zoneId;
    }
}
export class TaxRateService {
    async getApplicableTaxRate(ctx: RequestContext, zone: Zone, taxCategory: TaxCategory): Promise<TaxRate> {
        const rate = (await this.getActiveTaxRates(ctx)).find(r => r.test(zone, taxCategory));
        return rate || this.defaultTaxRate;
    }
}
",
            "src/tax.ts",
            &adapters_lang_ts::SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let said = |owner: &str| {
            let rule = out
                .nodes
                .iter()
                .filter(|n| n.label == "BusinessRule")
                .map(|n| GuardedExitEvidence::from_value(n.props["rule"].clone()).unwrap())
                .find(|r| r.owner_id.ends_with(owner))
                .unwrap();
            statement(&rule, owner, None, &[])
        };
        assert_eq!(
            said("TaxRateService.getApplicableTaxRate").as_deref(),
            Some(
                "It picks the first of the active tax rates whose test with the zone and the \
                 tax category holds; when none does, the default tax rate."
            )
        );
        assert_eq!(
            said("TaxRate.test").as_deref(),
            Some(
                "A tax rate's test holds when the tax category's ID equals the category ID, \
                 and the zone's ID equals the zone ID."
            )
        );
    }

    // AC-1052: a bare tested local reads as its definition only when the
    // recorded definition is a test, not a string or call spelling one.
    #[test]
    fn a_tested_local_reads_as_its_definition_only_when_that_is_a_test() {
        let out = adapters_lang_ts::extract_source(
            b"export function a(variant: any) {
    const untracked = variant.track === 'FALSE' || variant.flag === false;
    if (untracked) {
        throw new Error('x');
    }
}
export function b(variant: any) {
    const label = 'a || b';
    if (label) {
        throw new Error('x');
    }
}
",
            "src/tested.ts",
            &adapters_lang_ts::SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let said = |owner: &str| {
            let rule = out
                .nodes
                .iter()
                .filter(|n| n.label == "BusinessRule")
                .map(|n| GuardedExitEvidence::from_value(n.props["rule"].clone()).unwrap())
                .find(|r| r.owner_id.ends_with(owner))
                .unwrap();
            statement(&rule, owner, None, &[])
        };
        assert_eq!(
            said("#a").as_deref(),
            Some(
                "When the variant's track is \u{201c}FALSE\u{201d} or the variant's flag is \
                 false, it fails with an error."
            )
        );
        assert_eq!(
            said("#b").as_deref(),
            Some(
                "When a label is given, it fails with an error. Here the label is \u{201c}a || b\u{201d}."
            )
        );
    }
}
