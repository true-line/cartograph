//! Overload narrowing by literal argument kinds (AC-0433).
//!
//! Arity alone leaves a call ambiguous when several overloads take the same
//! number of parameters (`waitForCondition(c, 10_000, "msg")` against both
//! `(TestCondition, long, String)` and `(TestCondition, long, Supplier)`).
//! T0 still does not type arbitrary argument expressions, but a literal, a
//! `null` or a lambda proves enough to rule a parameter out: a string
//! literal can only bind to `String` or one of its supertypes, and a lambda
//! never binds to a primitive, an array, `String` or a boxed primitive.
//!
//! Narrowing only removes overloads the argument provably cannot invoke in
//! any of Java's applicability phases (JLS 15.12.2), so when exactly one
//! overload survives it is the one the compiler selects. Anything not proven
//! inapplicable is kept: an expression argument, a type variable, a name this
//! module does not know. No most-specific selection is attempted, so two
//! surviving overloads stay an ambiguous Gap.

use tree_sitter::Node as TsNode;

/// What a call-site argument proves about the parameter it binds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArgKind {
    Str,
    Int,
    Long,
    Bool,
    Char,
    Null,
    /// A lambda expression or method reference: binds only to a functional
    /// interface.
    Lambda,
    /// Any other expression: proves nothing.
    Unknown,
}

/// A declared fixed parameter's erased type, as far as narrowing needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParamShape {
    /// A primitive type keyword (`int`, `boolean`, ...).
    Primitive(String),
    Array,
    /// A type variable of the method or an enclosing type.
    TypeVar,
    /// The simple name of a class or interface type, generics erased.
    Named(String),
    /// A type form narrowing does not model: accepts every argument.
    Unknown,
}

/// The kind of each argument of a `method_invocation`, comments skipped.
pub(crate) fn arg_kinds(source: &[u8], call: TsNode<'_>) -> Vec<ArgKind> {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = arguments.walk();
    arguments
        .named_children(&mut walk)
        .filter(|argument| !crate::is_comment(*argument))
        .map(|argument| arg_kind(source, argument))
        .collect()
}

fn arg_kind(source: &[u8], argument: TsNode<'_>) -> ArgKind {
    match argument.kind() {
        "string_literal" | "text_block" => ArgKind::Str,
        "decimal_integer_literal"
        | "hex_integer_literal"
        | "octal_integer_literal"
        | "binary_integer_literal" => {
            let text = &source[argument.start_byte()..argument.end_byte()];
            if matches!(text.last(), Some(b'l' | b'L')) {
                ArgKind::Long
            } else {
                ArgKind::Int
            }
        }
        "true" | "false" => ArgKind::Bool,
        "character_literal" => ArgKind::Char,
        "null_literal" => ArgKind::Null,
        "lambda_expression" | "method_reference" => ArgKind::Lambda,
        "parenthesized_expression" => argument
            .named_child(0)
            .map_or(ArgKind::Unknown, |inner| arg_kind(source, inner)),
        _ => ArgKind::Unknown,
    }
}

/// Shapes of a declaration's fixed (`formal_parameter`) parameters, in order;
/// a trailing varargs parameter is not included.
pub(crate) fn param_shapes(source: &[u8], declaration: TsNode<'_>) -> Vec<ParamShape> {
    let Some(params) = declaration.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let type_vars = type_variables(source, declaration);
    let mut walk = params.walk();
    params
        .named_children(&mut walk)
        .filter(|param| param.kind() == "formal_parameter")
        .map(|param| {
            // `String names[]`: C-style dimensions after the name.
            let mut inner = param.walk();
            if param
                .named_children(&mut inner)
                .any(|child| child.kind() == "dimensions")
            {
                return ParamShape::Array;
            }
            param
                .child_by_field_name("type")
                .map_or(ParamShape::Unknown, |ty| shape(source, ty, &type_vars))
        })
        .collect()
}

fn shape(source: &[u8], ty: TsNode<'_>, type_vars: &[String]) -> ParamShape {
    let text = || String::from_utf8_lossy(&source[ty.start_byte()..ty.end_byte()]).into_owned();
    match ty.kind() {
        "integral_type" | "floating_point_type" | "boolean_type" => ParamShape::Primitive(text()),
        "array_type" => ParamShape::Array,
        "type_identifier" => {
            let name = text();
            if type_vars.contains(&name) {
                ParamShape::TypeVar
            } else {
                ParamShape::Named(name)
            }
        }
        // `java.util.function.Supplier`, `Map.Entry`: the last segment.
        "scoped_type_identifier" => {
            let mut walk = ty.walk();
            let last = ty
                .named_children(&mut walk)
                .filter(|child| child.kind() == "type_identifier")
                .last();
            last.map_or(ParamShape::Unknown, |last| {
                ParamShape::Named(
                    String::from_utf8_lossy(&source[last.start_byte()..last.end_byte()])
                        .into_owned(),
                )
            })
        }
        // `Supplier<String>` erases to `Supplier`.
        "generic_type" => ty
            .named_child(0)
            .map_or(ParamShape::Unknown, |base| shape(source, base, type_vars)),
        // `@NonNull String`: the annotations come first, the type last.
        "annotated_type" => {
            let mut walk = ty.walk();
            ty.named_children(&mut walk)
                .last()
                .map_or(ParamShape::Unknown, |base| shape(source, base, type_vars))
        }
        _ => ParamShape::Unknown,
    }
}

/// Type parameter names in scope at a declaration: its own and every
/// enclosing declaration's (generic methods, classes, interfaces, records).
fn type_variables(source: &[u8], declaration: TsNode<'_>) -> Vec<String> {
    let mut names = Vec::new();
    let mut node = Some(declaration);
    while let Some(current) = node {
        let mut walk = current.walk();
        for child in current.named_children(&mut walk) {
            if child.kind() != "type_parameters" {
                continue;
            }
            let mut params = child.walk();
            for param in child.named_children(&mut params) {
                let mut parts = param.walk();
                if let Some(name) = param
                    .named_children(&mut parts)
                    .find(|part| part.kind() == "type_identifier")
                {
                    names.push(
                        String::from_utf8_lossy(&source[name.start_byte()..name.end_byte()])
                            .into_owned(),
                    );
                }
            }
        }
        node = current.parent();
    }
    names
}

/// Simple names of `String`'s supertypes.
const STRING_SUPERTYPES: &[&str] = &[
    "String",
    "CharSequence",
    "Object",
    "Serializable",
    "Comparable",
    "Constable",
    "ConstantDesc",
];

/// Boxed primitives and `String`: final classes that are not functional
/// interfaces, so no lambda binds to them.
const NON_FUNCTIONAL: &[&str] = &[
    "String",
    "Integer",
    "Long",
    "Short",
    "Byte",
    "Character",
    "Boolean",
    "Double",
    "Float",
];

/// Whether `arg` could bind to a parameter of shape `param` in some
/// applicability phase. `false` only when that is impossible.
fn accepts(param: &ParamShape, arg: ArgKind) -> bool {
    match (param, arg) {
        (_, ArgKind::Unknown) | (ParamShape::TypeVar | ParamShape::Unknown, _) => true,
        (ParamShape::Primitive(primitive), arg) => {
            let widened: &[&str] = match arg {
                // No narrowing of constants in invocation contexts (JLS 5.3).
                ArgKind::Int => &["int", "long", "float", "double"],
                ArgKind::Long => &["long", "float", "double"],
                ArgKind::Char => &["char", "int", "long", "float", "double"],
                ArgKind::Bool => &["boolean"],
                _ => &[],
            };
            widened.contains(&primitive.as_str())
        }
        (ParamShape::Array, arg) => arg == ArgKind::Null,
        (ParamShape::Named(_), ArgKind::Null) => true,
        (ParamShape::Named(name), ArgKind::Lambda) => !NON_FUNCTIONAL.contains(&name.as_str()),
        (ParamShape::Named(name), arg) => {
            let boxed = match arg {
                ArgKind::Str => return STRING_SUPERTYPES.contains(&name.as_str()),
                ArgKind::Int => "Integer",
                ArgKind::Long => "Long",
                ArgKind::Char => "Character",
                ArgKind::Bool => "Boolean",
                _ => return true,
            };
            let numeric = matches!(arg, ArgKind::Int | ArgKind::Long);
            name == boxed
                || (numeric && name == "Number")
                || ["Object", "Serializable", "Comparable", "Constable"].contains(&name.as_str())
                || (arg != ArgKind::Bool && arg != ArgKind::Char && name == "ConstantDesc")
        }
    }
}

/// Whether a call with these argument kinds could invoke an overload whose
/// fixed parameters have these shapes. Only fixed positions are checked; a
/// varargs tail is never narrowed.
fn applicable(params: &[ParamShape], args: &[ArgKind]) -> bool {
    params
        .iter()
        .zip(args)
        .all(|(param, arg)| accepts(param, *arg))
}

/// The `overload_resolution` value on a CALLS edge that literal-kind
/// narrowing decided, so an audit can tell it from an arity-only match.
pub(crate) const RESOLUTION: &str = "literal-argument-kinds";

/// Among several arity-matching overloads, the one symbol the call's
/// argument kinds leave applicable. `None` when arity already decided (zero
/// or one candidate) or when zero or several overloads survive narrowing.
pub(crate) fn narrow<'a>(
    candidates: &[(&'a str, &[ParamShape])],
    args: &[ArgKind],
) -> Option<&'a str> {
    if candidates.len() < 2 {
        return None;
    }
    let mut survivors = candidates
        .iter()
        .filter(|(_, params)| applicable(params, args));
    match (survivors.next(), survivors.next()) {
        (Some((symbol, _)), None) => Some(symbol),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> ParamShape {
        ParamShape::Named(name.into())
    }

    // AC-0433: each literal kind is ruled out exactly where Java cannot bind it.
    #[test]
    fn literal_kinds_rule_out_only_impossible_parameters() {
        assert!(accepts(&named("String"), ArgKind::Str));
        assert!(accepts(&named("CharSequence"), ArgKind::Str));
        assert!(accepts(&named("Object"), ArgKind::Str));
        assert!(!accepts(&named("Supplier"), ArgKind::Str));
        assert!(!accepts(&ParamShape::Primitive("int".into()), ArgKind::Str));

        assert!(accepts(&ParamShape::Primitive("long".into()), ArgKind::Int));
        assert!(!accepts(
            &ParamShape::Primitive("short".into()),
            ArgKind::Int
        ));
        assert!(!accepts(
            &ParamShape::Primitive("int".into()),
            ArgKind::Long
        ));
        assert!(accepts(&named("Integer"), ArgKind::Int));
        assert!(!accepts(&named("Long"), ArgKind::Int));
        assert!(accepts(&named("Number"), ArgKind::Long));
        assert!(!accepts(&named("String"), ArgKind::Int));

        assert!(!accepts(&named("String"), ArgKind::Lambda));
        assert!(!accepts(
            &ParamShape::Primitive("int".into()),
            ArgKind::Lambda
        ));
        assert!(!accepts(&ParamShape::Array, ArgKind::Lambda));
        assert!(accepts(&named("Supplier"), ArgKind::Lambda));

        assert!(!accepts(
            &ParamShape::Primitive("int".into()),
            ArgKind::Null
        ));
        assert!(accepts(&named("Supplier"), ArgKind::Null));
        assert!(accepts(&ParamShape::Array, ArgKind::Null));

        for arg in [ArgKind::Str, ArgKind::Int, ArgKind::Lambda, ArgKind::Null] {
            assert!(accepts(&ParamShape::TypeVar, arg));
            assert!(accepts(&ParamShape::Unknown, arg));
        }
        assert!(accepts(
            &ParamShape::Primitive("int".into()),
            ArgKind::Unknown
        ));
    }
}
