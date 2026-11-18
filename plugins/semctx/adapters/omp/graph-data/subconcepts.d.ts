import type { TermVector } from "./embedding.js";
/** "folder": a child folder of the members' common folder. "content": a cluster of terms, links and semantic affinity. */
export type SubconceptKind = "folder" | "content";
export interface Subconcept {
	/** `folder:<path>` or `content:<first member name>`. Stable while membership is stable. */
	key: string;
	label: string;
	kind: SubconceptKind;
	/** Item IDs in name order. */
	members: string[];
	/** The child folder of a folder subconcept. */
	folder?: string;
	/** At most 3 terms that separate this subconcept from its siblings and that its label does not already name. */
	terms: string[];
}
export interface SubconceptItem {
	id: string;
	/** Stable identity, such as the codebase-relative path. */
	name: string;
	/** "" is the checkout root. */
	folder: string;
	vector?: TermVector;
}
/** The most content clusters for n members: about √(n / 2), at least 2 and at most `limit`. A clear similarity drop can keep fewer. */
export function clusterCount(n: number, limit: number): number;
/**
 * Deterministic partition: input order does not change the result. Every item belongs to exactly one subconcept.
 * Largest first; folders beyond `limit` fold into one last subconcept with the key `folder:<common folder>/*`.
 */
export function partitionSubconcepts(items: readonly SubconceptItem[], options?: {
	affinity?: readonly { a: string; b: string; weight: number }[];
	links?: readonly { a: string; b: string; weight: number }[];
	limit?: number;
}): Subconcept[];
