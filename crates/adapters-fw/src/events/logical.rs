//! Module-owned event layering payload currency. No allocator/RSS guarantee.
use super::*;
use std::mem::size_of;
/// Payload roles for a registry observation and its construction overlap.
#[derive(Debug, Clone, Copy)]
pub struct RegistryReservation {
    /// Retained typed registry, identities and rejections.
    pub retained: usize,
    /// Old/new candidate registry and current typed input coexist.
    pub layering_peak: usize,
}
fn sum(xs: &[usize]) -> Option<usize> {
    xs.iter().try_fold(0usize, |a, b| a.checked_add(*b))
}
fn typed(bytes: usize) -> Option<usize> {
    let headers = sum(&[
        size_of::<serde_json::Value>(),
        size_of::<String>(),
        size_of::<usize>().checked_mul(3)?,
    ])?;
    bytes.checked_add(bytes.checked_add(1)?.checked_mul(headers)?)
}
fn identity_bytes(v: &RegistryIdentity) -> Option<usize> {
    sum(&[
        v.name.len(),
        v.schema.len(),
        v.version.len(),
        v.content_hash.len(),
    ])
}
fn builtin_bytes() -> Option<usize> {
    let r = Registries::builtin();
    let mut total = size_of::<Registries>();
    for row in &r.sdks {
        let names = row.pattern.names();
        let key = match &row.identity {
            IdentityArg::First => 0,
            IdentityArg::Key(key) => key.len(),
        };
        total = sum(&[
            total,
            size_of::<EventSdk>(),
            row.kind.len(),
            names
                .iter()
                .try_fold(0usize, |a, b| a.checked_add(b.len()))?,
            key,
            row.select.as_ref().map_or(0, |select| select.first.len()),
            identity_bytes(&row.registry)?,
        ])?;
    }
    for row in &r.qualifiers {
        total = sum(&[
            total,
            size_of::<EventQualifier>(),
            row.kind.len(),
            row.component.len(),
            row.key.len(),
            identity_bytes(&row.registry)?,
        ])?;
    }
    for row in &r.batch_keys {
        total = sum(&[
            total,
            size_of::<BatchKey>(),
            row.kind.len(),
            row.key.len(),
            identity_bytes(&row.registry)?,
        ])?;
    }
    for row in &r.backings {
        total = sum(&[
            total,
            size_of::<ChannelBacking>(),
            row.resource_type.len(),
            row.kind.len(),
            row.attribute.len(),
            identity_bytes(&row.registry)?,
        ])?;
    }
    for row in &r.sources {
        total = sum(&[total, size_of::<RegistryIdentity>(), identity_bytes(row)?])?;
    }
    Some(total)
}
/// Inputs were already bounded by scope's filenames and aggregate file bytes.
/// Each accepted row owns a repeated RegistryIdentity; MAX_ENTRIES is the actual
/// per-file admission bound, independently of the input's byte-derived count.
pub fn registry_reservation(
    extension_bytes: usize,
    metadata_bytes: usize,
    offered_files: usize,
) -> Option<RegistryReservation> {
    let builtin = builtin_bytes()?;
    let loaded = offered_files.min(MAX_EXTENSION_FILES);
    let bytes = extension_bytes.min(loaded.checked_mul(MAX_REGISTRY_BYTES.checked_add(1)?)?);
    let entries = loaded.checked_mul(MAX_ENTRIES)?.min(bytes.checked_add(1)?);
    let row = [
        size_of::<EventSdk>(),
        size_of::<EventQualifier>(),
        size_of::<BatchKey>(),
        size_of::<ChannelBacking>(),
    ]
    .into_iter()
    .max()?;
    let identity = sum(&[
        metadata_bytes,
        SDK_SCHEMA.len().max(BACKING_SCHEMA.len()),
        MAX_VERSION_BYTES,
        core_prov::content_hash(&[]).len(),
    ])?;
    let accepted = sum(&[
        bytes,
        entries.checked_mul(row.checked_add(identity)?)?,
        loaded.checked_mul(size_of::<RegistryIdentity>().checked_add(identity)?)?,
    ])?;
    // A rejection can quote admitted input with Debug escaping. Six is the exact
    // JSON control escape length, with each static diagnostic supplied by this
    // owner module. Input-derived reason content is charged separately.
    let parse_diagnostic = serde_json::from_slice::<RegistryFile>(b"")
        .err()?
        .to_string()
        .len();
    let fixed = [
        format!("more than {MAX_EXTENSION_FILES} extension files"),
        format!("larger than {MAX_REGISTRY_BYTES} bytes"),
        format!("must declare 1..={MAX_ENTRIES} entries"),
        "invalid or duplicate qualifier for".into(),
        "invalid or duplicate batch key for".into(),
        "invalid or duplicate backing for".into(),
        "call shape is already registered".into(),
    ]
    .iter()
    .map(String::len)
    .max()?
    .checked_add(parse_diagnostic)?;
    let reasons = sum(&[
        typed(bytes)?,
        bytes.checked_mul("\\u0000".len())?,
        offered_files.checked_mul(fixed.checked_add(size_of::<RegistryRejection>())?)?,
        metadata_bytes,
    ])?;
    let retained = sum(&[builtin, accepted, reasons])?;
    let layering_peak = sum(&[
        retained,
        builtin,
        accepted,
        typed(bytes.min(MAX_REGISTRY_BYTES))?,
        size_of::<RegistryFile>(),
    ])?;
    Some(RegistryReservation {
        retained,
        layering_peak,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_payload_roles_are_monotone_and_include_embedded_clones() {
        let a = registry_reservation(1000, 1000, 1).unwrap();
        let b = registry_reservation(2000, 2000, 2).unwrap();
        assert!(a.retained >= builtin_bytes().unwrap());
        assert!(a.layering_peak >= a.retained);
        assert!(b.retained >= a.retained && b.layering_peak >= a.layering_peak);
        assert!(registry_reservation(usize::MAX, usize::MAX, usize::MAX).is_none());
    }
}
