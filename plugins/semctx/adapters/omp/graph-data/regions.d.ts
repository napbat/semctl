import type { TermVector } from "./embedding.js";
/** "doc": a documentation family. "path": a shared home-folder prefix. "terms": recorded links with shared identifier terms. */
export type MapRegionKind = "doc" | "path" | "terms";
export interface MapRegion {
	key: string;
	/** Family label, folder prefix, or at most 2 shared terms joined with " · ". Unique in the result. */
	label: string;
	kind: MapRegionKind;
	/** Sorted group IDs; at least 2. */
	members: string[];
	/** The shared folder prefix of a "path" region. */
	folder?: string;
}
export interface MapRegionInput {
	id: string;
	/** Home folder from `homeFolder`; "" is the checkout root. */
	home: string;
	vector?: TermVector;
	family?: { key: string; label: string };
	/** Size for ordering, for example the member count. Default 1. */
	weight?: number;
}
/** The deepest folder that holds at least 60% of the folders. "" is the checkout root and the result for no folders. */
export function homeFolder(folders: Iterable<string | undefined>): string;
/** At most `limit` (default 6) regions, largest total weight first. Undirected link weights between group IDs. */
export function mapRegions(groups: readonly MapRegionInput[], links?: readonly { a: string; b: string; weight: number }[], options?: { limit?: number }): {
	regions: MapRegion[];
	regionOf: Map<string, MapRegion>;
};
