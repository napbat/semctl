//! Server evidence that a search or grep result is incomplete.

use std::fmt::Write;

use crate::client::{ResultTruncation, api};

#[derive(Clone, Copy)]
pub(super) enum BudgetedResult {
    Search,
    Grep,
}

/// Both the text response and the native inspector use this server-budget warning.
pub(super) fn budget_note(result: BudgetedResult, truncation: &ResultTruncation) -> String {
    let next = match result {
        BudgetedResult::Search => "next: narrow the query, lower top_k, or search without expand.",
        BudgetedResult::Grep => "next: narrow path or the pattern, or lower max.",
    };
    match (truncation, result) {
        (ResultTruncation::ContentBudget, BudgetedResult::Search) => format!(
            "note: the server cut this result to its content budget (32 KiB of snippet text \
             by default). A snippet is cut, a hit is not expanded, or later hits are dropped. \
             {next}\n"
        ),
        (ResultTruncation::ContentBudget, BudgetedResult::Grep) => format!(
            "note: the server cut this result to its content budget (32 KiB of path and line \
             text, and 512 bytes for each line, by default). A line is cut, or later matches \
             are dropped. {next}\n"
        ),
        (ResultTruncation::Other(_), _) => format!(
            "note: the server shortened this result ({}). {next}\n",
            truncation.header()
        ),
    }
}

pub(super) fn write_expansion_marker(out: &mut String, hit: &api::SearchHit) {
    match (hit.expanded, hit.expansion_reason) {
        (Some(true), _) => out.push_str(" [expanded]"),
        (Some(false), Some(reason)) => {
            let (name, why) = expansion_reason_text(reason);
            write!(out, " [not expanded: {name} — {why}]")
                .expect("writing to a String cannot fail");
        }
        (Some(false), None) => out.push_str(" [not expanded]"),
        (None, _) => {}
    }
}

pub(super) fn expansion_reason_text(reason: api::ExpansionReason) -> (&'static str, &'static str) {
    match reason {
        api::ExpansionReason::SymbolChunk => (
            "SymbolChunk",
            "the snippet is the function or type chunk itself",
        ),
        api::ExpansionReason::NoEnclosingSymbol => {
            ("NoEnclosingSymbol", "no named declaration contains the hit")
        }
        api::ExpansionReason::GraphLoading => (
            "GraphLoading",
            "the graph is not ready; search again later for the body",
        ),
        api::ExpansionReason::Deadline => (
            "Deadline",
            "the expansion deadline passed; use expand_chunk for more context",
        ),
        api::ExpansionReason::ContentBudget => (
            "ContentBudget",
            "the body does not fit in the content budget of the server; use expand_chunk",
        ),
        api::ExpansionReason::MultipleProjects => (
            "MultipleProjects",
            "a search of more than one codebase does not expand hits",
        ),
    }
}
