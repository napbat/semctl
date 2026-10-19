//! Explicit local indexed-structure query; no source probes, model or embedding calls.

use super::inspection::{GRAPH_LIST_CAP, fetch_all_pages};
use super::{Client, FailureKind, QueryOutput, QueryPresentation, ToolError, api, bound_codebase};

pub async fn overview(
    client: &Client,
    presentation: QueryPresentation,
) -> Result<QueryOutput, ToolError> {
    let cb = bound_codebase(client, "overview")?;
    let tree = client
        .get::<Vec<api::FileTreeNode>>(&format!("/v1/codebases/{cb}/files/tree"))
        .await
        .map_err(|error| ToolError::from_client("overview", &error))?;
    let projects = client
        .get::<api::ProjectGraph>(&format!("/v1/codebases/{cb}/projects"))
        .await
        .map_err(|error| ToolError::from_client("overview", &error))?;
    let (imports, total) =
        complete_edges::<api::ImportEdge>(client, cb, "imports", "Resolved imports").await?;
    let (references, references_total) =
        complete_edges::<api::SymbolEdge>(client, cb, "symbol-edges", "Resolved symbol bindings")
            .await?;
    let display = super::display::project_overview(
        client,
        &tree,
        &projects,
        &imports,
        total,
        &references,
        references_total,
    )?;
    let text = display["caveats"]
        .as_array()
        .expect("overview projection owns its caveats")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(QueryOutput::new(
        text,
        (presentation == QueryPresentation::Inspector).then_some(display),
    ))
}

async fn complete_edges<T: serde::de::DeserializeOwned>(
    client: &Client,
    codebase: &str,
    endpoint: &str,
    label: &str,
) -> Result<(Vec<T>, u32), ToolError> {
    let (edges, total) = fetch_all_pages::<T>(
        client,
        &format!("/v1/codebases/{codebase}/graph/{endpoint}"),
        GRAPH_LIST_CAP,
    )
    .await
    .map_err(|error| ToolError::from_client("overview", &error))?;
    if edges.len() != total as usize {
        return Err(ToolError::new(
            "overview",
            FailureKind::Unavailable,
            format!(
                "{label} are incomplete: received {} of {total} reported links (local fetch limit {GRAPH_LIST_CAP}). No partial overview was returned.",
                edges.len()
            ),
        ));
    }
    Ok((edges, total))
}

#[cfg(test)]
mod tests;
