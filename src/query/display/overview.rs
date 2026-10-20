//! Explicit local overview on the same graph wire schema, without MCP carrier limits.

use std::borrow::Cow;
use std::collections::HashMap;

use serde_json::Value;

use crate::client::{Client, api};
use crate::query::{FailureKind, ToolError};

use super::graph::{Edge, EdgeKind, Graph, Node};
use super::{BoundedWriter, Data, Display, Identity};

pub(in crate::query) const MAX_OVERVIEW_NODES: usize = 12_000;
pub(in crate::query) const MAX_OVERVIEW_EDGES: usize = 48_000;
const MAX_OVERVIEW_BYTES: usize = 3 * 1024 * 1024;

fn invalid(reason: &str) -> ToolError {
    ToolError::new("overview", FailureKind::Unavailable, reason)
}

fn relative(value: &str, root_allowed: bool) -> Result<&str, ToolError> {
    if root_allowed && (value.is_empty() || value == ".") {
        return Ok("");
    }
    if value.is_empty()
        || value.len() > 4096
        || value.contains(['\\', ':'])
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(
            "Backend overview metadata contains an invalid codebase-relative path.",
        ));
    }
    Ok(value)
}

fn add_node<'a>(
    graph: &mut Graph<'a>,
    label: &'a str,
    category: Option<&'static str>,
    path: &'a str,
    detail: Option<String>,
) -> Result<usize, ToolError> {
    if graph.nodes.len() >= MAX_OVERVIEW_NODES {
        return Err(invalid(
            "Indexed overview exceeds the 12000-node local limit. No partial overview was returned.",
        ));
    }
    if label.len() > 4096 || label.chars().any(char::is_control) {
        return Err(invalid(
            "Backend overview metadata contains an invalid node label.",
        ));
    }
    let index = graph.nodes.len();
    graph.nodes.push(Node {
        id: index.to_string(),
        label: Cow::Borrowed(label),
        detail,
        hit: None,
        category,
        path: Some(path),
        project: None,
        indexing: None,
        indexing_error: None,
    });
    Ok(index)
}

fn add_edge<'g>(
    graph: &'g mut Graph<'_>,
    from: usize,
    to: usize,
    kind: EdgeKind,
    label: String,
) -> Result<&'g mut Edge, ToolError> {
    if graph.edges.len() >= MAX_OVERVIEW_EDGES {
        return Err(invalid(
            "Indexed overview exceeds the 48000-edge local limit. No partial overview was returned.",
        ));
    }
    graph.edges.push(Edge {
        from: graph.nodes[from].id.clone(),
        to: graph.nodes[to].id.clone(),
        label,
        kind: Some(kind),
        symbol: None,
        moniker: None,
    });
    Ok(graph.edges.last_mut().expect("edge was appended"))
}

struct Catalog<'a> {
    paths: HashMap<&'a str, (usize, bool)>,
    files: Vec<(&'a str, usize)>,
}

fn project_catalog<'a>(
    graph: &mut Graph<'a>,
    tree: &'a [api::FileTreeNode],
) -> Result<Catalog<'a>, ToolError> {
    let root = add_node(graph, "Indexed files", Some("directory"), "", None)?;
    graph.roots.push(graph.nodes[root].id.clone());
    let mut paths = HashMap::new();
    paths.insert("", (root, true));
    let mut files = Vec::new();
    let mut stack: Vec<_> = tree.iter().rev().map(|node| (node, "")).collect();
    let mut root_wrapper = false;
    while let Some((node, parent)) = stack.pop() {
        let path = relative(&node.path, node.is_directory)?;
        if path.is_empty() {
            if root_wrapper || !node.is_directory || !parent.is_empty() {
                return Err(invalid(
                    "Backend file tree contains duplicate or invalid root directories.",
                ));
            }
            root_wrapper = true;
        } else {
            if paths.contains_key(path)
                || path.rsplit_once('/').map_or("", |(parent, _)| parent) != parent
            {
                return Err(invalid(
                    "Backend file tree contains duplicate paths or inconsistent directory membership.",
                ));
            }
            if path.rsplit('/').next() != Some(node.name.as_str())
                || node.size.is_some_and(|size| size < 0)
            {
                return Err(invalid(
                    "Backend file tree contains inconsistent file metadata.",
                ));
            }
            let index = add_node(
                graph,
                &node.name,
                Some(if node.is_directory {
                    "directory"
                } else {
                    "file"
                }),
                path,
                None,
            )?;
            paths.insert(path, (index, node.is_directory));
            if !node.is_directory {
                files.push((path, index));
            }
            add_edge(
                graph,
                paths[parent].0,
                index,
                EdgeKind::Containment,
                "contains".into(),
            )?;
        }
        if let Some(children) = node.children.as_ref() {
            if !node.is_directory && !children.is_empty() {
                return Err(invalid("Backend file tree attaches children to a file."));
            }
            if stack.len().saturating_add(children.len()) > MAX_OVERVIEW_NODES {
                return Err(invalid(
                    "Indexed overview exceeds the 12000-node local limit. No partial overview was returned.",
                ));
            }
            stack.extend(children.iter().rev().map(|child| (child, path)));
        }
    }
    Ok(Catalog { paths, files })
}

fn project_file_status<'a>(
    graph: &mut Graph<'a>,
    catalog: &mut Catalog<'a>,
    files: &'a [api::CodebaseFile],
) -> Result<(), ToolError> {
    let mut seen = std::collections::HashSet::new();
    for file in files {
        let path = relative(&file.path, false)?;
        if file.size < 0 || !seen.insert(path) {
            return Err(invalid(
                "Backend file catalog contains invalid sizes or duplicate paths.",
            ));
        }
        let index = match catalog.paths.get(path) {
            Some(&(_, true)) => {
                return Err(invalid(
                    "Backend file catalog identifies a directory as a file.",
                ));
            }
            Some(&(index, false)) => index,
            None => {
                let label = path
                    .rsplit('/')
                    .next()
                    .expect("validated relative paths have a name");
                let index = add_node(graph, label, Some("file"), path, None)?;
                catalog.paths.insert(path, (index, false));
                catalog.files.push((path, index));
                // The flat catalog proves the file identity, but not a missing
                // tree snapshot's containment. Do not invent directory nodes.
                graph.roots.push(graph.nodes[index].id.clone());
                graph.truncated = true;
                index
            }
        };
        graph.nodes[index].indexing = file.status.as_ref().map(api::CatalogFileStatus::indexing);
    }
    if files.iter().any(|file| {
        file.status
            .as_ref()
            .is_some_and(|status| matches!(status.indexing(), "pending" | "processing" | "failed"))
    }) {
        graph.truncated = true;
        graph.caveats.push("Per-file catalog evidence reports pending, processing, or failed indexing. Available structural relationships remain visible; embedding completion is not required.".into());
    }
    Ok(())
}

fn project_build_units<'a>(
    graph: &mut Graph<'a>,
    source: &'a api::ProjectGraph,
    catalog: &Catalog<'_>,
) -> Result<HashMap<&'a str, Vec<usize>>, ToolError> {
    let mut project_roots: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut identities: HashMap<&str, Vec<usize>> = HashMap::new();
    for project in &source.projects {
        let path = relative(&project.root, true)?;
        if project.kind.len() > 512 {
            return Err(invalid("Backend project kind exceeds the metadata limit."));
        }
        let index = add_node(
            graph,
            &project.name,
            Some("project"),
            path,
            Some(format!(
                "{} · {} reported project files",
                project.kind, project.file_count
            )),
        )?;
        graph.nodes[index].project = Some(path);
        project_roots.entry(path).or_default().push(index);
        identities.entry(path).or_default().push(index);
        if let Some(config) = project.config_file.as_deref() {
            let config = relative(config, false)?;
            if config != path {
                identities.entry(config).or_default().push(index);
            }
        }
        graph.roots.push(graph.nodes[index].id.clone());
        if let Some(&(directory, true)) = catalog.paths.get(path) {
            add_edge(
                graph,
                index,
                directory,
                EdgeKind::Association,
                "project root".into(),
            )?;
        }
    }
    let mut groups = Vec::with_capacity(source.groups.len());
    for group in &source.groups {
        let path = relative(&group.root, true)?;
        if group.kind.len() > 512 {
            return Err(invalid("Backend group kind exceeds the metadata limit."));
        }
        let index = add_node(
            graph,
            &group.name,
            Some("group"),
            path,
            Some(group.kind.clone()),
        )?;
        identities.entry(path).or_default().push(index);
        graph.roots.push(graph.nodes[index].id.clone());
        groups.push(index);
    }
    let mut unresolved = HashMap::new();
    for (group, &from) in source.groups.iter().zip(&groups) {
        for child in &group.children {
            let path = relative(child, true)?;
            let to = match identities.get(path).map(Vec::as_slice) {
                Some([index]) => *index,
                _ => {
                    if let Some(&index) = unresolved.get(path) {
                        index
                    } else {
                        let index = add_node(graph, child, None, path, Some("Reported project/group child root; identity unresolved or ambiguous".into()))?;
                        unresolved.insert(path, index);
                        index
                    }
                }
            };
            add_edge(
                graph,
                from,
                to,
                EdgeKind::Containment,
                "contains reported child".into(),
            )?;
        }
    }
    if !unresolved.is_empty() {
        graph.caveats.push("Some reported project/group children have unresolved or ambiguous identities. Their exact root tokens are preserved, not guessed.".into());
    }
    Ok(project_roots)
}

fn connect_project_files<'a>(
    graph: &mut Graph<'a>,
    files: &[(&'a str, usize)],
    project_roots: &HashMap<&str, Vec<usize>>,
) -> Result<(), ToolError> {
    for &(path, index) in files {
        let mut ancestor = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        loop {
            if let Some(owners) = project_roots.get(ancestor) {
                for &owner in owners {
                    add_edge(
                        graph,
                        owner,
                        index,
                        EdgeKind::Association,
                        "within project root".into(),
                    )?;
                }
                if owners.len() == 1 {
                    graph.nodes[index].project = Some(ancestor);
                }
                break;
            }
            if ancestor.is_empty() {
                break;
            }
            ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
    Ok(())
}

fn connect_imports(
    graph: &mut Graph<'_>,
    catalog: &Catalog<'_>,
    imports: &[api::ImportEdge],
) -> Result<(), ToolError> {
    for import in imports {
        let from = relative(&import.from, false)?;
        let to = relative(&import.to, false)?;
        if [from, to]
            .iter()
            .any(|path| catalog.paths.get(path).is_some_and(|entry| entry.1))
        {
            return Err(invalid("Resolved import endpoints identify directories."));
        }
        if import.import_path.len() > 4096 || import.import_path.chars().any(char::is_control) {
            return Err(invalid(
                "Backend import metadata exceeds the local overview limit.",
            ));
        }
        let (Some(&(from, false)), Some(&(to, false))) =
            (catalog.paths.get(from), catalog.paths.get(to))
        else {
            graph.truncated = true;
            continue;
        };
        add_edge(
            graph,
            from,
            to,
            EdgeKind::Import,
            if import.import_path.is_empty() {
                "imports".into()
            } else {
                format!("imports · {}", import.import_path)
            },
        )?;
    }
    Ok(())
}

fn connect_references(
    graph: &mut Graph<'_>,
    catalog: &Catalog<'_>,
    references: &[api::SymbolEdge],
) -> Result<(), ToolError> {
    for reference in references {
        let from = relative(&reference.from_file, false)?;
        let to = relative(&reference.to_file, false)?;
        if [from, to]
            .iter()
            .any(|path| catalog.paths.get(path).is_some_and(|entry| entry.1))
        {
            return Err(invalid(
                "Resolved symbol-binding endpoints identify directories.",
            ));
        }
        if [&reference.name, &reference.moniker].iter().any(|value| {
            value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
        }) {
            return Err(invalid(
                "Backend symbol-binding evidence is empty, invalid, or exceeds the local overview metadata limit.",
            ));
        }
        let (Some(&(from, false)), Some(&(to, false))) =
            (catalog.paths.get(from), catalog.paths.get(to))
        else {
            graph.truncated = true;
            continue;
        };
        let edge = add_edge(graph, from, to, EdgeKind::Reference, "references".into())?;
        edge.symbol = Some(reference.name.clone());
        edge.moniker = Some(reference.moniker.clone());
    }
    Ok(())
}

/// Available backend layers and their explicit incompleteness evidence.
pub(in crate::query) struct OverviewInput<'a> {
    pub tree: &'a [api::FileTreeNode],
    pub projects: &'a api::ProjectGraph,
    pub imports: &'a [api::ImportEdge],
    pub imports_total: u32,
    pub references: &'a [api::SymbolEdge],
    pub references_total: u32,
    pub files: &'a [api::CodebaseFile],
    pub caveats: Vec<String>,
    pub loading: bool,
}

/// Every catalog file is retained, even when it has no resolved imports or project.
/// Hash lookups plus ancestor walks avoid a files × projects or nodes × edges scan.
pub(in crate::query) fn project_overview(
    client: &Client,
    input: OverviewInput<'_>,
) -> Result<Value, ToolError> {
    let OverviewInput {
        tree,
        projects,
        imports,
        imports_total,
        references,
        references_total,
        files,
        caveats,
        loading,
    } = input;
    if imports.len() > imports_total as usize || references.len() > references_total as usize {
        return Err(invalid(
            "Backend overview relationship counts are inconsistent.",
        ));
    }
    if tree.len() > MAX_OVERVIEW_NODES
        || projects
            .projects
            .len()
            .saturating_add(projects.groups.len())
            > MAX_OVERVIEW_NODES
    {
        return Err(invalid(
            "Indexed overview exceeds the 12000-node local limit. No partial overview was returned.",
        ));
    }
    let mut graph = Graph::new("Indexed project overview", None);
    graph.loading = loading.then_some(true);
    graph.truncated = !caveats.is_empty()
        || imports.len() != imports_total as usize
        || references.len() != references_total as usize;
    graph.caveats = caveats;
    let mut catalog = project_catalog(&mut graph, tree)?;
    project_file_status(&mut graph, &mut catalog, files)?;
    let project_roots = project_build_units(&mut graph, projects, &catalog)?;
    connect_project_files(&mut graph, &catalog.files, &project_roots)?;
    let before_imports = graph.edges.len();
    connect_imports(&mut graph, &catalog, imports)?;
    let import_count = graph.edges.len() - before_imports;
    let before_references = graph.edges.len();
    connect_references(&mut graph, &catalog, references)?;
    let reference_count = graph.edges.len() - before_references;
    if graph.truncated {
        graph.caveats.push("Partial indexed structure: indexing is incomplete or some structural data is unavailable. The specific caveats and reported relationship counts describe what is available. Only catalog-backed endpoints are connected; no missing links are inferred.".into());
    }
    graph.caveats.push(format!("Available indexed structure: {} files, {} directories, {} projects, {} groups, {import_count} of {imports_total} resolved file imports, {reference_count} of {references_total} resolved symbol bindings.", catalog.files.len(), catalog.paths.len() - catalog.files.len(), projects.projects.len(), projects.groups.len()));
    graph.caveats.push("Containment describes indexed paths. Project association uses nearest reported project roots, not compiler inclusion or verified semantic binding. Reference edges preserve backend-resolved symbol names and monikers; these and file imports provide structural evidence for inferred functional communities, not embedding similarity or every runtime relationship.".into());
    let display = Display {
        version: 1,
        identity: Identity::new(client, true),
        data: Data::Graph { graph },
    };
    let mut bytes = BoundedWriter {
        inner: Vec::new(),
        written: 0,
        max: MAX_OVERVIEW_BYTES,
    };
    serde_json::to_writer(&mut bytes, &display).map_err(|_| invalid("Indexed overview exceeds the 3 MiB local display limit. No partial overview was returned."))?;
    serde_json::from_slice(&bytes.inner)
        .map_err(|_| invalid("Could not encode the indexed overview."))
}

#[cfg(test)]
mod tests;
