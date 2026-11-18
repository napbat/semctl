import { conceptRevision, groupConcept, lookupConcept, notifyColorObservers, type ConceptColor } from "./concept-colors";
import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { conceptPalette, normalizeConceptPath } from "./graph-data/palette.js";

type GraphNode = GraphData["nodes"][number];
type ColorMode = GraphAppearance["color"];
/** `concept` carries the concept evidence; structure colors have none. */
export interface GraphColor extends Pick<ConceptColor, "key" | "label" | "light" | "dark" | "reason"> { source: "concept" | "structure"; concept?: ConceptColor }
interface GroupColor { color: GraphColor; identity: string }
interface GroupBinding {
	scope: string | null;
	members: readonly GraphNode[];
	sharedRevision: number;
	shared?: GroupColor;
	structure?: GroupColor;
	/** Home-folder color of a source-structure group, used when member colors are mixed. */
	folder?: GroupColor;
	/** The own color of a content subconcept; it takes precedence over member colors. */
	own?: GroupColor;
}
const groupBindings = new WeakMap<GraphNode, GroupBinding>();
const structuralColors = new WeakMap<GraphNode, GraphColor>();
const conceptColors = new WeakMap<ConceptColor, GraphColor>();
let currentMode: ColorMode = "concepts";
let revision = 0;

export function getGraphColorMode(): ColorMode { return currentMode; }
export function graphColorRevision(): number { return revision; }
function conceptColor(concept: ConceptColor): GraphColor {
	let color = conceptColors.get(concept);
	if (!color) {
		color = Object.freeze({ key: concept.key, label: concept.label, light: concept.light, dark: concept.dark, reason: concept.reason, source: "concept", concept });
		conceptColors.set(concept, color);
	}
	return color;
}

/** Category colors are profile-wide; changing them repaints without changing graph evidence or grouping. */
export function setGraphColorMode(mode: ColorMode): void {
	if ((mode !== "concepts" && mode !== "structure" && mode !== "neutral") || mode === currentMode) return;
	currentMode = mode;
	revision++;
	notifyColorObservers();
}

/** A source directory identifies structural color; query ids, labels, status and result order never do. */
export function lookupGraphColor(codebaseId: string | null | undefined, node: GraphNode, mode: ColorMode = currentMode): GraphColor | undefined {
	if (mode !== "concepts" && mode !== "structure") return undefined;
	const binding = groupBindings.get(node);
	if (binding) {
		if (binding.scope !== (codebaseId ?? null)) return undefined;
		if (mode === "concepts" && binding.sharedRevision !== conceptRevision()) {
			// Mark before descending so even accidentally cyclic wrapper bindings stay neutral.
			binding.sharedRevision = conceptRevision();
			binding.shared = undefined;
			binding.shared = resolveGroup(binding.scope, binding.members, "concepts");
		}
		return binding.own?.color ?? (mode === "concepts" ? binding.shared : binding.structure)?.color ?? binding.folder?.color;
	}
	const codebase = node.hit?.codebaseId ?? codebaseId;
	if (typeof codebase !== "string" || !codebase || codebase.length > 4096 || codebase.trim() !== codebase || /[\u0000-\u001f\u007f-\u009f]/.test(codebase)) return undefined;
	// An unbound aggregate's single path cannot establish the category of all its unseen members.
	if (node.category === "group") return undefined;
	const directoryNode = node.category === "directory" || node.category === "project";
	const source = node.hit?.path ?? node.path;
	const path = directoryNode && source === "" ? "" : normalizeConceptPath(source);
	if (path === undefined) return undefined;
	// Doc concepts also name directories and project roots by path.
	if (mode === "concepts") {
		const concept = lookupConcept(codebase, path);
		if (concept) return conceptColor(concept);
	}
	const directory = directoryNode ? path : path.slice(0, Math.max(0, path.lastIndexOf("/")));
	let color = structuralColors.get(node);
	if (!color || color.key !== folderKey(codebase, directory)) {
		color = folderColor(codebase, directory);
		structuralColors.set(node, color);
	}
	return color;
}

const folderKey = (codebase: string, directory: string) => `structure:${JSON.stringify([codebase, directory])}`;
/** One structural color per codebase folder. Chips, graph dots and folder-based group dots share it. */
function folderColor(codebase: string, directory: string): GraphColor {
	const key = folderKey(codebase, directory);
	return Object.freeze({ key, label: directory || ".", ...conceptPalette(key), source: "structure",
		reason: "Reported source directory, shared across graph views in this codebase. Structural category, not semantic similarity, confidence, or embedding evidence." });
}

function resolveGroup(codebaseId: string | null | undefined, members: readonly GraphNode[], mode: "concepts" | "structure"): GroupColor | undefined {
	let result: GroupColor | undefined;
	for (const member of members) {
		const binding = groupBindings.get(member);
		const color = lookupGraphColor(codebaseId, member, mode);
		if (!color) {
			// Only an explicitly synthetic, unbound wrapper with no source evidence can be skipped.
			if (!binding && member.category === "group" && !member.hit && member.path === undefined && member.project === undefined) continue;
			return undefined;
		}
		const bound = binding && (binding.own ?? (mode === "concepts" ? binding.shared : binding.structure) ?? binding.folder);
		const identity = bound ? bound.identity : JSON.stringify([member.hit?.codebaseId ?? codebaseId, color.source, color.key]);
		if (result && result.identity !== identity) return undefined;
		result ??= { color, identity };
	}
	// A group shows the concept itself, never the assignment route of one member.
	const concept = result?.color.concept && groupConcept(result.color.concept);
	return result && concept && concept !== result.color.concept ? { color: conceptColor(concept), identity: result.identity } : result;
}

/**
 * A mixed or unresolved source aggregate stays neutral, including across codebase boundaries.
 * A source-structure group passes its home `folder`; mixed members then show that folder's color.
 * A content subconcept passes `own`: its key names one palette color, so sibling clusters in one folder stay apart.
 */
export function graphGroupColor(codebaseId: string | null | undefined, members: readonly GraphNode[], mode: ColorMode = currentMode, folder?: string, own?: OwnColor): GraphColor | undefined {
	if (mode !== "concepts" && mode !== "structure") return undefined;
	return ownColor(codebaseId, own)?.color ?? resolveGroup(codebaseId, members, mode)?.color ?? folderFallback(codebaseId, folder)?.color;
}
function folderFallback(codebaseId: string | null | undefined, folder: string | undefined): GroupColor | undefined {
	if (typeof codebaseId !== "string" || !codebaseId || folder === undefined) return undefined;
	const color = folderColor(codebaseId, folder);
	return { color, identity: JSON.stringify([codebaseId, color.source, color.key]) };
}
/** The key and label of a content subconcept color. */
export interface OwnColor { key: string; label: string }
function ownColor(codebaseId: string | null | undefined, own: OwnColor | undefined): GroupColor | undefined {
	if (!own) return undefined;
	const key = `subconcept:${JSON.stringify([codebaseId ?? null, own.key])}`;
	const color: GraphColor = Object.freeze({ key, label: own.label, ...conceptPalette(key), source: "structure",
		reason: "Content cluster inside a group: shared terms, links and semantic search profiles. Not a dependency, confidence or similarity score." });
	return { color, identity: key };
}

/**
 * Retain source membership for new evidence, while palette-only repaints reuse both cached resolutions.
 * `folder` is the home folder of a source-structure group. `own` gives a content subconcept its own color.
 */
export function bindGraphGroupColors(node: GraphNode, codebaseId: string | null | undefined, members: readonly GraphNode[], folder?: string, own?: OwnColor): void {
	const retained = Object.freeze(members.slice());
	groupBindings.set(node, { scope: codebaseId ?? null, members: retained, sharedRevision: conceptRevision(),
		shared: resolveGroup(codebaseId, retained, "concepts"), structure: resolveGroup(codebaseId, retained, "structure"),
		folder: folderFallback(codebaseId, folder), own: ownColor(codebaseId, own) });
}
