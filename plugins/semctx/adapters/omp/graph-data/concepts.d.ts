import type { GraphData } from "../graph";
export interface InferredConcept {
	key: string;
	type: string;
	label: string;
	members: string[];
	symbols: { symbol: string; moniker: string | null; count: number; weight: number }[];
	projects: string[];
	directories: string[];
	relationCount: number;
	edges: number[];
}
export function inferConcepts(data: Pick<GraphData, "nodes" | "edges">): {
	available: boolean;
	reason: string;
	groups: InferredConcept[];
	assignments: Map<string, InferredConcept>;
	unassigned: string[];
	stats: { passes: number; visits: number };
};
