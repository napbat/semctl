//! Projections of explicitly reported file, type, project, and outline facts.

use crate::client::api;

use super::{Graph, MAX_EDGES, MAX_NODES, location};

pub(super) fn types<'a>(symbol: &'a str, source: &'a api::TypeHierarchy) -> Graph<'a> {
    let mut graph = Graph::new("Type hierarchy", Some(symbol));
    graph.truncated = source.nodes.len() > MAX_NODES || source.edges.len() > MAX_EDGES;
    if !source.complete {
        graph.caveat("The server reports an incomplete type hierarchy.");
    }
    if let Some(caveat) = &source.caveat {
        if !graph.text_fits(&[caveat]) {
            return graph;
        }
        graph.caveat(caveat);
    }
    for node in source.nodes.iter().take(MAX_NODES) {
        if !graph.text_fits(&[
            &node.identity,
            &node.name,
            &node.qualified_name,
            &node.kind,
            node.path.as_deref().unwrap_or(""),
        ]) {
            break;
        }
        let mut hit = location(node.path.as_deref());
        hit.symbol = Some(&node.name);
        hit.qualified_name = Some(&node.qualified_name);
        hit.kind = Some(&node.kind);
        hit.start_line = node.line_start;
        hit.end_line = node.line_end;
        hit.start_byte = node.byte_start;
        hit.end_byte = node.byte_end;
        if node.external {
            hit.external_target = Some(&node.identity);
        }
        graph.node(
            &node.identity,
            node.qualified_name.as_str(),
            Some(format!(
                "{} · {}",
                node.kind,
                if node.external {
                    "external identity"
                } else {
                    "indexed identity"
                }
            )),
            Some(hit),
        );
    }
    for edge in source.edges.iter().take(MAX_EDGES) {
        if !graph.text_fits(&[&edge.relation, &edge.origin]) {
            break;
        }
        graph.edge(
            &edge.subtype,
            &edge.supertype,
            format!(
                "{} · origin={} · depth={}",
                edge.relation, edge.origin, edge.depth
            ),
        );
    }
    for root in source.roots.iter().take(MAX_NODES) {
        graph.root(root);
    }
    graph
}

fn paged(graph: &mut Graph<'_>, total: u32, returned: usize) {
    graph.caveat(format!(
        "{returned} relationships returned · {total} total reported"
    ));
    if usize::try_from(total).is_ok_and(|total| total > returned) {
        graph.caveat("This response is one page, not the complete relationship graph.");
    }
}

pub(super) fn imports(edges: &[api::ImportEdge], total: u32) -> Graph<'_> {
    let mut graph = Graph::new("File imports", None);
    paged(&mut graph, total, edges.len());
    graph.truncated = edges.len() > MAX_EDGES;
    for edge in edges.iter().take(MAX_EDGES) {
        if !graph.text_fits(&[&edge.from, &edge.to, &edge.import_path]) {
            break;
        }
        graph.node(
            &edge.from,
            edge.from.as_str(),
            None,
            Some(location(Some(&edge.from))),
        );
        graph.node(
            &edge.to,
            edge.to.as_str(),
            None,
            Some(location(Some(&edge.to))),
        );
        graph.edge(
            &edge.from,
            &edge.to,
            format!("imports {}", edge.import_path),
        );
    }
    graph
}

pub(super) fn symbol_edges(edges: &[api::SymbolEdge], total: u32) -> Graph<'_> {
    let mut graph = Graph::new("Reference bindings", None);
    paged(&mut graph, total, edges.len());
    graph.truncated = edges.len() > MAX_EDGES;
    for edge in edges.iter().take(MAX_EDGES) {
        if !graph.text_fits(&[&edge.from_file, &edge.to_file, &edge.moniker, &edge.name]) {
            break;
        }
        graph.node(
            &edge.from_file,
            edge.from_file.as_str(),
            None,
            Some(location(Some(&edge.from_file))),
        );
        let target = format!("{}#{}", edge.to_file, edge.moniker);
        let mut hit = location(Some(&edge.to_file));
        hit.symbol = Some(&edge.name);
        hit.qualified_name = Some(&edge.moniker);
        graph.node(
            &target,
            edge.moniker.as_str(),
            Some(edge.to_file.clone()),
            Some(hit),
        );
        graph.edge(
            &edge.from_file,
            &target,
            format!("reference {} binds to", edge.name),
        );
    }
    graph
}

pub(super) fn external_links(links: &[api::ExternalLink], total: u32) -> Graph<'_> {
    let mut graph = Graph::new("Cross-codebase links", None);
    paged(&mut graph, total, links.len());
    graph.caveat("External targets belong to their reported codebases. These locations are not verified local files.");
    graph.truncated = links.len() > MAX_EDGES;
    for link in links.iter().take(MAX_EDGES) {
        if !graph.text_fits(&[
            &link.from_file,
            &link.import_path,
            &link.target_codebase_id,
            &link.target_file,
            &link.descriptor,
            &link.target_name,
            &link.ecosystem,
            &link.target_package,
        ]) {
            break;
        }
        graph.node(
            &link.from_file,
            link.from_file.as_str(),
            None,
            Some(location(Some(&link.from_file))),
        );
        let target = format!(
            "{}:{}#{}",
            link.target_codebase_id, link.target_file, link.descriptor
        );
        let mut hit = location(Some(&link.target_file));
        hit.symbol = Some(&link.target_name);
        hit.external_target = Some(&link.descriptor);
        hit.codebase_id = Some(&link.target_codebase_id);
        let detail = format!(
            "{} · package={} · descriptor={} · codebase={} · file={} · confidence={}",
            link.ecosystem,
            link.target_package,
            link.descriptor,
            link.target_codebase_id,
            link.target_file,
            link.confidence
        );
        graph.node(&target, link.target_name.as_str(), Some(detail), Some(hit));
        graph.edge(
            &link.from_file,
            &target,
            format!("resolves import {}", link.import_path),
        );
    }
    graph
}

pub(super) fn projects(source: &api::ProjectGraph) -> Graph<'_> {
    let mut graph = Graph::new("Projects and workspaces", None);
    graph.truncated = source.groups.len().saturating_add(source.projects.len()) > MAX_NODES;
    for group in source.groups.iter().take(MAX_NODES) {
        if !graph.text_fits(&[&group.root, &group.name, &group.kind]) {
            break;
        }
        let id = format!("group:{}", group.root);
        graph.node(
            &id,
            group.name.as_str(),
            Some(format!("{} · root={}", group.kind, group.root)),
            None,
        );
    }
    for project in source.projects.iter().take(MAX_NODES) {
        if !graph.text_fits(&[
            &project.root,
            &project.name,
            &project.kind,
            project.config_file.as_deref().unwrap_or(""),
        ]) {
            break;
        }
        let id = format!("project:{}", project.root);
        let mut hit = location(project.config_file.as_deref());
        hit.project = Some(&project.root);
        let detail = format!(
            "{} · root={} · files={} · config={}",
            project.kind,
            project.root,
            project.file_count,
            project.config_file.as_deref().unwrap_or("not reported")
        );
        graph.node(&id, project.name.as_str(), Some(detail), Some(hit));
    }
    for group in source.groups.iter().take(MAX_NODES) {
        let parent = format!("group:{}", group.root);
        graph.truncated |= group.children.len() > MAX_EDGES;
        for child in group.children.iter().take(MAX_EDGES) {
            if !graph.text_fits(&[child]) {
                break;
            }
            let group_id = format!("group:{child}");
            let project_id = format!("project:{child}");
            let group_match = graph.nodes.iter().any(|node| node.id == group_id);
            let project_match = graph.nodes.iter().any(|node| node.id == project_id);
            if group_match == project_match {
                // The child token is an explicit source fact. Do not guess a
                // project name/root match or collapse an ambiguous identity.
                let id = format!("child:{child}");
                graph.node(&id, child.as_str(), Some("Reported child token; project identity is unresolved, ambiguous, or outside this preview".into()), None);
                graph.edge(&parent, &id, "contains reported child token");
                graph.caveat("Some child tokens do not identify exactly one retained project/group root. The view preserves the reported tokens without guessing ownership.");
            } else {
                let target = if group_match { &group_id } else { &project_id };
                graph.edge(&parent, target, "contains reported child");
            }
        }
    }
    graph
}

pub(super) fn outline(source: &api::FileOutline) -> Graph<'_> {
    let mut graph = Graph::new("File outline", Some(&source.path));
    graph.caveat("Outline depth is reported per declaration. No parent edges are inferred from line ranges or depth.");
    graph.truncated = source.entries.len() > MAX_NODES;
    for entry in source.entries.iter().take(MAX_NODES) {
        if !graph.text_fits(&[
            &entry.chunk_id,
            &entry.kind,
            entry.symbol.as_deref().unwrap_or(""),
            entry.qualified_symbol.as_deref().unwrap_or(""),
            entry.symbol_kind.as_deref().unwrap_or(""),
        ]) {
            break;
        }
        let label = entry
            .qualified_symbol
            .as_deref()
            .or(entry.symbol.as_deref())
            .unwrap_or(&entry.kind);
        let mut hit = location(Some(&source.path));
        hit.symbol = entry.symbol.as_deref();
        hit.qualified_name = entry.qualified_symbol.as_deref();
        hit.kind = Some(entry.symbol_kind.as_deref().unwrap_or(&entry.kind));
        hit.start_line = Some(entry.line_start);
        hit.end_line = Some(entry.line_end);
        hit.snippet = entry.body.as_deref();
        if entry
            .body
            .as_ref()
            .is_some_and(|body| body.len() > super::MAX_DISPLAY_BYTES / 4)
        {
            hit.snippet = None;
            graph.caveat("Some outline bodies are omitted to keep the relationship view bounded. Full bodies remain in the native text.");
        }
        graph.node(
            &entry.chunk_id,
            label,
            Some(format!(
                "depth={} · kind={} · lines={}-{}",
                entry.depth, entry.kind, entry.line_start, entry.line_end
            )),
            Some(hit),
        );
    }
    graph
}
