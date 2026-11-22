//! Fixed protocol construction can precede the W-byte response check. Count
//! production builders rather than assuming W admits their metadata schemas.
use super::*;
use crate::mcp_reads::{ReadBasis, worker::WorkerLimits};
use context_hub::logical::{count, json_bytes, slots, string_wire, sum};

pub(crate) fn logical_protocol_workspace_bytes(limits: WorkerLimits) -> Option<usize> {
    let hash = core_prov::content_hash(&[]);
    let basis = ReadBasis::reservation_fixture();
    let basis_id = format!("mcp-read-basis-v1:{hash}");
    let tools = [
        ReadTool::Capabilities,
        ReadTool::Discover,
        ReadTool::Query,
        ReadTool::Artifacts,
        ReadTool::ReadArtifact,
        ReadTool::Evidence,
    ];
    let mut admission = Admission::new(tools, limits.dispatch.max_request_ids);
    let Action::Respond(initialization) = admission.receive(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"x","capabilities":{},"clientInfo":{"name":"x","version":"x"}}})) else { return None; };
    let initialization_peak = json_bytes(count(&initialization)?)?;
    let descriptors: Vec<_> = tools.iter().copied().map(tool_descriptor).collect();
    let list = json!({"jsonrpc":"2.0","id":null,"result":{"tools":descriptors}});
    let capabilities = success(
        &Value::Null,
        "Context capabilities.",
        capabilities_value(
            "",
            &basis_id,
            &basis,
            &tools,
            limits.graph,
            limits.dispatch,
            Some(limits.evidence),
        ),
    );
    let catalogue = spec::artifact_catalog();
    let artifacts = success(
        &Value::Null,
        "Official artifact catalog.",
        json!({"session_id":"","basis_id":basis_id,"artifacts":catalogue}),
    );
    // Descriptor input-schema DOM -> descriptor Vec -> enclosing response DOM.
    // All three role sets use the complete list's byte-derived structural cap,
    // so nested schema construction is covered even for a one-byte W policy.
    let schema = json_bytes(count(&list)?)?;
    let list_peak = sum(&[schema, schema, schema, slots::<Value>(tools.len())?])?;
    // capabilities_value's data DOM is copied into the success envelope; basis
    // and implemented-tool temporary vectors remain live during construction.
    let caps = json_bytes(count(&capabilities)?)?;
    let capabilities_peak = sum(&[caps, caps, count(&basis)?, slots::<&str>(tools.len())?])?;
    let artifacts_dom = json_bytes(count(&artifacts)?)?;
    let catalogue_peak = sum(&[
        artifacts_dom,
        artifacts_dom,
        count(&catalogue)?,
        slots::<spec::ArtifactDescriptor>(catalogue.len())?,
    ])?;
    // Error envelopes are constructed before bounded() too. Include every closed
    // host error variant and the largest resulting complete production envelope.
    let errors = [
        ReadError::StaleBasis,
        ReadError::Limit,
        ReadError::AdmissionLimit,
        ReadError::Storage,
        ReadError::ArtifactUnavailable,
        ReadError::EvidenceUnavailable,
        ReadError::EvidenceInvalid,
        ReadError::EvidenceForgotten,
        ReadError::EvidenceOmitted(core_graph::source::ReceiptOmissionKind::Declarations),
        ReadError::EvidenceOmitted(core_graph::source::ReceiptOmissionKind::Rules),
        ReadError::EvidenceOmitted(core_graph::source::ReceiptOmissionKind::StoreCapacity),
        ReadError::InvalidRequest,
    ];
    let mut error_peak = 0;
    for error in errors {
        error_peak = error_peak.max(json_bytes(count(&tool_error(&Value::Null, error))?)?);
        error_peak = error_peak.max(json_bytes(count(&error_response(None, error))?)?);
    }
    // Service activation generates a Capability and exposes its 32 random
    // bytes as 64 hex characters for the session ID (capability.rs). Unlike a
    // client ID this fixed host identifier can exceed a tiny R admission cap.
    let session_copy = json_bytes(string_wire(64)?)?;
    sum(&[
        session_copy,
        list_peak
            .max(capabilities_peak)
            .max(catalogue_peak)
            .max(error_peak)
            .max(initialization_peak),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiny_response_limit_still_reserves_full_protocol_metadata() {
        let mut limits = crate::mcp_reads::worker::tests::limits();
        limits.dispatch.max_response_bytes = NonZeroUsize::new(1).unwrap();
        let tiny = logical_protocol_workspace_bytes(limits).unwrap();
        let full_list = json!({"tools":IMPLEMENTED.into_iter().chain([ReadTool::Evidence]).map(tool_descriptor).collect::<Vec<_>>()});
        assert!(tiny >= json_bytes(count(&full_list).unwrap()).unwrap());
        limits.dispatch.max_response_bytes = NonZeroUsize::new(usize::MAX).unwrap();
        assert!(logical_protocol_workspace_bytes(limits).unwrap() >= tiny);
        limits.dispatch.max_response_bytes = NonZeroUsize::new(1).unwrap();
        limits.max_request_frame_bytes = NonZeroUsize::new(1).unwrap();
        assert_eq!(logical_protocol_workspace_bytes(limits), Some(tiny));
    }
}
