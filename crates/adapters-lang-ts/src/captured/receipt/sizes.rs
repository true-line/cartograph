//! Optional capture-time measurements, committed by the enclosing receipt ID.
use super::*;

/// Size metadata is a bound claim, not proof of producer trust. The charged
/// read must independently verify the object and check both actual sizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSourceSizes {
    schema_version: u32,
    redactor: String,
    object_digest: String,
    ranges: Vec<RangeSizes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeSizes {
    byte_start: u64,
    byte_end: u64,
    original: u64,
    redacted: u64,
}

pub(super) fn present<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ReceiptSourceSizes>, D::Error> {
    ReceiptSourceSizes::deserialize(deserializer).map(Some)
}

impl ReceiptSourceSizes {
    pub(super) fn validate(&self, content: &Content) -> Result<(), CapturedError> {
        if !super::compact_version(content.schema_version)
            || self.schema_version != 1
            || self.object_digest != content.file.digest
            || self.redactor.is_empty()
            || self.redactor.len() > 64
            || self.ranges.len() != content.ranges.len()
        {
            return Err(CapturedError::Invalid("receipt source sizes"));
        }
        for (size, range) in self.ranges.iter().zip(&content.ranges) {
            let bound = range
                .captured
                .byte_end
                .checked_sub(range.captured.byte_start)
                .and_then(|len| len.checked_mul(6))
                .ok_or(CapturedError::Invalid("receipt size range"))?;
            if size.byte_start != range.captured.byte_start
                || size.byte_end != range.captured.byte_end
                || size.original > bound
                || size.redacted > bound
            {
                return Err(CapturedError::Invalid("receipt size range"));
            }
        }
        Ok(())
    }

    pub(super) fn heap_bytes(&self) -> usize {
        self.redactor.capacity()
            + self.object_digest.capacity()
            + self.ranges.capacity() * std::mem::size_of::<RangeSizes>()
    }
}

impl Receipt {
    /// Attach measurements only while the producer owns the captured file.
    /// Returns a new content identity; callers must publish that identity in
    /// the graph source binding. Never retrofit an already-published receipt.
    /// Unmeasurable text or an oversized enriched receipt retains the original
    /// receipt, whose missing measurements require conservative admission.
    pub fn with_source_sizes(self, file: &CapturedFile) -> Result<Self, CapturedError> {
        self.validate()?;
        if !super::compact_version(self.content.schema_version)
            || file.reference() != &self.content.file
        {
            return Err(CapturedError::Invalid("receipt size file binding"));
        }
        let ranges = self
            .content
            .ranges
            .iter()
            .map(|range| {
                let start = usize::try_from(range.captured.byte_start)
                    .map_err(|_| CapturedError::Invalid("receipt size offset"))?;
                let end = usize::try_from(range.captured.byte_end)
                    .map_err(|_| CapturedError::Invalid("receipt size offset"))?;
                let text = file
                    .bytes()
                    .get(start..end)
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .ok_or(CapturedError::Invalid("receipt size text"))?;
                let sizes = core_redact::measure_direct_source_json_content(text)
                    .ok_or(CapturedError::Invalid("receipt size measurement"))?;
                Ok(RangeSizes {
                    byte_start: range.captured.byte_start,
                    byte_end: range.captured.byte_end,
                    original: sizes.original as u64,
                    redacted: sizes.redacted as u64,
                })
            })
            .collect::<Result<Vec<_>, CapturedError>>();
        let Ok(ranges) = ranges else {
            // Optional sizing cannot remove an otherwise valid citation.
            return Ok(self);
        };
        let sizes = ReceiptSourceSizes {
            schema_version: 1,
            redactor: core_redact::TEXT_POLICY_VERSION.into(),
            object_digest: self.content.file.digest.clone(),
            ranges,
        };
        Ok(self.install_source_sizes(sizes, MAX_RECEIPT_BYTES))
    }

    // Transactional optional enrichment: retain the original value on failure,
    // without cloning the complete receipt/range inventory. Tests narrow the
    // admission cap; production always passes the existing receipt ceiling.
    fn install_source_sizes(mut self, sizes: ReceiptSourceSizes, max_bytes: usize) -> Self {
        let previous = self.content.source_sizes.replace(sizes);
        let previous_id = std::mem::take(&mut self.receipt_id);
        let accepted = content_id(&self.content).is_ok_and(|id| {
            self.receipt_id = id;
            self.to_json().is_ok_and(|json| json.len() <= max_bytes)
        });
        if !accepted {
            self.content.source_sizes = previous;
            self.receipt_id = previous_id;
        }
        self
    }

    /// Check a claimed size against already-authorized, digest-verified text.
    /// This does not verify the object digest or authorize reading the text.
    /// Missing or unsupported metadata leaves conservative admission unchanged.
    pub fn source_sizes_fit(&self, index: usize, text: &str) -> Result<bool, CapturedError> {
        let reserved = self.source_sizes(index)?;
        let range = self
            .content
            .ranges
            .get(index)
            .ok_or(CapturedError::Invalid("receipt size index"))?;
        if text.len() as u64 != range.captured.byte_end - range.captured.byte_start {
            return Ok(false);
        }
        Ok(reserved.is_none_or(|reserved| {
            core_redact::measure_direct_source_json_content(text).is_some_and(|actual| {
                actual.original <= reserved.original && actual.redacted <= reserved.redacted
            })
        }))
    }

    /// Return a supported metadata claim after receipt validation. This grants
    /// no source-read authority; old captures and unknown redactors return None.
    pub fn source_sizes(
        &self,
        index: usize,
    ) -> Result<Option<core_redact::DirectSourceJsonContentSizes>, CapturedError> {
        self.validate()?;
        let Some(sizes) = &self.content.source_sizes else {
            return Ok(None);
        };
        if sizes.redactor != core_redact::TEXT_POLICY_VERSION {
            return Ok(None);
        }
        let size = sizes
            .ranges
            .get(index)
            .ok_or(CapturedError::Invalid("receipt size index"))?;
        Ok(Some(core_redact::DirectSourceJsonContentSizes {
            original: usize::try_from(size.original)
                .map_err(|_| CapturedError::Invalid("receipt size overflow"))?,
            redacted: usize::try_from(size.redacted)
                .map_err(|_| CapturedError::Invalid("receipt size overflow"))?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured() -> (Receipt, Receipt) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("source.ts"),
            b"function run(){ if (ready) return false; }",
        )
        .unwrap();
        let source = source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source,
            &["source.ts".into()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let file = capture.file("source.ts").unwrap();
        let (_, receipts) = crate::captured::extract_file(
            file,
            &SourceId {
                repo: "acme/project",
                commit: "workdir",
            },
        )
        .unwrap();
        let old = receipts.into_iter().next().unwrap();
        let new = old.clone().with_source_sizes(file).unwrap();
        (old, new)
    }

    // AC-0287: optional measurements change identity without rewriting old wire.
    #[test]
    fn measured_receipt_roundtrips_and_compacts_without_changing_legacy() {
        let (old, new) = measured();
        assert_ne!(old.id(), new.id());
        assert_eq!(old.source_sizes(0).unwrap(), None);
        assert!(!old.to_json().unwrap().contains("source_sizes"));
        assert!(new.source_sizes(0).unwrap().is_some());
        let json = new.to_json().unwrap();
        let decoded = Receipt::from_json(&json).unwrap();
        assert_eq!(decoded, new);
        let compact = MintedReceipt::compact(new.clone(), &mut None).unwrap();
        assert_eq!(compact.to_receipt().unwrap(), new);
        assert_eq!(compact.to_json().unwrap(), json);
        assert_eq!(Receipt::from_json(&old.to_json().unwrap()).unwrap(), old);
    }

    // AC-0287: optional metadata cannot discard a valid bounded receipt.
    #[test]
    fn optional_size_enrichment_preserves_original_when_it_does_not_fit() {
        let (old, measured) = measured();
        let cap = old.to_json().unwrap().len();
        assert!(measured.to_json().unwrap().len() > cap);
        let sizes = measured.content.source_sizes.unwrap();
        let fallback = old.clone().install_source_sizes(sizes, cap);
        assert_eq!(fallback, old);
        assert_eq!(fallback.to_json().unwrap(), old.to_json().unwrap());
        assert_eq!(fallback.source_sizes(0).unwrap(), None);
    }

    // AC-0287: receipt hashes bind the claim; policy/range checks remain separate.
    #[test]
    fn measured_receipt_rejects_tampering_and_falls_back_for_unknown_redactor() {
        let (_, new) = measured();
        let mut changed = new.clone();
        changed.content.source_sizes.as_mut().unwrap().ranges[0].original = 0;
        assert!(changed.validate().is_err());
        // A rewritten content-consistent claim is NOT semantic truth. Host
        // post-read verification must reject this before installing evidence.
        changed.receipt_id = content_id(&changed.content).unwrap();
        changed.validate().unwrap();
        assert_eq!(changed.source_sizes(0).unwrap().unwrap().original, 0);
        let range = &changed.content.ranges[0].captured;
        let code = "function run(){ if (ready) return false; }";
        assert!(
            !changed
                .source_sizes_fit(0, &code[range.byte_start as usize..range.byte_end as usize])
                .unwrap()
        );
        let mut unknown = new.clone();
        unknown.content.source_sizes.as_mut().unwrap().redactor = "redact-text@future".into();
        unknown.receipt_id = content_id(&unknown.content).unwrap();
        assert_eq!(unknown.source_sizes(0).unwrap(), None);
        let mut wrong_range = new;
        wrong_range.content.source_sizes.as_mut().unwrap().ranges[0].byte_end += 1;
        wrong_range.receipt_id = content_id(&wrong_range.content).unwrap();
        assert!(wrong_range.validate().is_err());
    }
}
