//! Explicit local indexed-structure query; no source probes, model or embedding calls.

use super::display::OverviewInput;
use super::inspection::{FILES_PAGE_MAX, GRAPH_LIST_CAP};
use super::{Client, FailureKind, QueryOutput, QueryPresentation, ToolError, api, bound_codebase};
use crate::client::ApiFailure;

pub async fn overview(
    client: &Client,
    presentation: QueryPresentation,
) -> Result<QueryOutput, ToolError> {
    let snapshot = client.available_snapshot();
    let client = &snapshot;
    let cb = bound_codebase(client, "overview")?;
    let mut caveats = Vec::new();
    let mut loading = false;
    let tree = match client
        .get::<Vec<api::FileTreeNode>>(&format!("/v1/codebases/{cb}/files/tree"))
        .await
    {
        Ok(tree) => tree,
        Err(error) => {
            unavailable_layer(&error, "File tree", &mut caveats, &mut loading)?;
            Vec::new()
        }
    };
    let projects = match client
        .get::<api::ProjectGraph>(&format!("/v1/codebases/{cb}/projects"))
        .await
    {
        Ok(projects) => projects,
        Err(error) => {
            unavailable_layer(&error, "Projects", &mut caveats, &mut loading)?;
            api::ProjectGraph {
                projects: Vec::new(),
                groups: Vec::new(),
            }
        }
    };
    let (imports, total) = available_pages::<api::ImportEdge>(
        client,
        &format!("/v1/codebases/{cb}/graph/imports"),
        "Resolved imports",
        &mut caveats,
        &mut loading,
    )
    .await?;
    let (references, references_total) = available_pages::<api::SymbolEdge>(
        client,
        &format!("/v1/codebases/{cb}/graph/symbol-edges"),
        "Resolved symbol bindings",
        &mut caveats,
        &mut loading,
    )
    .await?;
    let (files, _) = available_pages::<api::CodebaseFile>(
        client,
        &format!("/v1/codebases/{cb}/files"),
        "Per-file indexing metadata",
        &mut caveats,
        &mut loading,
    )
    .await?;
    let display = super::display::project_overview(
        client,
        OverviewInput {
            tree: &tree,
            projects: &projects,
            imports: &imports,
            imports_total: total,
            references: &references,
            references_total,
            files: &files,
            caveats,
            loading,
        },
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

// Only a typed loading response establishes that a structural layer is temporarily
// unavailable. Authentication, transport, unsupported routes and malformed data fail.
fn unavailable_layer(
    error: &anyhow::Error,
    label: &str,
    caveats: &mut Vec<String>,
    loading: &mut bool,
) -> Result<(), ToolError> {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<ApiFailure>()
            .is_some_and(ApiFailure::is_loading)
    }) {
        *loading = true;
        caveats.push(format!(
            "{label} unavailable while the backend loads this layer: {error:#}"
        ));
        Ok(())
    } else {
        Err(ToolError::from_client("overview", error))
    }
}

async fn available_pages<T: serde::de::DeserializeOwned>(
    client: &Client,
    endpoint: &str,
    label: &str,
    caveats: &mut Vec<String>,
    loading: &mut bool,
) -> Result<(Vec<T>, u32), ToolError> {
    let mut rows = Vec::new();
    let mut total = 0;
    for page in 0.. {
        let path = format!("{endpoint}?page={page}&pageSize={FILES_PAGE_MAX}");
        let result = match client.get_page::<T>(&path).await {
            Ok(result) => result,
            Err(error) => {
                unavailable_layer(&error, label, caveats, loading)?;
                break;
            }
        };
        if result.number != page || result.items.len() > FILES_PAGE_MAX as usize {
            return Err(ToolError::new(
                "overview",
                FailureKind::Unavailable,
                format!("{label} returned invalid pagination metadata."),
            ));
        }
        total = result.total;
        let got = result.items.len();
        rows.extend(result.items);
        if rows.len() > total as usize {
            return Err(ToolError::new(
                "overview",
                FailureKind::Unavailable,
                format!("{label} returned more rows than its reported total."),
            ));
        }
        if got == 0 || rows.len() >= total as usize || rows.len() >= GRAPH_LIST_CAP {
            break;
        }
    }
    if rows.len() != total as usize {
        caveats.push(format!("{label} incomplete: received {} of {total} reported rows (local fetch limit {GRAPH_LIST_CAP}). Available rows are retained.", rows.len()));
    }
    Ok((rows, total))
}

#[cfg(test)]
mod tests;
