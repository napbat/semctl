//! Semantic file affinity on the native display schema.

use serde::Serialize;
use serde_json::Value;

use crate::client::Client;
use crate::query::{FailureKind, ToolError};

use super::{Data, Display, Identity};

/// The origin of the search text that describes one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(in crate::query) enum QuerySource {
    /// Markdown headings from the local checkout.
    Content,
    /// Indexed outline entry names.
    Outline,
    /// The humanized file stem and its parent folder name.
    Name,
}

/// What the bounded search for a file returned, not whether the file is indexed.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SearchEvidence {
    SelfMatch,
    OtherMatches,
    Empty,
    Loading,
}

#[derive(Debug, PartialEq, Serialize)]
pub(in crate::query) struct AffinityFile<'a> {
    path: &'a str,
    query: QuerySource,
    // Version 1 consumers require this field. It has always meant a self-match.
    #[serde(rename = "indexed")]
    pub(in crate::query) self_match: bool,
    results: usize,
    search: SearchEvidence,
}

impl<'a> AffinityFile<'a> {
    pub(in crate::query) fn new(
        path: &'a str,
        query: QuerySource,
        ranked: Option<&[String]>,
    ) -> Self {
        let search = match ranked {
            None => SearchEvidence::Loading,
            Some([]) => SearchEvidence::Empty,
            Some(paths) if paths.iter().any(|result| result == path) => SearchEvidence::SelfMatch,
            Some(_) => SearchEvidence::OtherMatches,
        };
        Self {
            path,
            query,
            self_match: search == SearchEvidence::SelfMatch,
            results: ranked.map_or(0, <[String]>::len),
            search,
        }
    }

    pub(in crate::query) fn is_empty(&self) -> bool {
        self.search == SearchEvidence::Empty
    }
}

/// One related file pair. `a` is before `b` in byte order.
#[derive(Debug, PartialEq, Serialize)]
pub(in crate::query) struct AffinityPair<'a> {
    pub(in crate::query) a: &'a str,
    pub(in crate::query) b: &'a str,
    pub(in crate::query) direct: f64,
    pub(in crate::query) shared: f64,
}

#[derive(Serialize)]
pub(super) struct Affinity<'a> {
    files: Vec<AffinityFile<'a>>,
    pairs: Vec<AffinityPair<'a>>,
    caveats: Vec<String>,
}

/// The input size bounds the display (64 files, 2016 pairs), so no byte cap applies.
pub(in crate::query) fn project_affinity<'a>(
    client: &Client,
    files: Vec<AffinityFile<'a>>,
    pairs: Vec<AffinityPair<'a>>,
    caveats: Vec<String>,
) -> Result<Value, ToolError> {
    let display = Display {
        version: 1,
        identity: Identity::new(client, true),
        data: Data::Affinity {
            affinity: Affinity {
                files,
                pairs,
                caveats,
            },
        },
    };
    serde_json::to_value(&display).map_err(|_| {
        ToolError::new(
            "affinity",
            FailureKind::Unavailable,
            "Could not encode the affinity display.",
        )
    })
}
