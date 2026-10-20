//! Bounded relationship projections. This module performs no source or network I/O.

use std::borrow::Cow;

use serde::Serialize;
use serde_json::Value;

use crate::client::{Client, api};

use super::{
    Binding, Data, Display, Hit, Identity, MAX_DISPLAY_BYTES, MAX_DISPLAY_HITS, bounded,
    search_hit_bytes,
};

mod relations;
#[cfg(test)]
mod tests;

const MAX_NODES: usize = 64;
const MAX_EDGES: usize = 128;

#[derive(Clone, Copy)]
pub(in crate::query) enum GraphInput<'a> {
    Trace {
        symbol: &'a str,
        result: &'a api::TraceResult,
    },
    CallGraph {
        symbol: &'a str,
        graph: &'a api::CallGraph,
    },
    CallPath {
        from: &'a str,
        to: &'a str,
        hits: &'a [api::SearchHit],
    },
    FlowPath {
        from: &'a str,
        to: &'a str,
        hits: &'a [api::SearchHit],
    },
    Boundaries {
        boundary: &'a str,
        targets: &'a [String],
        incoming: bool,
    },
    Types {
        symbol: &'a str,
        hierarchy: &'a api::TypeHierarchy,
    },
    Clusters {
        label: &'a str,
        clusters: &'a [api::GraphCluster],
    },
    Unused {
        page: &'a api::Page<api::UnusedDefinition>,
    },
    Imports {
        edges: &'a [api::ImportEdge],
        total: u32,
    },
    SymbolEdges {
        edges: &'a [api::SymbolEdge],
        total: u32,
    },
    ExternalLinks {
        links: &'a [api::ExternalLink],
        total: u32,
    },
    Projects(&'a api::ProjectGraph),
    Outline(&'a api::FileOutline),
}

#[derive(Serialize)]
pub(super) struct Graph<'a> {
    pub(super) title: Cow<'a, str>,
    pub(super) query: Option<&'a str>,
    pub(super) nodes: Vec<Node<'a>>,
    pub(super) edges: Vec<Edge>,
    pub(super) roots: Vec<String>,
    pub(super) caveats: Vec<String>,
    pub(super) truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) loading: Option<bool>,
    #[serde(skip)]
    invalid: bool,
}

#[derive(Serialize)]
pub(super) struct Node<'a> {
    pub(super) id: String,
    pub(super) label: Cow<'a, str>,
    pub(super) detail: Option<String>,
    pub(super) hit: Option<Hit<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) category: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) project: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) indexing: Option<&'static str>,
    #[serde(rename = "indexingError", skip_serializing_if = "Option::is_none")]
    pub(super) indexing_error: Option<&'a str>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum EdgeKind {
    Containment,
    Association,
    Import,
    Reference,
}

#[derive(Serialize)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<EdgeKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) moniker: Option<String>,
}

impl<'a> Graph<'a> {
    pub(super) fn new(title: impl Into<Cow<'a, str>>, query: Option<&'a str>) -> Self {
        Self {
            title: title.into(),
            query,
            nodes: Vec::new(),
            edges: Vec::new(),
            roots: Vec::new(),
            caveats: Vec::new(),
            truncated: false,
            loading: None,
            invalid: false,
        }
    }

    fn text_fits(&mut self, texts: &[&str]) -> bool {
        let fits = texts
            .iter()
            .fold(0usize, |total, text| total.saturating_add(text.len()))
            <= MAX_DISPLAY_BYTES;
        self.invalid |= !fits;
        fits
    }

    fn caveat(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text.len() > MAX_DISPLAY_BYTES {
            self.invalid = true;
        } else if !self.caveats.contains(&text) {
            self.caveats.push(text);
        }
    }

    fn node(
        &mut self,
        id: &str,
        label: impl Into<Cow<'a, str>>,
        detail: Option<String>,
        mut hit: Option<Hit<'a>>,
    ) -> bool {
        if self.nodes.iter().any(|node| node.id == id) {
            return true;
        }
        if self.nodes.len() == MAX_NODES {
            self.truncated = true;
            return false;
        }
        let label = label.into();
        if id.is_empty()
            || id.len() > MAX_DISPLAY_BYTES
            || label.len() > MAX_DISPLAY_BYTES
            || detail
                .as_ref()
                .is_some_and(|text| text.len() > MAX_DISPLAY_BYTES)
        {
            self.invalid = true;
            return false;
        }
        if hit.is_some()
            && self.nodes.iter().filter(|node| node.hit.is_some()).count() == MAX_DISPLAY_HITS
        {
            hit = None;
            self.caveat("Source previews are limited to 32 locations. Full results remain in the native text.");
        }
        self.nodes.push(Node {
            id: id.into(),
            label,
            detail,
            hit,
            category: None,
            path: None,
            project: None,
            indexing: None,
            indexing_error: None,
        });
        true
    }

    fn hit(&mut self, hit: &'a api::SearchHit) -> bool {
        if self.nodes.len() == MAX_NODES && !self.nodes.iter().any(|node| node.id == hit.id) {
            self.truncated = true;
            return false;
        }
        if !self.text_fits(&[
            &hit.id,
            hit.path.as_deref().unwrap_or(""),
            hit.qualified_symbol
                .as_deref()
                .or(hit.symbol.as_deref())
                .unwrap_or(""),
        ]) {
            return false;
        }
        let preview = if search_hit_bytes(hit) <= MAX_DISPLAY_BYTES / 4 {
            Some(Hit::graph(hit))
        } else {
            self.caveat("Some source previews are omitted to keep the relationship view bounded. Full results remain in the native text.");
            None
        };
        let label = hit
            .qualified_symbol
            .as_deref()
            .or(hit.symbol.as_deref())
            .or(hit.path.as_deref())
            .unwrap_or(&hit.id);
        let detail = hit
            .path
            .as_deref()
            .map(|path| match (hit.line_start, hit.line_end) {
                (Some(start), Some(end)) => format!("{path}:{start}-{end}"),
                _ => path.to_owned(),
            });
        self.node(&hit.id, label, detail, preview)
    }

    fn edge(&mut self, from: &str, to: &str, label: impl Into<String>) {
        if !self.nodes.iter().any(|node| node.id == from)
            || !self.nodes.iter().any(|node| node.id == to)
        {
            self.truncated = true;
            return;
        }
        let label = label.into();
        if label.len() > MAX_DISPLAY_BYTES {
            self.invalid = true;
            return;
        }
        if self
            .edges
            .iter()
            .any(|edge| edge.from == from && edge.to == to && edge.label == label)
        {
            return;
        }
        if self.edges.len() == MAX_EDGES {
            self.truncated = true;
            return;
        }
        self.edges.push(Edge {
            from: from.into(),
            to: to.into(),
            label,
            kind: None,
            symbol: None,
            moniker: None,
        });
    }

    fn root(&mut self, id: &str) {
        if !self.nodes.iter().any(|node| node.id == id) {
            self.truncated = true;
        } else if !self.roots.iter().any(|root| root == id) {
            self.roots.push(id.into());
        }
    }

    // These are navigation starting points, not a claim that the graph is a tree.
    // Include disconnected cycles even when another component has an incoming-free node.
    fn finish_roots(&mut self) {
        for node in &self.nodes {
            if !self.edges.iter().any(|edge| edge.to == node.id) && !self.roots.contains(&node.id) {
                self.roots.push(node.id.clone());
            }
        }
        let mut reachable = [false; MAX_NODES];
        for (seen, node) in reachable.iter_mut().zip(&self.nodes) {
            *seen = self.roots.contains(&node.id);
        }
        loop {
            let mut changed = false;
            for edge in &self.edges {
                let from = self.nodes.iter().position(|node| node.id == edge.from);
                let to = self.nodes.iter().position(|node| node.id == edge.to);
                if let (Some(from), Some(to)) = (from, to)
                    && reachable[from]
                    && !reachable[to]
                {
                    reachable[to] = true;
                    changed = true;
                }
            }
            if changed {
                continue;
            }
            if let Some(index) = reachable[..self.nodes.len()]
                .iter()
                .position(|value| !value)
            {
                self.roots.push(self.nodes[index].id.clone());
                reachable[index] = true;
            } else {
                break;
            }
        }
    }
}

pub(in crate::query) fn project_graph(client: &Client, input: GraphInput<'_>) -> Option<Value> {
    let query_fits = match &input {
        GraphInput::Trace { symbol, .. }
        | GraphInput::CallGraph { symbol, .. }
        | GraphInput::Types { symbol, .. } => symbol.len() <= MAX_DISPLAY_BYTES,
        GraphInput::CallPath { from, to, .. } | GraphInput::FlowPath { from, to, .. } => {
            from.len().saturating_add(to.len()) <= MAX_DISPLAY_BYTES
        }
        GraphInput::Boundaries { boundary, .. } => boundary.len() <= MAX_DISPLAY_BYTES,
        GraphInput::Clusters { label, .. } => label.len() <= MAX_DISPLAY_BYTES,
        GraphInput::Outline(outline) => outline.path.len() <= MAX_DISPLAY_BYTES,
        _ => true,
    };
    if !query_fits {
        return None;
    }
    let mut graph = match input {
        GraphInput::Trace { symbol, result } => trace(symbol, result),
        GraphInput::CallGraph { symbol, graph } => calls(symbol, graph),
        GraphInput::CallPath { from, to, hits } => path(from, to, hits, false),
        GraphInput::FlowPath { from, to, hits } => path(from, to, hits, true),
        GraphInput::Boundaries {
            boundary,
            targets,
            incoming,
        } => boundaries(boundary, targets, incoming),
        GraphInput::Types { symbol, hierarchy } => relations::types(symbol, hierarchy),
        GraphInput::Clusters { label, clusters } => clusters_view(label, clusters),
        GraphInput::Unused { page } => unused(page),
        GraphInput::Imports { edges, total } => relations::imports(edges, total),
        GraphInput::SymbolEdges { edges, total } => relations::symbol_edges(edges, total),
        GraphInput::ExternalLinks { links, total } => relations::external_links(links, total),
        GraphInput::Projects(projects) => relations::projects(projects),
        GraphInput::Outline(outline) => relations::outline(outline),
    };
    if graph.invalid
        || graph.nodes.is_empty()
        || graph.title.len() > MAX_DISPLAY_BYTES
        || graph
            .query
            .is_some_and(|query| query.len() > MAX_DISPLAY_BYTES)
    {
        return None;
    }
    graph.finish_roots();
    if graph.truncated {
        graph.caveat("This view is truncated to at most 64 nodes and 128 relationships. Full results remain in the native text.");
    }
    let mut display = Display {
        version: 1,
        identity: Identity::new(client, false),
        data: Data::Graph { graph },
    };
    if let Some(value) = bounded(&display) {
        return Some(value);
    }
    let Data::Graph { graph } = &mut display.data else {
        return None;
    };
    let mut removed = false;
    for node in &mut graph.nodes {
        removed |= node.hit.take().is_some();
    }
    if removed {
        graph.caveat("Source previews are omitted to fit the display byte limit. Full results remain in the native text.");
        bounded(&display)
    } else {
        None
    }
}

fn trace<'a>(symbol: &'a str, result: &'a api::TraceResult) -> Graph<'a> {
    let mut graph = Graph::new("Symbol neighborhood", Some(symbol));
    graph.truncated = result
        .definition
        .len()
        .saturating_add(result.callers.len())
        .saturating_add(result.callees.len())
        > MAX_NODES - 1;
    // The query is the shared subject. Multiple definition matches do not imply
    // every caller binds to every definition match.
    let subject = format!("query:{symbol}");
    graph.node(&subject, symbol, Some("Queried symbol".into()), None);
    graph.root(&subject);
    for hit in result.definition.iter().take(MAX_NODES) {
        graph.hit(hit);
        graph.edge(&subject, &hit.id, "definition match");
    }
    for hit in result.callers.iter().take(MAX_NODES) {
        graph.hit(hit);
        graph.edge(&hit.id, &subject, "calls queried symbol");
    }
    for hit in result.callees.iter().take(MAX_NODES) {
        graph.hit(hit);
        graph.edge(&subject, &hit.id, "queried symbol calls");
    }
    graph
}

fn calls<'a>(symbol: &'a str, source: &'a api::CallGraph) -> Graph<'a> {
    let mut graph = Graph::new("Call graph", Some(symbol));
    graph.truncated = source.nodes.len() > MAX_NODES || source.edges.len() > MAX_EDGES;
    for hit in source.nodes.iter().take(MAX_NODES) {
        graph.hit(hit);
    }
    for edge in source.edges.iter().take(MAX_EDGES) {
        graph.edge(&edge.from, &edge.to, "calls");
    }
    graph
}

fn path<'a>(from: &'a str, to: &'a str, hits: &'a [api::SearchHit], flow: bool) -> Graph<'a> {
    let mut graph = Graph::new(
        if flow {
            "Value-flow witness"
        } else {
            "Call path"
        },
        Some(from),
    );
    graph.truncated = hits.len() > MAX_NODES;
    graph.caveat(format!("Requested path: {from} → {to}"));
    if flow {
        graph.caveat(
            "Ordered value-flow witness. Consecutive witness entries do not prove a direct call.",
        );
    }
    for hit in hits.iter().take(MAX_NODES) {
        graph.hit(hit);
    }
    for pair in hits.windows(2).take(MAX_NODES - 1) {
        graph.edge(
            &pair[0].id,
            &pair[1].id,
            if flow {
                "next in flow witness"
            } else {
                "calls along path"
            },
        );
    }
    if let Some(hit) = hits.first() {
        graph.root(&hit.id);
    }
    graph
}

fn boundaries<'a>(boundary: &'a str, targets: &'a [String], incoming: bool) -> Graph<'a> {
    let mut graph = Graph::new(
        if incoming {
            "Incoming value flow"
        } else {
            "Reachable boundaries"
        },
        Some(boundary),
    );
    graph.truncated = targets.len() > MAX_NODES - 1;
    graph.node(boundary, boundary, Some("External boundary".into()), None);
    graph.root(boundary);
    for target in targets.iter().take(MAX_NODES) {
        graph.node(
            target,
            target.as_str(),
            Some("External boundary".into()),
            None,
        );
        if incoming {
            graph.edge(target, boundary, "flows into");
        } else {
            graph.edge(boundary, target, "reaches");
        }
    }
    graph.caveat("Boundary reachability does not imply a direct call.");
    graph
}

fn clusters_view<'a>(label: &'a str, clusters: &'a [api::GraphCluster]) -> Graph<'a> {
    let mut graph = Graph::new(label, None);
    graph.caveat("Groups show reported membership only. The response does not include relationships between members.");
    graph.truncated = clusters.len() > MAX_NODES;
    for cluster in clusters.iter().take(MAX_NODES) {
        if !graph.text_fits(&[cluster.hash.as_deref().unwrap_or("")]) {
            break;
        }
        let id = format!("cluster:{}", cluster.index);
        let detail = Some(match &cluster.hash {
            Some(hash) => format!("Reported size: {} · hash: {hash}", cluster.size),
            None => format!("Reported size: {}", cluster.size),
        });
        graph.node(&id, format!("Group {}", cluster.index), detail, None);
        graph.root(&id);
        graph.truncated |= cluster.chunks.len() > MAX_NODES;
        for hit in cluster.chunks.iter().take(MAX_NODES) {
            graph.hit(hit);
            graph.edge(&id, &hit.id, "member");
        }
        if usize::try_from(cluster.size).is_ok_and(|size| size > cluster.chunks.len()) {
            graph.caveat("Some group members are absent from this response.");
        }
    }
    graph
}

fn unused(page: &api::Page<api::UnusedDefinition>) -> Graph<'_> {
    let mut graph = Graph::new("Unused definitions", None);
    graph.caveat(format!(
        "Page {} · page size {} · {} total reported definitions",
        page.number, page.size, page.total
    ));
    graph.truncated = page.items.len() > MAX_NODES;
    for row in page.items.iter().take(MAX_NODES) {
        if !graph.text_fits(&[&row.reason, &row.completeness_caveat]) {
            break;
        }
        graph.hit(&row.definition);
        if let Some(node) = graph
            .nodes
            .iter_mut()
            .find(|node| node.id == row.definition.id)
        {
            node.detail = Some(row.reason.clone());
        }
        graph.caveat(&row.completeness_caveat);
    }
    if usize::try_from(page.total).is_ok_and(|total| total > page.items.len()) {
        graph.caveat("This response is one page, not the complete set of definitions.");
    }
    graph
}

fn location(path: Option<&str>) -> Hit<'_> {
    Hit {
        path,
        symbol: None,
        qualified_name: None,
        enclosing_symbol: None,
        is_write: None,
        reference_namespace: None,
        reference_kind: None,
        external_target: None,
        project: None,
        language: None,
        kind: None,
        score: None,
        start_line: None,
        end_line: None,
        start_byte: None,
        end_byte: None,
        snippet: None,
        snippet_start_line: None,
        snippet_end_line: None,
        snippet_start_byte: None,
        snippet_end_byte: None,
        focus_line: None,
        focus_byte: None,
        codebase_id: None,
        binding: Binding {
            state: "not-probed",
            reason: None,
        },
    }
}
