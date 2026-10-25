//! Shared capture-store read owners. Callers retain their receipt, target and
//! guards separately; this is logical payload accounting, not allocator/RSS.
use crate::mcp_reads::dispatch::EvidenceWorkLimits;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

pub(crate) struct CaptureWorkspace {
    pub manifest_peak: usize,
    pub read_peak: usize,
}
impl CaptureWorkspace {
    pub fn peak(&self) -> usize {
        self.manifest_peak.max(self.read_peak)
    }
}

/// `reference_payload` bounds the borrowed file reference's strings, which the
/// store copies into its independently derived expected reference. For tasks,
/// that reference belongs to an already validated inspection target.
pub(crate) fn workspace(
    limits: EvidenceWorkLimits,
    reference_payload: usize,
) -> Option<CaptureWorkspace> {
    let manifest = usize::try_from(source_capture::MAX_MANIFEST_BYTES).ok()?;
    // Decode precedes file-count validation. Malformed entries consume at least
    // one raw byte each; unescaped string payload cannot exceed the raw input.
    let typed = sum(&[
        manifest,
        size_of::<source_capture::CaptureManifest>(),
        slots::<source_capture::FileEntry>(manifest.checked_add(1)?)?,
    ])?;
    // SQL typeof strings and manifest/Git identity strings are bounded before
    // returning. Count their headers and the raw/canonical or object/span Vecs.
    let metadata = sum(&[
        slots::<String>(3)?,
        slots::<Vec<u8>>(2)?,
        7,
        7,
        "capture-v1:".len(),
        64,
        40,
    ])?;
    // CaptureKind is internally tagged: serde can retain a buffered Content
    // tree before refusing an unknown kind or malformed nested value. Charge
    // that independently of decoded file headers and the string scratch.
    // Canonical serialization first counts/refuses before allocating its bytes.
    let manifest_peak = sum(&[manifest, typed, manifest, json_bytes(manifest)?, metadata])?;
    let object = usize::try_from(
        limits
            .max_file_validation_bytes
            .get()
            .min(source_capture::MAX_FILE_BYTES),
    )
    .ok()?;
    let span = usize::try_from(
        limits
            .max_span_bytes
            .get()
            .min(source_capture::MAX_SPAN_BYTES),
    )
    .ok()?;
    // read_bound_span_file retains manifest/expected reference with the object;
    // read_span then copies the span while retaining the object. Combining those
    // sequential phases is conservative. UTF-8 conversion moves the span Vec.
    // The expected reference is built before equality refusal. A mismatching
    // manifest's source/digest can differ from the borrowed reference, so never
    // use a smaller caller payload in place of the producer's field ceilings.
    let expected_reference = reference_payload.max(sum(&[
        source_capture::MAX_SOURCE_ID_BYTES,
        source_capture::MAX_PATH_BYTES,
        "capture-v1:".len(),
        64,
        64,
    ])?);
    let read_peak = sum(&[
        metadata,
        typed,
        object,
        span,
        size_of::<source_capture::CaptureFileRef>(),
        expected_reference,
    ])?;
    Some(CaptureWorkspace {
        manifest_peak,
        read_peak,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn limits(file: u64, span: u64) -> EvidenceWorkLimits {
        EvidenceWorkLimits {
            max_file_validation_bytes: NonZeroU64::new(file).unwrap(),
            max_span_bytes: NonZeroU64::new(span).unwrap(),
        }
    }

    #[test]
    fn capture_read_roles_grow_before_phase_max_and_stop_at_hard_caps() {
        // AC-0327: a larger manifest phase must not hide missing file/span roles.
        let base = workspace(limits(1, 1), 8192).unwrap();
        let larger = workspace(limits(128, 64), 8192).unwrap();
        assert_eq!(larger.read_peak - base.read_peak, 127 + 63);
        assert_eq!(larger.manifest_peak, base.manifest_peak);
        assert_eq!(
            workspace(limits(1, 1), 8208).unwrap().read_peak - base.read_peak,
            16
        );
        let caps = workspace(
            limits(
                source_capture::MAX_FILE_BYTES,
                source_capture::MAX_SPAN_BYTES,
            ),
            16,
        )
        .unwrap();
        let oversized = workspace(limits(u64::MAX, u64::MAX), 16).unwrap();
        assert_eq!(caps.read_peak, oversized.read_peak);
        assert_eq!(caps.peak(), oversized.peak());
        assert!(workspace(limits(1, 1), usize::MAX).is_none());
    }

    #[test]
    fn capture_workspace_covers_real_bounded_utf8_read_and_cap_refusals() {
        // AC-0327: same CaptureStore API used by the detached task host.
        let root = tempfile::tempdir().unwrap();
        let bytes = "café".as_bytes();
        std::fs::write(root.path().join("source"), bytes).unwrap();
        let capture = source_capture::capture_working_tree(
            root.path(),
            &source_capture::SourceId::new("synthetic").unwrap(),
            &["source".into()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let reference = capture.file("source").unwrap().span(0, 5).unwrap();
        let mut store =
            source_capture::CaptureStore::in_memory(source_capture::StoreLimits::default())
                .unwrap();
        store.persist(&capture).unwrap();
        let bound = workspace(
            limits(5, 5),
            serde_json::to_vec(&reference.file).unwrap().len(),
        )
        .unwrap();
        let text = store.read_text_span_bounded(&reference, 5, 5).unwrap();
        assert_eq!(text, "café");
        let manifest = serde_json::to_vec(capture.manifest()).unwrap();
        assert!(
            bound.manifest_peak
                >= manifest.len() * 2 + size_of::<source_capture::CaptureManifest>()
        );
        assert!(bound.read_peak >= manifest.len() + bytes.len() + text.len());
        assert!(store.read_text_span_bounded(&reference, 4, 5).is_err());
        assert!(store.read_text_span_bounded(&reference, 5, 4).is_err());
        assert_eq!(
            store.read_text_span_bounded(&reference, 5, 5).unwrap(),
            text
        );
    }
    #[test]
    fn capture_manifest_unknown_kind_is_rejected_through_bounded_store_decode() {
        // AC-0327: exercise the tagged decoder, not an invalid manifest hash.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("captures.sqlite");
        let store =
            source_capture::CaptureStore::open(&path, source_capture::StoreLimits::default())
                .unwrap();
        let mut nested = serde_json::json!(["buffered value"]);
        for _ in 0..32 {
            nested = serde_json::json!([nested]);
        }
        let raw = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "source_id": "synthetic",
            "kind": {"nested": nested, "kind": "unknown"}, "files": [],
        }))
        .unwrap();
        let id = format!("capture-v1:{}", core_prov::content_hash(&raw));
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO captures(id,manifest,max_span_bytes) VALUES (?1,?2,5)",
                rusqlite::params![id, raw],
            )
            .unwrap();
        assert!(matches!(
            store.capture_belongs_to_source(
                &source_capture::SourceId::new("synthetic").unwrap(),
                &id
            ),
            Err(source_capture::CaptureError::Corrupt(
                "invalid stored manifest encoding"
            )),
        ));
    }
}
