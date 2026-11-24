//! Compact carrier for freshly minted receipts (ADR-0046 decision 7, slice 2,
//! #725). A [`Receipt`] repeats its file's identity in every range; a
//! directory extraction keeps tens of thousands of them, so it retains this
//! form instead: the per-file identity is shared by every receipt minted for
//! the file, and each range keeps only its role, index and offsets. The full
//! [`Receipt`] is rebuilt on demand, byte for byte, so wire form, identity and
//! validation are unchanged.
use super::{
    CaptureFileRef, CaptureSpanRef, CapturedError, Content, EvidenceRef, Grammar, InputClosure,
    RECEIPT_SCHEMA_VERSION, RangeRole, Receipt, ReceiptRange, SourceScope, contract, inventory,
};
use core_graph::source::{FactKey, edge_digest, node_digest};
use core_graph::{Edge, Node};
use std::mem::size_of;
use std::sync::Arc;

/// Identity shared by every receipt minted for one captured file. The other
/// header fields are fixed by the current contract and the file's path.
#[derive(Debug, PartialEq, Eq)]
pub struct MintedHeader {
    repo_key: String,
    file: CaptureFileRef,
    commit_sha: String,
}

impl MintedHeader {
    /// Retained bytes of one shared header: the `Arc` allocation (control
    /// block and value) plus every owned string. The source id is counted at
    /// its length, as its capacity is not exposed.
    pub fn retained_bytes(&self) -> usize {
        2 * size_of::<usize>()
            + size_of::<Self>()
            + self.repo_key.capacity()
            + self.commit_sha.capacity()
            + self.file.source_id.as_str().len()
            + self.file.capture_id.capacity()
            + self.file.path.capacity()
            + self.file.digest.capacity()
    }
}

/// One range of a minted receipt: role, index, the shared offsets and the
/// cited file, `0` for the header's file and `n` for `other_files[n - 1]`.
type Span = (RangeRole, u32, u64, u64, u32);

/// A freshly minted v3 receipt, or one under contract v4 with its closure
/// state, in compact form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedReceipt {
    receipt_id: String,
    header: Arc<MintedHeader>,
    fact_key: FactKey,
    fact_digest: String,
    spans: Box<[Span]>,
    /// Further captured files the spans cite (AC-1039), in path order.
    other_files: Box<[CaptureFileRef]>,
    source_sizes: Option<super::ReceiptSourceSizes>,
    /// The closure state of a contract-v4 receipt; `None` under v3.
    closure: Option<super::ReceiptClosure>,
    /// The record an established closure state names, shared by every
    /// receipt of its file, so the host retains it with them (AC-1084).
    input_closure: Option<Arc<super::super::InputClosureV1>>,
}

impl MintedReceipt {
    /// Compact a receipt minted under the current contract, sharing `header`
    /// when it already holds this file's identity. Every range must be
    /// derivable from the header, which `validate()` already requires of v3.
    pub(in crate::captured) fn compact(
        receipt: Receipt,
        header: &mut Option<Arc<MintedHeader>>,
    ) -> Result<Self, CapturedError> {
        let content = receipt.content;
        let current = contract(RECEIPT_SCHEMA_VERSION, content.grammar)?;
        let commit_sha = match (content.schema_version, content.commit_sha) {
            (RECEIPT_SCHEMA_VERSION, Some(commit_sha)) => commit_sha,
            _ => return Err(CapturedError::Invalid("minted receipt contract")),
        };
        if content.source_id != content.file.source_id
            || content.producer_contract != current.producer
            || content.grammar != Grammar::for_file(&content.file)
            || content.grammar_package != current.grammar_package
            || content.parser_package != current.parser_package
        {
            return Err(CapturedError::Invalid("minted receipt contract"));
        }
        let slot = |file: &CaptureFileRef| {
            if *file == content.file {
                Some(0)
            } else {
                content
                    .other_files
                    .iter()
                    .position(|other| other == file)
                    .and_then(|index| u32::try_from(index + 1).ok())
            }
        };
        if content.ranges.iter().any(|range| {
            range.evidence.repo != content.repo_key
                || range.evidence.path != range.captured.file.path
                || range.evidence.commit_sha != commit_sha
                || slot(&range.captured.file).is_none()
                || range.evidence.byte_start != range.captured.byte_start
                || range.evidence.byte_end != range.captured.byte_end
        }) {
            return Err(CapturedError::Invalid("minted receipt range"));
        }
        let spans = content
            .ranges
            .iter()
            .map(|range| {
                (
                    range.role,
                    range.index,
                    range.evidence.byte_start,
                    range.evidence.byte_end,
                    slot(&range.captured.file).unwrap_or(0),
                )
            })
            .collect();
        let shared = match header {
            Some(shared)
                if shared.repo_key == content.repo_key
                    && shared.file == content.file
                    && shared.commit_sha == commit_sha =>
            {
                Arc::clone(shared)
            }
            _ => {
                let shared = Arc::new(MintedHeader {
                    repo_key: content.repo_key,
                    file: content.file,
                    commit_sha,
                });
                *header = Some(Arc::clone(&shared));
                shared
            }
        };
        Ok(Self {
            source_sizes: content.source_sizes.clone(),
            receipt_id: receipt.receipt_id,
            header: shared,
            fact_key: content.fact_key,
            fact_digest: content.fact_digest,
            spans,
            other_files: content.other_files.into_boxed_slice(),
            closure: None,
            input_closure: None,
        })
    }

    /// This freshly minted v3 receipt under contract v4 with `closure`, as
    /// [`Receipt::with_closure`]: same fact and spans, a new identity. An
    /// established state carries exactly the record it names.
    pub(in crate::captured) fn with_closure(
        mut self,
        closure: super::ReceiptClosure,
        record: Option<Arc<super::super::InputClosureV1>>,
    ) -> Result<Self, CapturedError> {
        let named = match &closure {
            super::ReceiptClosure::Established { closure_id } => Some(closure_id),
            super::ReceiptClosure::InputClosureNotEstablished { .. } => None,
        };
        if self.closure.is_some() || named != record.as_ref().map(|record| &record.closure_id) {
            return Err(CapturedError::Invalid("receipt input closure"));
        }
        self.receipt_id = self.to_receipt()?.with_closure(closure.clone())?.receipt_id;
        self.closure = Some(closure);
        self.input_closure = record;
        Ok(self)
    }

    /// The input closure record this receipt's established state names.
    pub fn input_closure(&self) -> Option<&super::super::InputClosureV1> {
        self.input_closure.as_deref()
    }

    /// The captured file a span's slot names.
    fn slot_file(&self, slot: u32) -> &CaptureFileRef {
        match slot {
            0 => &self.header.file,
            n => &self.other_files[n as usize - 1],
        }
    }

    /// The complete receipt this carrier stands for.
    pub fn to_receipt(&self) -> Result<Receipt, CapturedError> {
        let header = &*self.header;
        let (schema_version, input_closure) = match &self.closure {
            None => (RECEIPT_SCHEMA_VERSION, InputClosure::LEGACY),
            Some(closure) => (4, InputClosure::Closure(closure.clone())),
        };
        let current = contract(schema_version, Grammar::for_file(&header.file))?;
        Ok(Receipt {
            receipt_id: self.receipt_id.clone(),
            content: Content {
                schema_version,
                source_id: header.file.source_id.clone(),
                repo_key: header.repo_key.clone(),
                file: header.file.clone(),
                commit_sha: Some(header.commit_sha.clone()),
                producer_contract: current.producer.into(),
                grammar: Grammar::for_file(&header.file),
                grammar_package: current.grammar_package.into(),
                parser_package: current.parser_package.into(),
                fact_key: self.fact_key.clone(),
                fact_digest: self.fact_digest.clone(),
                ranges: self
                    .spans
                    .iter()
                    .map(|&(role, index, byte_start, byte_end, slot)| {
                        let file = self.slot_file(slot);
                        ReceiptRange {
                            role,
                            index,
                            evidence: EvidenceRef {
                                repo: header.repo_key.clone(),
                                path: file.path.clone(),
                                byte_start,
                                byte_end,
                                commit_sha: header.commit_sha.clone(),
                            },
                            captured: CaptureSpanRef {
                                file: file.clone(),
                                byte_start,
                                byte_end,
                            },
                        }
                    })
                    .collect(),
                other_files: self.other_files.to_vec(),
                source_sizes: self.source_sizes.clone(),
                source_scope: SourceScope::PrimarySourceOnly,
                input_closure,
            },
        })
    }

    /// The closure state of a contract-v4 receipt; `None` under v3.
    pub fn closure(&self) -> Option<&super::ReceiptClosure> {
        self.closure.as_ref()
    }

    /// Serialize the complete receipt, as [`Receipt::to_json`] would.
    pub fn to_json(&self) -> Result<String, CapturedError> {
        self.to_receipt()?.to_json()
    }

    /// Immutable receipt identity.
    pub fn id(&self) -> &str {
        &self.receipt_id
    }
    /// Registered logical source.
    pub fn source_id(&self) -> &source_capture::SourceId {
        &self.header.file.source_id
    }
    /// Exact registered graph namespace.
    pub fn repo_key(&self) -> &str {
        &self.header.repo_key
    }
    /// Full primary-file capture binding.
    pub fn file(&self) -> &CaptureFileRef {
        &self.header.file
    }
    /// Typed node or directed-edge identity.
    pub fn fact_key(&self) -> &FactKey {
        &self.fact_key
    }
    /// Canonical digest of the complete original emitted fact.
    pub fn fact_digest(&self) -> &str {
        &self.fact_digest
    }
    /// The shared per-file header, for once-per-header accounting.
    pub fn header(&self) -> &Arc<MintedHeader> {
        &self.header
    }
    /// Each cited span of the captured files, built one at a time.
    pub fn captured_spans(&self) -> impl Iterator<Item = CaptureSpanRef> + '_ {
        self.spans
            .iter()
            .map(|&(_, _, byte_start, byte_end, slot)| CaptureSpanRef {
                file: self.slot_file(slot).clone(),
                byte_start,
                byte_end,
            })
    }

    /// Retained bytes of this receipt, excluding its shared header: the
    /// value itself plus its id, fact-key strings, digest and spans.
    pub fn retained_bytes(&self) -> usize {
        let key = match &self.fact_key {
            FactKey::Node { id } => id.capacity(),
            FactKey::Edge {
                source,
                label,
                destination,
            } => source.capacity() + label.capacity() + destination.capacity(),
        };
        size_of::<Self>()
            + self.receipt_id.capacity()
            + key
            + self.fact_digest.capacity()
            + self.spans.len() * size_of::<Span>()
            + self
                .other_files
                .iter()
                .map(|file| {
                    size_of::<CaptureFileRef>()
                        + file.source_id.as_str().len()
                        + file.capture_id.capacity()
                        + file.path.capacity()
                        + file.digest.capacity()
                })
                .sum::<usize>()
            + self
                .source_sizes
                .as_ref()
                .map_or(0, |sizes| sizes.heap_bytes())
            + match &self.closure {
                None => 0,
                Some(super::ReceiptClosure::Established { closure_id }) => closure_id.capacity(),
                Some(super::ReceiptClosure::InputClosureNotEstablished { reasons }) => {
                    reasons.capacity() * size_of::<super::super::ClosureGapReason>()
                }
            }
    }

    /// The closure record this receipt shares with the other receipts of its
    /// file, for once-per-record accounting.
    pub(in crate::captured) fn shared_input_closure(
        &self,
    ) -> Option<&Arc<super::super::InputClosureV1>> {
        self.input_closure.as_ref()
    }

    fn matches_inventory(&self, props: &serde_json::Value) -> bool {
        let header = &*self.header;
        inventory(
            props,
            RECEIPT_SCHEMA_VERSION,
            Grammar::for_file(&self.header.file),
        )
        .is_ok_and(|ranges| {
            ranges.len() == self.spans.len()
                && ranges.iter().zip(self.spans.iter()).all(
                    |((role, index, evidence), &(span_role, span_index, start, end, slot))| {
                        *role == span_role
                            && *index == span_index
                            && evidence.repo == header.repo_key
                            && evidence.path == self.slot_file(slot).path
                            && evidence.commit_sha == header.commit_sha
                            && evidence.byte_start == start
                            && evidence.byte_end == end
                    },
                )
        })
    }

    /// As [`Receipt::matches_node`].
    pub fn matches_node(&self, node: &Node) -> bool {
        matches!(&self.fact_key, FactKey::Node { id } if *id == node.id)
            && node_digest(node).is_ok_and(|digest| digest == self.fact_digest)
            && self.matches_inventory(&node.props)
    }

    /// As [`Receipt::matches_edge`].
    pub fn matches_edge(&self, edge: &Edge) -> bool {
        matches!(&self.fact_key, FactKey::Edge { source, label, destination }
            if *source == edge.src && *label == edge.label && *destination == edge.dst)
            && edge_digest(edge).is_ok_and(|digest| digest == self.fact_digest)
            && self.matches_inventory(&edge.props)
    }
}

/// A receipt in either form, as a host checks, persists and binds it.
pub trait Citable {
    /// Immutable receipt identity.
    fn id(&self) -> &str;
    /// Registered logical source.
    fn source_id(&self) -> &source_capture::SourceId;
    /// Exact registered graph namespace.
    fn repo_key(&self) -> &str;
    /// Full primary-file capture binding.
    fn file(&self) -> &CaptureFileRef;
    /// Typed node or directed-edge identity.
    fn fact_key(&self) -> &FactKey;
    /// Canonical digest of the complete original emitted fact.
    fn fact_digest(&self) -> &str;
    /// Each cited span of the captured file.
    fn captured_spans(&self) -> Box<dyn Iterator<Item = CaptureSpanRef> + '_>;
    /// Canonical receipt JSON.
    fn to_json(&self) -> Result<String, CapturedError>;
    /// The input closure state of a contract-v4 receipt; `None` before v4.
    fn closure(&self) -> Option<&super::ReceiptClosure>;
    /// The record an established closure state names, when the producer
    /// handed it over with the receipt; a host retains it with the receipt.
    fn input_closure(&self) -> Option<&super::super::InputClosureV1>;
    /// Full content/range match against a node.
    fn matches_node(&self, node: &Node) -> bool;
    /// Full content/range match against an edge.
    fn matches_edge(&self, edge: &Edge) -> bool;
}

impl Citable for Receipt {
    fn id(&self) -> &str {
        Receipt::id(self)
    }
    fn source_id(&self) -> &source_capture::SourceId {
        Receipt::source_id(self)
    }
    fn repo_key(&self) -> &str {
        Receipt::repo_key(self)
    }
    fn file(&self) -> &CaptureFileRef {
        Receipt::file(self)
    }
    fn fact_key(&self) -> &FactKey {
        Receipt::fact_key(self)
    }
    fn fact_digest(&self) -> &str {
        Receipt::fact_digest(self)
    }
    fn captured_spans(&self) -> Box<dyn Iterator<Item = CaptureSpanRef> + '_> {
        Box::new(self.ranges().iter().map(|range| range.captured.clone()))
    }
    fn to_json(&self) -> Result<String, CapturedError> {
        Receipt::to_json(self)
    }
    fn closure(&self) -> Option<&super::ReceiptClosure> {
        Receipt::closure(self)
    }
    /// A full receipt carries no record; its host supplies it.
    fn input_closure(&self) -> Option<&super::super::InputClosureV1> {
        None
    }
    fn matches_node(&self, node: &Node) -> bool {
        Receipt::matches_node(self, node)
    }
    fn matches_edge(&self, edge: &Edge) -> bool {
        Receipt::matches_edge(self, edge)
    }
}

impl Citable for MintedReceipt {
    fn id(&self) -> &str {
        MintedReceipt::id(self)
    }
    fn source_id(&self) -> &source_capture::SourceId {
        MintedReceipt::source_id(self)
    }
    fn repo_key(&self) -> &str {
        MintedReceipt::repo_key(self)
    }
    fn file(&self) -> &CaptureFileRef {
        MintedReceipt::file(self)
    }
    fn fact_key(&self) -> &FactKey {
        MintedReceipt::fact_key(self)
    }
    fn fact_digest(&self) -> &str {
        MintedReceipt::fact_digest(self)
    }
    fn captured_spans(&self) -> Box<dyn Iterator<Item = CaptureSpanRef> + '_> {
        Box::new(MintedReceipt::captured_spans(self))
    }
    fn to_json(&self) -> Result<String, CapturedError> {
        MintedReceipt::to_json(self)
    }
    fn closure(&self) -> Option<&super::ReceiptClosure> {
        MintedReceipt::closure(self)
    }
    fn input_closure(&self) -> Option<&super::super::InputClosureV1> {
        MintedReceipt::input_closure(self)
    }
    fn matches_node(&self, node: &Node) -> bool {
        MintedReceipt::matches_node(self, node)
    }
    fn matches_edge(&self, edge: &Edge) -> bool {
        MintedReceipt::matches_edge(self, edge)
    }
}
