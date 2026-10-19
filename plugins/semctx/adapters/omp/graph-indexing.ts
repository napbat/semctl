import type { GraphData } from "./graph";

export type GraphIndexing = "pending" | "processing" | "ready" | "failed" | "unknown";
export type GraphIndexingCounts = Record<GraphIndexing | "unreported", number>;
const states: readonly GraphIndexing[] = ["failed", "processing", "pending", "ready", "unknown"];

/** Explicit metadata is evidence; legacy source nodes stay unreported, synthetic hierarchy does not. */
export function indexingCounts(nodes: readonly GraphData["nodes"][number][]): GraphIndexingCounts {
	const counts: GraphIndexingCounts = { pending: 0, processing: 0, ready: 0, failed: 0, unknown: 0, unreported: 0 };
	for (const node of nodes) {
		if (node.indexing === undefined && !node.hit &&
			(node.category === "directory" || node.category === "project" || node.category === "group")) continue;
		counts[node.indexing ?? "unreported"]++;
	}
	return counts;
}
export function indexingSummary(counts: GraphIndexingCounts): string | undefined {
	if (!states.some(state => counts[state] > 0)) return undefined;
	return `Indexing: ${[...states, "unreported" as const].filter(state => counts[state] > 0).map(state => `${counts[state]} ${state}`).join(" · ")}`;
}
export function groupIndexing(counts: GraphIndexingCounts): GraphIndexing | undefined {
	for (const state of ["failed", "processing", "pending"] as const) if (counts[state]) return state;
	if (counts.unknown) return "unknown";
	return counts.ready ? counts.unreported ? "unknown" : "ready" : undefined;
}
export function indexingText(node: GraphData["nodes"][number]): string | undefined {
	const status = node.indexing;
	if (status === undefined) return undefined;
	const label = node.category === "group"
		? status === "pending" ? "Group includes pending embeddings (not processing)."
			: status === "processing" ? "Group includes processing embeddings."
			: status === "failed" ? "Group includes failed embeddings."
			: status === "ready" ? "All group embeddings ready." : "Embedding status unknown for some group members."
		: status === "pending" ? "Embedding pending (not processing)."
			: status === "processing" ? "Embedding processing."
			: status === "failed" ? "Embedding failed."
			: status === "ready" ? "Embedding ready." : "Embedding status unknown.";
	return `${label}${node.indexingError ? `\n${node.indexingError}` : ""}`;
}
