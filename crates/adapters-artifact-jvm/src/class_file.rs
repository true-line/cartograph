//! A bounded reader of the JVM class-file format (JVMS §4): the constant
//! pool, the class's own name, super class and interfaces, its methods and
//! the method calls each method's bytecode makes. Nothing is executed or
//! verified; a class the reader cannot walk byte for byte is refused whole.

use std::fmt;

/// The class-file magic number.
const MAGIC: u32 = 0xCAFE_BABE;

/// Access flag: an interface.
pub const ACC_INTERFACE: u16 = 0x0200;
/// Access flag: an abstract class or method.
pub const ACC_ABSTRACT: u16 = 0x0400;
/// Access flag: a compiler-generated member.
pub const ACC_SYNTHETIC: u16 = 0x1000;
/// Access flag: an annotation interface.
pub const ACC_ANNOTATION: u16 = 0x2000;
/// Access flag: an enum class.
pub const ACC_ENUM: u16 = 0x4000;
/// Access flag on a method: a bridge method.
pub const ACC_BRIDGE: u16 = 0x0040;
/// Access flag: a static member (on an `InnerClasses` entry, a static
/// nested class).
pub const ACC_STATIC: u16 = 0x0008;

/// Why a class file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The bytes do not start with `0xCAFEBABE`.
    Magic,
    /// The bytes end inside a structure.
    Truncated,
    /// A constant-pool tag this reader does not know.
    ConstantTag,
    /// A constant-pool index out of range or of the wrong kind.
    ConstantIndex,
    /// An opcode outside the JVM instruction set, or a malformed switch.
    Opcode,
    /// Bytes left over after the class's attributes.
    Trailing,
}

impl Refusal {
    /// A stable code for the refusal, recorded on its Gap.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Magic => "magic",
            Refusal::Truncated => "truncated",
            Refusal::ConstantTag => "constant_tag",
            Refusal::ConstantIndex => "constant_index",
            Refusal::Opcode => "opcode",
            Refusal::Trailing => "trailing_bytes",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// A byte span of the class file, end exclusive.
pub type Span = (u64, u64);

/// A method a call instruction names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRef {
    /// Internal name of the owner class (`com/acme/Pay`).
    pub owner: String,
    /// Method name (`<init>` for a constructor).
    pub name: String,
    /// Method descriptor (`(Ljava/lang/String;)V`).
    pub descriptor: String,
}

/// One call instruction in a method's bytecode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// `invokevirtual`, `invokespecial`, `invokestatic` or `invokeinterface`.
    Invoke {
        /// The instruction's mnemonic.
        opcode: &'static str,
        /// The method it names.
        target: MemberRef,
        /// The instruction's bytes in the class file.
        span: Span,
    },
    /// `invokedynamic` whose bootstrap arguments name methods of the class
    /// (a lambda or method reference body).
    Dynamic {
        /// Methods named by method handles in the bootstrap arguments.
        handles: Vec<MemberRef>,
        /// The instruction's bytes in the class file.
        span: Span,
    },
}

/// One method of the class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    /// Access flags.
    pub access: u16,
    /// Method name.
    pub name: String,
    /// Method descriptor.
    pub descriptor: String,
    /// The `method_info` structure's bytes.
    pub span: Span,
    /// Calls in its `Code` attribute, in bytecode order.
    pub calls: Vec<Call>,
}

/// A class reference in the constant pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRef {
    /// Internal name (`com/acme/Pay`, or `[Lcom/acme/Pay;` for an array).
    pub name: String,
    /// The `CONSTANT_Class_info` entry's bytes.
    pub span: Span,
}

/// One field of the class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field descriptor (`Ljava/time/Clock;`).
    pub descriptor: String,
    /// The `field_info` structure's bytes.
    pub span: Span,
}

/// The class's own `InnerClasses` entry: it is nested in another class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nesting {
    /// Internal name of the enclosing class, `None` for a local or
    /// anonymous class.
    pub outer: Option<String>,
    /// The access flags the source declared (`ACC_STATIC` for a static
    /// nested class).
    pub access: u16,
}

/// What a class file states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassFile {
    /// Access flags.
    pub access: u16,
    /// Internal name of the class.
    pub this_class: String,
    /// Internal name of the super class (`None` for `java/lang/Object`).
    pub super_class: Option<String>,
    /// Internal names of the direct interfaces.
    pub interfaces: Vec<String>,
    /// The bytes of the access flags, this-class and super-class fields.
    pub header_span: Span,
    /// Every class the constant pool references, in pool order.
    pub class_refs: Vec<ClassRef>,
    /// Fields in declaration order.
    pub fields: Vec<Field>,
    /// Methods in declaration order.
    pub methods: Vec<Method>,
    /// How the class is nested, from its own `InnerClasses` entry.
    pub nesting: Option<Nesting>,
}

#[derive(Debug, Clone)]
enum Constant {
    Unused,
    Utf8(String),
    Class(u16),
    NameAndType(u16, u16),
    MemberRef(u16, u16),
    FieldRef,
    MethodHandle(u16),
    InvokeDynamic(u16),
    Other,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Refusal> {
        let end = self.at.checked_add(n).ok_or(Refusal::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(Refusal::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, Refusal> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Refusal> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, Refusal> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

struct Pool {
    entries: Vec<Constant>,
    spans: Vec<Span>,
}

impl Pool {
    fn get(&self, index: u16) -> Result<&Constant, Refusal> {
        match self.entries.get(usize::from(index)) {
            Some(Constant::Unused) | None => Err(Refusal::ConstantIndex),
            Some(entry) => Ok(entry),
        }
    }

    fn utf8(&self, index: u16) -> Result<&str, Refusal> {
        match self.get(index)? {
            Constant::Utf8(text) => Ok(text),
            _ => Err(Refusal::ConstantIndex),
        }
    }

    fn class(&self, index: u16) -> Result<&str, Refusal> {
        match self.get(index)? {
            Constant::Class(name) => self.utf8(*name),
            _ => Err(Refusal::ConstantIndex),
        }
    }

    fn member(&self, index: u16) -> Result<MemberRef, Refusal> {
        let Constant::MemberRef(class, nat) = self.get(index)? else {
            return Err(Refusal::ConstantIndex);
        };
        let Constant::NameAndType(name, descriptor) = self.get(*nat)? else {
            return Err(Refusal::ConstantIndex);
        };
        Ok(MemberRef {
            owner: self.class(*class)?.to_string(),
            name: self.utf8(*name)?.to_string(),
            descriptor: self.utf8(*descriptor)?.to_string(),
        })
    }
}

/// Decode modified UTF-8 (JVMS §4.4.7): a NUL is `C0 80` and supplementary
/// characters are surrogate pairs. Anything else undecodable is replaced.
fn modified_utf8(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (unit, len) = if b & 0x80 == 0 {
            (u16::from(b), 1)
        } else if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            (
                (u16::from(b & 0x1F) << 6) | u16::from(bytes[i + 1] & 0x3F),
                2,
            )
        } else if b & 0xF0 == 0xE0 && i + 2 < bytes.len() {
            (
                (u16::from(b & 0x0F) << 12)
                    | (u16::from(bytes[i + 1] & 0x3F) << 6)
                    | u16::from(bytes[i + 2] & 0x3F),
                3,
            )
        } else {
            (0xFFFD, 1)
        };
        units.push(unit);
        i += len;
    }
    String::from_utf16_lossy(&units)
}

fn read_pool(reader: &mut Reader<'_>) -> Result<Pool, Refusal> {
    let count = reader.u16()?;
    let mut entries = vec![Constant::Unused];
    let mut spans = vec![(0, 0)];
    let mut index = 1u16;
    while index < count {
        let start = reader.at;
        let tag = reader.u8()?;
        let (entry, wide) = match tag {
            1 => {
                let len = reader.u16()?;
                (
                    Constant::Utf8(modified_utf8(reader.take(usize::from(len))?)),
                    false,
                )
            }
            3 | 4 => {
                reader.take(4)?;
                (Constant::Other, false)
            }
            5 | 6 => {
                reader.take(8)?;
                (Constant::Other, true)
            }
            7 => (Constant::Class(reader.u16()?), false),
            8 | 16 | 19 | 20 => {
                reader.u16()?;
                (Constant::Other, false)
            }
            9 => {
                reader.take(4)?;
                (Constant::FieldRef, false)
            }
            10 | 11 => (Constant::MemberRef(reader.u16()?, reader.u16()?), false),
            12 => (Constant::NameAndType(reader.u16()?, reader.u16()?), false),
            15 => {
                reader.u8()?;
                (Constant::MethodHandle(reader.u16()?), false)
            }
            17 => {
                reader.take(4)?;
                (Constant::Other, false)
            }
            18 => {
                let bootstrap = reader.u16()?;
                reader.u16()?;
                (Constant::InvokeDynamic(bootstrap), false)
            }
            _ => return Err(Refusal::ConstantTag),
        };
        entries.push(entry);
        spans.push((start as u64, reader.at as u64));
        index += 1;
        if wide {
            // An 8-byte constant takes two pool slots (JVMS §4.4.5).
            entries.push(Constant::Unused);
            spans.push((0, 0));
            index = index.checked_add(1).ok_or(Refusal::ConstantIndex)?;
        }
    }
    Ok(Pool { entries, spans })
}

/// The length of the instruction at `pc` in `code`.
fn instruction_len(code: &[u8], pc: usize) -> Result<usize, Refusal> {
    let opcode = *code.get(pc).ok_or(Refusal::Truncated)?;
    let len = match opcode {
        0x00..=0x0f | 0x1a..=0x35 | 0x3b..=0x83 | 0x85..=0x98 | 0xac..=0xb1 => 1,
        0xbe | 0xbf | 0xc2 | 0xc3 | 0xca => 1,
        0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
        0x11 | 0x13 | 0x14 | 0x84 | 0x99..=0xa8 | 0xb2..=0xb8 => 3,
        0xbb | 0xbd | 0xc0 | 0xc1 | 0xc6 | 0xc7 => 3,
        0xc5 => 4,
        0xb9 | 0xba | 0xc8 | 0xc9 => 5,
        0xc4 => match code.get(pc + 1) {
            Some(0x84) => 6,
            Some(_) => 4,
            None => return Err(Refusal::Truncated),
        },
        0xaa | 0xab => {
            // Operands start at the next 4-byte boundary of the code array.
            let base = (pc + 4) & !3;
            let word = |at: usize| -> Result<i32, Refusal> {
                let b = code.get(at..at + 4).ok_or(Refusal::Truncated)?;
                Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            };
            let entries = if opcode == 0xaa {
                let (low, high) = (word(base + 4)?, word(base + 8)?);
                if high < low {
                    return Err(Refusal::Opcode);
                }
                12 + 4 * (i64::from(high) - i64::from(low) + 1)
            } else {
                let pairs = word(base + 4)?;
                if pairs < 0 {
                    return Err(Refusal::Opcode);
                }
                8 + 8 * i64::from(pairs)
            };
            let len = (base - pc) as i64 + entries;
            usize::try_from(len).map_err(|_| Refusal::Opcode)?
        }
        _ => return Err(Refusal::Opcode),
    };
    if pc + len > code.len() {
        return Err(Refusal::Truncated);
    }
    Ok(len)
}

fn mnemonic(opcode: u8) -> &'static str {
    match opcode {
        0xb6 => "invokevirtual",
        0xb7 => "invokespecial",
        0xb8 => "invokestatic",
        _ => "invokeinterface",
    }
}

/// Bootstrap method arguments that are method handles, per bootstrap index.
type Bootstraps = Vec<Vec<MemberRef>>;

fn read_calls(
    code: &[u8],
    code_start: usize,
    pool: &Pool,
    bootstraps: &Bootstraps,
) -> Result<Vec<Call>, Refusal> {
    let mut calls = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        let len = instruction_len(code, pc)?;
        let span = ((code_start + pc) as u64, (code_start + pc + len) as u64);
        let opcode = code[pc];
        if matches!(opcode, 0xb6..=0xba) {
            let index = u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
            if opcode == 0xba {
                let Constant::InvokeDynamic(bootstrap) = pool.get(index)? else {
                    return Err(Refusal::ConstantIndex);
                };
                let handles = bootstraps
                    .get(usize::from(*bootstrap))
                    .cloned()
                    .ok_or(Refusal::ConstantIndex)?;
                calls.push(Call::Dynamic { handles, span });
            } else {
                calls.push(Call::Invoke {
                    opcode: mnemonic(opcode),
                    target: pool.member(index)?,
                    span,
                });
            }
        }
        pc += len;
    }
    Ok(calls)
}

struct RawMethod {
    access: u16,
    name: String,
    descriptor: String,
    span: Span,
    code: Option<(usize, usize)>,
}

/// Skip one member's attributes, returning the `Code` attribute's code
/// array position when `want_code` and it is present.
fn read_attributes(
    reader: &mut Reader<'_>,
    pool: &Pool,
    want_code: bool,
) -> Result<Option<(usize, usize)>, Refusal> {
    let mut code = None;
    for _ in 0..reader.u16()? {
        let name = reader.u16()?;
        let len = reader.u32()? as usize;
        let start = reader.at;
        let body = reader.take(len)?;
        if want_code && pool.utf8(name)? == "Code" {
            if body.len() < 8 {
                return Err(Refusal::Truncated);
            }
            let code_len = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
            if 8 + code_len > body.len() {
                return Err(Refusal::Truncated);
            }
            code = Some((start + 8, code_len));
        }
    }
    Ok(code)
}

fn read_bootstraps(body: &[u8], pool: &Pool) -> Result<Bootstraps, Refusal> {
    let mut reader = Reader { bytes: body, at: 0 };
    let mut out = Vec::new();
    for _ in 0..reader.u16()? {
        reader.u16()?;
        let mut handles = Vec::new();
        for _ in 0..reader.u16()? {
            let argument = reader.u16()?;
            if let Constant::MethodHandle(reference) = pool.get(argument)? {
                // A field handle (a getter or setter) calls no method.
                if !matches!(pool.get(*reference)?, Constant::FieldRef) {
                    handles.push(pool.member(*reference)?);
                }
            }
        }
        out.push(handles);
    }
    Ok(out)
}

/// Read a class file.
pub fn parse(bytes: &[u8]) -> Result<ClassFile, Refusal> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.u32().map_err(|_| Refusal::Magic)? != MAGIC {
        return Err(Refusal::Magic);
    }
    reader.take(4)?;
    let pool = read_pool(&mut reader)?;
    let header_start = reader.at as u64;
    let access = reader.u16()?;
    let this_class = pool.class(reader.u16()?)?.to_string();
    let super_index = reader.u16()?;
    let super_class = if super_index == 0 {
        None
    } else {
        Some(pool.class(super_index)?.to_string())
    };
    let header_span = (header_start, reader.at as u64);
    let mut interfaces = Vec::new();
    for _ in 0..reader.u16()? {
        interfaces.push(pool.class(reader.u16()?)?.to_string());
    }
    let mut fields = Vec::new();
    for _ in 0..reader.u16()? {
        let start = reader.at as u64;
        reader.u16()?;
        pool.utf8(reader.u16()?)?;
        let descriptor = pool.utf8(reader.u16()?)?.to_string();
        read_attributes(&mut reader, &pool, false)?;
        fields.push(Field {
            descriptor,
            span: (start, reader.at as u64),
        });
    }
    let mut raw = Vec::new();
    for _ in 0..reader.u16()? {
        let start = reader.at as u64;
        let access = reader.u16()?;
        let name = pool.utf8(reader.u16()?)?.to_string();
        let descriptor = pool.utf8(reader.u16()?)?.to_string();
        let code = read_attributes(&mut reader, &pool, true)?;
        raw.push(RawMethod {
            access,
            name,
            descriptor,
            span: (start, reader.at as u64),
            code,
        });
    }
    let mut bootstraps = Vec::new();
    let mut nesting = None;
    for _ in 0..reader.u16()? {
        let name = reader.u16()?;
        let len = reader.u32()? as usize;
        let body = reader.take(len)?;
        match pool.utf8(name)? {
            "BootstrapMethods" => bootstraps = read_bootstraps(body, &pool)?,
            "InnerClasses" => nesting = read_nesting(body, &pool, &this_class)?,
            _ => {}
        }
    }
    if reader.at != bytes.len() {
        return Err(Refusal::Trailing);
    }
    let mut methods = Vec::with_capacity(raw.len());
    for method in raw {
        let calls = match method.code {
            Some((start, len)) => {
                read_calls(&bytes[start..start + len], start, &pool, &bootstraps)?
            }
            None => Vec::new(),
        };
        methods.push(Method {
            access: method.access,
            name: method.name,
            descriptor: method.descriptor,
            span: method.span,
            calls,
        });
    }
    let class_refs = pool
        .entries
        .iter()
        .zip(&pool.spans)
        .filter_map(|(entry, span)| match entry {
            Constant::Class(name) => pool.utf8(*name).ok().map(|name| ClassRef {
                name: name.to_string(),
                span: *span,
            }),
            _ => None,
        })
        .collect();
    Ok(ClassFile {
        access,
        this_class,
        super_class,
        interfaces,
        header_span,
        class_refs,
        fields,
        methods,
        nesting,
    })
}

/// The class's own entry in its `InnerClasses` attribute (JVMS §4.7.6).
fn read_nesting(body: &[u8], pool: &Pool, this_class: &str) -> Result<Option<Nesting>, Refusal> {
    let mut reader = Reader { bytes: body, at: 0 };
    let mut own = None;
    for _ in 0..reader.u16()? {
        let inner = reader.u16()?;
        let outer = reader.u16()?;
        reader.u16()?;
        let access = reader.u16()?;
        if pool.class(inner)? == this_class {
            let outer = if outer == 0 {
                None
            } else {
                Some(pool.class(outer)?.to_string())
            };
            own = Some(Nesting { outer, access });
        }
    }
    if reader.at != body.len() {
        return Err(Refusal::Truncated);
    }
    Ok(own)
}

/// The internal names of the classes a field or method descriptor names,
/// array element classes included, in order.
pub fn descriptor_classes(descriptor: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = descriptor;
    while let Some(at) = rest.find('L') {
        let Some(end) = rest[at..].find(';') else {
            break;
        };
        out.push(&rest[at + 1..at + end]);
        rest = &rest[at + end + 1..];
    }
    out
}

/// The Java-source spelling of a field descriptor's type: simple class
/// names (nested `$` read as `.`), primitives by keyword, arrays with `[]`.
fn field_type(descriptor: &str) -> Option<(String, &str)> {
    let mut dims = 0;
    let mut rest = descriptor;
    while let Some(next) = rest.strip_prefix('[') {
        dims += 1;
        rest = next;
    }
    let (base, rest) = match rest.as_bytes().first()? {
        b'B' => ("byte".to_string(), &rest[1..]),
        b'C' => ("char".to_string(), &rest[1..]),
        b'D' => ("double".to_string(), &rest[1..]),
        b'F' => ("float".to_string(), &rest[1..]),
        b'I' => ("int".to_string(), &rest[1..]),
        b'J' => ("long".to_string(), &rest[1..]),
        b'S' => ("short".to_string(), &rest[1..]),
        b'Z' => ("boolean".to_string(), &rest[1..]),
        b'V' => ("void".to_string(), &rest[1..]),
        b'L' => {
            let end = rest.find(';')?;
            let internal = &rest[1..end];
            let simple = internal.rsplit('/').next().unwrap_or(internal);
            (simple.replace('$', "."), &rest[end + 1..])
        }
        _ => return None,
    };
    Some((format!("{base}{}", "[]".repeat(dims)), rest))
}

/// The parameter types of a method descriptor, spelled as in Java source
/// (erased: no type arguments).
pub fn parameter_types(descriptor: &str) -> Option<Vec<String>> {
    let mut rest = descriptor.strip_prefix('(')?;
    let mut types = Vec::new();
    while !rest.starts_with(')') {
        let (spelled, next) = field_type(rest)?;
        types.push(spelled);
        rest = next;
    }
    Some(types)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `invokedynamic` naming a bootstrap method the class does not have
    /// is a bad constant index, not a call with no handles.
    #[test]
    fn a_missing_bootstrap_method_is_refused() {
        let pool = Pool {
            entries: vec![Constant::Unused, Constant::InvokeDynamic(3)],
            spans: vec![(0, 0), (10, 15)],
        };
        let code = [0xba, 0, 1, 0, 0, 0xb1];
        assert_eq!(
            read_calls(&code, 0, &pool, &Vec::new()),
            Err(Refusal::ConstantIndex)
        );
        let bootstraps = vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        assert_eq!(read_calls(&code, 0, &pool, &bootstraps).unwrap().len(), 1);
    }

    #[test]
    fn descriptors_name_their_classes() {
        assert_eq!(
            descriptor_classes("(Ljava/time/Clock;I[[Lcom/a/B$C;)Ljava/util/List;"),
            vec!["java/time/Clock", "com/a/B$C", "java/util/List"]
        );
        assert!(descriptor_classes("(IJ)V").is_empty());
    }
}
