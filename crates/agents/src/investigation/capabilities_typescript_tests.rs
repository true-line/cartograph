//! The UI's copy of the capability table is generated from it (ADR-0071
//! slice c, AC-1401), so the client keeps no hand-written version list.
//! Regenerate with `CARTOGRAPH_WRITE_GENERATED=1 cargo test -p agents
//! ui_specialist_table_is_generated_from_the_capability_table`.

use super::*;

const UI_TABLE: &str = "../../ui/src/generated/specialistTable.ts";

fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn union(values: impl Iterator<Item = String>) -> String {
    values
        .map(|value| format!("\n  | '{value}'"))
        .collect::<String>()
}

fn typescript() -> String {
    let mut out = String::from(
        "// GENERATED from crates/agents/src/investigation/capabilities.rs (ADR-0071) — do not edit.\n\
         // Regenerate: CARTOGRAPH_WRITE_GENERATED=1 cargo test -p agents ui_specialist_table_is_generated_from_the_capability_table\n\n",
    );
    out += &format!(
        "export type SpecialistId ={};\n",
        union(SpecialistId::all().map(wire))
    );
    out += &format!(
        "export type SpecialistRole ={};\n",
        union(SpecialistRole::ALL.into_iter().map(wire))
    );
    out += &format!(
        "export type SpecialistAvailability ={};\n",
        union(
            [
                Availability::Current,
                Availability::Offered,
                Availability::Retired
            ]
            .into_iter()
            .map(wire)
        )
    );
    out += &format!(
        "export type SpecialistCapability ={};\n\n",
        union(Capability::ALL.into_iter().map(wire))
    );
    out += "export interface SpecialistTableRow {\n  readonly role: SpecialistRole;\n  readonly version: number;\n  readonly availability: SpecialistAvailability;\n  readonly capabilities: readonly SpecialistCapability[];\n}\n\n";
    out += "/** One row per specialist version, in host declaration order. */\nexport const SPECIALIST_TABLE: Readonly<Record<SpecialistId, SpecialistTableRow>> = {\n";
    for row in SPECIALIST_TABLE {
        let capabilities = row
            .capabilities
            .iter()
            .map(|capability| format!("'{}'", wire(capability)))
            .collect::<Vec<_>>()
            .join(", ");
        out += &format!(
            "  '{}': {{ role: '{}', version: {}, availability: '{}', capabilities: [{}] }},\n",
            wire(row.id),
            wire(row.role),
            row.version,
            wire(row.availability),
            capabilities
        );
    }
    out += "};\n";
    out
}

// AC-1401: the generated UI table is exactly the host table.
#[test]
fn ui_specialist_table_is_generated_from_the_capability_table() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(UI_TABLE);
    let expected = typescript();
    if std::env::var_os("CARTOGRAPH_WRITE_GENERATED").is_some() {
        std::fs::write(&path, &expected).unwrap();
    }
    let actual = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        actual == expected,
        "{} is stale; regenerate it (see this file's header)",
        path.display()
    );
}
