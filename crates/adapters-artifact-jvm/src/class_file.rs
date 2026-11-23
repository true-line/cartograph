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
    /// A field or method descriptor that is not well formed (JVMS §4.3).
    Descriptor,
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
            Refusal::Descriptor => "descriptor",
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
    /// The source file name its `SourceFile` attribute records
    /// (`Ledger.java`, `Models.kt`).
    pub source_file: Option<String>,
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
    /// A `CONSTANT_InvokeDynamic`, naming a bootstrap method by index.
    InvokeDynamic(u16),
    /// A `CONSTANT_Dynamic`, naming a bootstrap method by index.
    Dynamic(u16),
    Other,
}

/// What a constant-pool reference must name.
#[derive(Debug, Clone, Copy)]
enum Want {
    Utf8,
    Class,
    NameAndType,
    /// A field, method or interface method reference.
    Member,
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

/// Every constant's references to other constants name a constant of the
/// kind JVMS §4.4 requires, so a class whose pool cites a missing or wrong
/// constant is refused even where the reader does not need that constant.
fn check_references(pool: &Pool, refs: &[(u16, Want)]) -> Result<(), Refusal> {
    for &(index, want) in refs {
        let ok = matches!(
            (want, pool.get(index)?),
            (Want::Utf8, Constant::Utf8(_))
                | (Want::Class, Constant::Class(_))
                | (Want::NameAndType, Constant::NameAndType(..))
                | (Want::Member, Constant::MemberRef(..) | Constant::FieldRef)
        );
        if !ok {
            return Err(Refusal::ConstantIndex);
        }
    }
    Ok(())
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
    let mut refs = Vec::new();
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
            7 => {
                let name = reader.u16()?;
                refs.push((name, Want::Utf8));
                (Constant::Class(name), false)
            }
            // String, MethodType, Module and Package name a UTF-8 constant.
            8 | 16 | 19 | 20 => {
                refs.push((reader.u16()?, Want::Utf8));
                (Constant::Other, false)
            }
            9..=11 => {
                let (class, nat) = (reader.u16()?, reader.u16()?);
                refs.push((class, Want::Class));
                refs.push((nat, Want::NameAndType));
                let entry = if tag == 9 {
                    Constant::FieldRef
                } else {
                    Constant::MemberRef(class, nat)
                };
                (entry, false)
            }
            12 => {
                let (name, descriptor) = (reader.u16()?, reader.u16()?);
                refs.push((name, Want::Utf8));
                refs.push((descriptor, Want::Utf8));
                (Constant::NameAndType(name, descriptor), false)
            }
            15 => {
                reader.u8()?;
                let reference = reader.u16()?;
                refs.push((reference, Want::Member));
                (Constant::MethodHandle(reference), false)
            }
            17 | 18 => {
                let bootstrap = reader.u16()?;
                refs.push((reader.u16()?, Want::NameAndType));
                let entry = if tag == 18 {
                    Constant::InvokeDynamic(bootstrap)
                } else {
                    Constant::Dynamic(bootstrap)
                };
                (entry, false)
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
    let pool = Pool { entries, spans };
    check_references(&pool, &refs)?;
    Ok(pool)
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
            code = Some((start + 8, code_length(body)?));
        }
    }
    Ok(code)
}

/// The length of a `Code` attribute's bytecode array, once the whole
/// attribute (JVMS §4.7.3) is walked: the exception table and the nested
/// attributes must fill its body exactly.
fn code_length(body: &[u8]) -> Result<usize, Refusal> {
    let mut reader = Reader { bytes: body, at: 0 };
    reader.take(4)?;
    let code_len = reader.u32()? as usize;
    reader.take(code_len)?;
    let handlers = usize::from(reader.u16()?);
    reader.take(handlers * 8)?;
    for _ in 0..reader.u16()? {
        reader.u16()?;
        let len = reader.u32()? as usize;
        reader.take(len)?;
    }
    if reader.at != body.len() {
        return Err(Refusal::Truncated);
    }
    Ok(code_len)
}

fn read_bootstraps(body: &[u8], pool: &Pool) -> Result<Bootstraps, Refusal> {
    let mut reader = Reader { bytes: body, at: 0 };
    let mut out = Vec::new();
    for _ in 0..reader.u16()? {
        if !matches!(pool.get(reader.u16()?)?, Constant::MethodHandle(_)) {
            return Err(Refusal::ConstantIndex);
        }
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
    if reader.at != body.len() {
        return Err(Refusal::Truncated);
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
        if !field_descriptor_ok(&descriptor) {
            return Err(Refusal::Descriptor);
        }
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
        if !method_descriptor_ok(&descriptor) {
            return Err(Refusal::Descriptor);
        }
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
    let mut source_file = None;
    for _ in 0..reader.u16()? {
        let name = reader.u16()?;
        let len = reader.u32()? as usize;
        let body = reader.take(len)?;
        match pool.utf8(name)? {
            "BootstrapMethods" => bootstraps = read_bootstraps(body, &pool)?,
            "InnerClasses" => nesting = read_nesting(body, &pool, &this_class)?,
            "SourceFile" => {
                let [high, low] = body else {
                    return Err(Refusal::Truncated);
                };
                source_file = Some(pool.utf8(u16::from_be_bytes([*high, *low]))?.to_string());
            }
            _ => {}
        }
    }
    if reader.at != bytes.len() {
        return Err(Refusal::Trailing);
    }
    // Every dynamic constant names a bootstrap method the class has.
    if pool.entries.iter().any(|entry| {
        matches!(entry, Constant::InvokeDynamic(at) | Constant::Dynamic(at)
            if usize::from(*at) >= bootstraps.len())
    }) {
        return Err(Refusal::ConstantIndex);
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
        source_file,
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
/// names (nested `$` read as `.`), or fully qualified ones when `qualified`,
/// primitives by keyword, arrays with `[]`.
fn field_type(descriptor: &str, qualified: bool) -> Option<(String, &str)> {
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
            if internal.is_empty() {
                return None;
            }
            let name = if qualified {
                internal
            } else {
                internal.rsplit('/').next().unwrap_or(internal)
            };
            (name.replace(['/', '$'], "."), &rest[end + 1..])
        }
        _ => return None,
    };
    Some((format!("{base}{}", "[]".repeat(dims)), rest))
}

/// The parameter types of a method descriptor, spelled as in Java source
/// (erased: no type arguments).
pub fn parameter_types(descriptor: &str) -> Option<Vec<String>> {
    spelled_parameters(descriptor, false)
}

/// [`parameter_types`] with every class fully qualified (`java.util.Date`),
/// to tell apart overloads whose simple spellings collide.
pub fn qualified_parameter_types(descriptor: &str) -> Option<Vec<String>> {
    spelled_parameters(descriptor, true)
}

fn spelled_parameters(descriptor: &str, qualified: bool) -> Option<Vec<String>> {
    Some(split_method(descriptor, qualified)?.0)
}

/// A method descriptor's spelled parameters and the text after `)`.
fn split_method(descriptor: &str, qualified: bool) -> Option<(Vec<String>, &str)> {
    let mut rest = descriptor.strip_prefix('(')?;
    let mut types = Vec::new();
    while !rest.starts_with(')') {
        if is_void(rest) {
            return None;
        }
        let (spelled, next) = field_type(rest, qualified)?;
        types.push(spelled);
        rest = next;
    }
    Some((types, &rest[1..]))
}

/// Whether a descriptor fragment's type is `void` (or an array of it).
fn is_void(fragment: &str) -> bool {
    fragment.trim_start_matches('[').starts_with('V')
}

/// A whole field descriptor: one non-void field type and nothing after it.
fn field_descriptor_ok(descriptor: &str) -> bool {
    !is_void(descriptor) && matches!(field_type(descriptor, false), Some((_, "")))
}

/// A whole method descriptor: parameters, then one return type (or `V`)
/// and nothing after it.
fn method_descriptor_ok(descriptor: &str) -> bool {
    split_method(descriptor, false).is_some_and(|(_, ret)| ret == "V" || field_descriptor_ok(ret))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Descriptors are walked whole (JVMS §4.3): no unknown type letter,
    /// no `void` parameter or field, nothing after the return type.
    #[test]
    fn descriptors_are_checked_whole() {
        for good in ["()V", "(I[JLjava/util/List;)Ljava/lang/String;", "([[Z)[I"] {
            assert!(method_descriptor_ok(good), "{good}");
        }
        for bad in ["(Q)V", "(V)V", "()", "()VV", "(I", "()[V", "(L;)V", "I"] {
            assert!(!method_descriptor_ok(bad), "{bad}");
        }
        assert!(field_descriptor_ok("[Ljava/util/Map$Entry;"));
        for bad in ["V", "[V", "II", "Lx", ""] {
            assert!(!field_descriptor_ok(bad), "{bad}");
        }
    }

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

    /// A `Code` attribute must hold its exception table and nested
    /// attributes after the bytecode; one that ends early is truncated.
    #[test]
    fn a_code_attribute_is_walked_whole() {
        // max_stack, max_locals, code_length 1, `return`.
        let head = [0, 1, 0, 1, 0, 0, 0, 1, 0xb1];
        let whole = [&head[..], &[0, 0, 0, 0]].concat();
        assert_eq!(code_length(&whole), Ok(1));
        assert_eq!(code_length(&head), Err(Refusal::Truncated));
        let no_attributes = [&head[..], &[0, 0]].concat();
        assert_eq!(code_length(&no_attributes), Err(Refusal::Truncated));
        let short_handler = [&head[..], &[0, 1, 0, 0]].concat();
        assert_eq!(code_length(&short_handler), Err(Refusal::Truncated));
        let trailing = [&whole[..], &[0]].concat();
        assert_eq!(code_length(&trailing), Err(Refusal::Truncated));
    }

    /// A constant a reader never needs still has to cite constants that
    /// exist and are of the kind JVMS §4.4 requires.
    #[test]
    fn every_constant_reference_is_checked() {
        let utf8 = |text: &str| {
            let mut bytes = vec![1, 0, text.len() as u8];
            bytes.extend_from_slice(text.as_bytes());
            bytes
        };
        let pool = |entries: &[Vec<u8>]| {
            let mut bytes = vec![0, entries.len() as u8 + 1];
            entries
                .iter()
                .for_each(|entry| bytes.extend_from_slice(entry));
            read_pool(&mut Reader {
                bytes: &bytes,
                at: 0,
            })
            .map(|_| ())
        };
        // A String naming #1, a UTF-8 constant: well formed.
        assert_eq!(pool(&[utf8("a"), vec![8, 0, 1]]), Ok(()));
        // A String naming a constant that does not exist.
        assert_eq!(
            pool(&[utf8("a"), vec![8, 0, 9]]),
            Err(Refusal::ConstantIndex)
        );
        // A String naming a String, not a UTF-8 constant.
        assert_eq!(
            pool(&[utf8("a"), vec![8, 0, 3], vec![8, 0, 1]]),
            Err(Refusal::ConstantIndex)
        );
        // A field reference whose class is a UTF-8 constant.
        assert_eq!(
            pool(&[utf8("a"), vec![12, 0, 1, 0, 1], vec![9, 0, 1, 0, 2]]),
            Err(Refusal::ConstantIndex)
        );
        // A method handle naming a name-and-type, not a member.
        assert_eq!(
            pool(&[utf8("a"), vec![12, 0, 1, 0, 1], vec![15, 5, 0, 2]]),
            Err(Refusal::ConstantIndex)
        );
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
