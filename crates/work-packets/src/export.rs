//! Deterministic inert Markdown, using the same validated detail as the UI.
use crate::body::BoundedBytes;
use crate::{PacketDetail, PacketError, PacketResult, validate_detail};
use serde::Serialize;
use std::io::Write;

/// Complete seven-group presentation and independent metadata. Source content
/// is rendered inside indented code blocks; it cannot close a fence, introduce
/// Markdown links, activate HTML, or expose an executable action.
pub fn export_markdown(detail: &PacketDetail) -> PacketResult<String> {
    validate_detail(detail)?;
    let mut output = BoundedBytes::new(4 * 1024 * 1024);
    let execution = match detail.execution {
        crate::ExecutionState::Unoffered => "unoffered",
        crate::ExecutionState::Dispatched => "dispatched",
        crate::ExecutionState::Submitted => "submitted",
    };
    write!(
        output,
        "# Local work packet\n\nExecution: {execution}. Capability requests confer no authority.\n\n"
    )
    .map_err(limit)?;
    section(&mut output, "Packet identity", &detail.packet_id)?;
    if let Some(dispatch) = &detail.dispatch {
        section(
            &mut output,
            "Dispatch (local branch; not pushed or merged)",
            dispatch,
        )?;
    }
    section(
        &mut output,
        "Body schema version",
        &detail.body.schema_version,
    )?;
    section(
        &mut output,
        "Lineage (outside reviewed body; additions do not change review revision)",
        &detail.lineage,
    )?;
    section(&mut output, "Review (independent metadata)", &detail.review)?;
    section(
        &mut output,
        "Freshness (scoped observation)",
        &detail.freshness,
    )?;
    section(&mut output, "1. Identity", &detail.body.identity)?;
    section(
        &mut output,
        "2. Intent (requests and claims)",
        &detail.body.intent,
    )?;
    section(&mut output, "3. Evidence", &detail.body.evidence)?;
    section(&mut output, "4. Work (proposals)", &detail.body.work)?;
    section(
        &mut output,
        "5. Acceptance (proposed verification and distinct observations)",
        &detail.body.acceptance,
    )?;
    section(
        &mut output,
        "6. Handoff (requests, never grants)",
        &detail.body.handoff,
    )?;
    section(
        &mut output,
        "7. Return (no worker completion claimed)",
        &detail.body.returns,
    )?;
    // Sections are separated by blank lines; terminate the document with one LF.
    output.bytes.pop();
    String::from_utf8(output.bytes).map_err(|_| PacketError::InvalidInput("export UTF-8"))
}
fn limit(_: std::io::Error) -> PacketError {
    PacketError::LimitExceeded("Markdown export")
}
fn section<T: Serialize>(output: &mut BoundedBytes, title: &str, value: &T) -> PacketResult<()> {
    write!(output, "## {title}\n\n    ").map_err(limit)?;
    // Prefix every line while streaming. Even hostile backticks, raw HTML,
    // images, URLs and Markdown control characters remain code-block text.
    struct Indent<'a>(&'a mut BoundedBytes);
    impl Write for Indent<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for part in bytes.split_inclusive(|b| *b == b'\n') {
                self.0.write_all(part)?;
                if part.last() == Some(&b'\n') {
                    self.0.write_all(b"    ")?;
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer_pretty(Indent(output), value)
        .map_err(|_| PacketError::LimitExceeded("Markdown export"))?;
    output.write_all(b"\n\n").map_err(limit)?;
    Ok(())
}
