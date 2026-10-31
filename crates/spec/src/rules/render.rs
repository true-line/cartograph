//! Streaming formatting and fallible output measurement for rule inventories.

use core_prov::{ProducingMethod, Provenance};
use serde::{Serialize, Serializer};
use std::fmt::{self, Write};
use std::io;

pub(super) struct Formatted<F>(pub(super) F);
impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Display for Formatted<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(f)
    }
}

pub(super) struct DisplayString<T>(pub(super) T);
impl<T: fmt::Display> Serialize for DisplayString<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

pub(crate) struct Escaped<T>(pub(crate) T);
impl<T: fmt::Display> fmt::Display for Escaped<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            Escape {
                output: f,
                markdown_table: false
            },
            "{}",
            self.0
        )
    }
}

pub(super) struct TableSafe<T>(pub(super) T);
impl<T: fmt::Display> fmt::Display for TableSafe<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            Escape {
                output: f,
                markdown_table: true
            },
            "{}",
            self.0
        )
    }
}

struct Escape<'a, W: fmt::Write + ?Sized> {
    output: &'a mut W,
    markdown_table: bool,
}
impl<W: fmt::Write + ?Sized> fmt::Write for Escape<'_, W> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for character in value.chars() {
            if self.markdown_table {
                match character {
                    '|' => self.output.write_str("\\|")?,
                    '\r' | '\n' => self.output.write_char(' ')?,
                    other => self.output.write_char(other)?,
                }
            } else {
                match character {
                    '&' => self.output.write_str("&amp;")?,
                    '<' => self.output.write_str("&lt;")?,
                    '>' => self.output.write_str("&gt;")?,
                    '\r' => {}
                    '\n' => self.output.write_str(" ⏎ ")?,
                    '\\' | '`' | '*' | '_' | '[' | ']' | '|' | '#' | '!' => {
                        self.output.write_char('\\')?;
                        self.output.write_char(character)?;
                    }
                    other => self.output.write_char(other)?,
                }
            }
        }
        Ok(())
    }
}

pub(super) struct JsonFmt<'a, W: fmt::Write + ?Sized>(pub(super) &'a mut W);
impl<W: fmt::Write + ?Sized> io::Write for JsonFmt<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
        self.0
            .write_str(text)
            .map_err(|_| io::Error::other("rule inventory output limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn json<T: Serialize + ?Sized, W: fmt::Write + ?Sized>(
    output: &mut W,
    value: &T,
) -> fmt::Result {
    serde_json::to_writer(JsonFmt(output), value).map_err(|_| fmt::Error)
}

pub(super) fn escaped_json_string(output: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    json(
        &mut Escape {
            output,
            markdown_table: false,
        },
        value,
    )
}

pub(super) struct Counter {
    pub(super) bytes: usize,
    pub(super) maximum: usize,
}
impl fmt::Write for Counter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let bytes = self.bytes.checked_add(text.len()).ok_or(fmt::Error)?;
        if bytes > self.maximum {
            return Err(fmt::Error);
        }
        self.bytes = bytes;
        Ok(())
    }
}

pub(super) struct NestedHeadings<'a, W: fmt::Write + ?Sized> {
    pub(super) output: &'a mut W,
    pub(super) line_start: bool,
}
impl<W: fmt::Write + ?Sized> fmt::Write for NestedHeadings<'_, W> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for part in text.split_inclusive('\n') {
            if self.line_start && part.starts_with('#') {
                self.output.write_char('#')?;
            }
            self.output.write_str(part)?;
            if !part.is_empty() {
                self.line_start = part.ends_with('\n');
            }
        }
        Ok(())
    }
}

pub(super) struct MethodLabel<'a>(pub(super) &'a ProducingMethod);
impl fmt::Display for MethodLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ProducingMethod::Rule { rule, version } => write!(f, "rule {rule}@{version}"),
            ProducingMethod::Model { model } => write!(f, "model {model}"),
            ProducingMethod::Human { actor } => write!(f, "human {actor}"),
            ProducingMethod::Unrecorded => f.write_str("not recorded"),
        }
    }
}

pub(super) struct EvidenceText<'a>(pub(super) &'a Provenance);
impl fmt::Display for EvidenceText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.evidence.is_empty() {
            return f.write_str("—");
        }
        for (index, evidence) in self.0.evidence.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(
                f,
                "{}:{} bytes {}..{} @ {}",
                evidence.repo,
                evidence.path,
                evidence.byte_start,
                evidence.byte_end,
                evidence.commit_sha
            )?;
        }
        Ok(())
    }
}
