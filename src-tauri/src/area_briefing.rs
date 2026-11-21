//! Area briefings (#2055, AC-0931): what a topic expert is handed for one
//! subsystem area before it starts.
//!
//! [`compile`] reads the graph, the decisions and the knowledge overlay
//! once, compiles D1 as the export does, and cuts each requested area's
//! part out of it (`spec::area_briefing`) with the names of the areas next
//! to it (`spec::area_neighbours`). No model runs and nothing is written
//! (R-INT-1, R-INT-3).
//!
//! An overlay that is not current for the graph supplies no areas, so a
//! briefing is refused then, as an area scope is (#2044).

use crate::AppState;
use core_graph::overlay::OverlayItem;
use serde::Serialize;
use tauri::Manager;

const D1_ARTIFACT_ID: &str = "d1-product-specification";

/// One area's briefing as the app shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AreaBriefingView {
    /// The Subsystem id.
    pub(crate) area: String,
    /// The area's name.
    pub(crate) name: String,
    /// Its member files.
    pub(crate) files: usize,
    /// The briefing, as Markdown.
    pub(crate) text: String,
    /// The D1 sections that hold a block for the area.
    pub(crate) sections: Vec<&'static str>,
    /// Capability subsections left out to keep the bound.
    pub(crate) left_out: usize,
    /// The names of the areas it calls or is called from.
    pub(crate) neighbours: Vec<String>,
}

/// The briefing of the Subsystem `only`, or of every named Subsystem by
/// name, from one compilation of D1. Not by id: an area's id hashes its
/// members' ids, which carry the registration's random key (ADR-0023), so
/// id order would change from one registration of a source to the next
/// (AC-1203). Names are unique; the id only breaks a tie.
pub(crate) fn compile(
    state: &AppState,
    only: Option<&str>,
) -> Result<Vec<AreaBriefingView>, String> {
    let review = crate::curated::gather(state);
    let read = crate::read_spec_locked(state)?;
    let mut areas: Vec<(String, String, usize, spec::AreaNeighbours)> = read
        .knowledge
        .subsystems
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) if only.is_none_or(|id| id == node.id) => {
                let name = node.props["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())?;
                let neighbours =
                    spec::area_neighbours(&read.edges, &read.knowledge.subsystems, &node.id)?;
                Some((
                    node.id.clone(),
                    name.to_owned(),
                    fact.cites.len(),
                    neighbours,
                ))
            }
            _ => None,
        })
        .collect();
    if areas.is_empty() {
        return Err("area_not_found".into());
    }
    areas.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let document = crate::compile_spec_artifact(
        read.inputs(Some(&review)),
        spec::ExportMode::BestEffort,
        D1_ARTIFACT_ID,
    )?
    .content;
    Ok(areas
        .into_iter()
        .map(|(area, name, files, neighbours)| {
            let briefing = spec::area_briefing(
                &document,
                &name,
                files,
                &neighbours,
                spec::AREA_BRIEFING_BYTES,
            );
            let mut near = neighbours.calls;
            near.extend(neighbours.called_by);
            near.sort();
            near.dedup();
            AreaBriefingView {
                area,
                name,
                files,
                text: briefing.text,
                sections: briefing.sections,
                left_out: briefing.left_out,
                neighbours: near,
            }
        })
        .collect())
}

/// The briefing a topic expert for the Subsystem `area` starts from.
#[tauri::command]
pub(crate) async fn area_briefing(
    area: String,
    app: tauri::AppHandle,
) -> Result<AreaBriefingView, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        compile(&state, Some(&area))?
            .pop()
            .ok_or_else(|| "area_not_found".to_owned())
    })
    .await
}

#[cfg(test)]
#[path = "area_briefing_tests.rs"]
pub(crate) mod tests;
