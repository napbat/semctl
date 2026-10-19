import { conceptRevision, lookupConcept, notifyColorObservers, type ConceptColor } from "./concept-colors";
import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { conceptPalette, normalizeConceptPath } from "./graph-data/palette.js";

type GraphNode = GraphData["nodes"][number];
type ColorMode = GraphAppearance["color"];
export interface GraphColor extends ConceptColor { source: "concept" | "structure" }
interface GroupColor { color: GraphColor; identity: string }
interface GroupBinding {
	scope: string | null;
	members: readonly GraphNode[];
	sharedRevision: number;
	shared?: GroupColor;
	structure?: GroupColor;
}
const groupBindings = new WeakMap<GraphNode, GroupBinding>();
const structuralColors = new WeakMap<GraphNode, GraphColor>();
const conceptColors = new WeakMap<ConceptColor, GraphColor>();
let currentMode: ColorMode = "concepts";
let revision = 0;

export function getGraphColorMode(): ColorMode { return currentMode; }
export function graphColorRevision(): number { return revision; }

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
		return (mode === "concepts" ? binding.shared : binding.structure)?.color;
	}
	const codebase = node.hit?.codebaseId ?? codebaseId;
	if (typeof codebase !== "string" || !codebase || codebase.length > 4096 || codebase.trim() !== codebase || /[\u0000-\u001f\u007f-\u009f]/.test(codebase)) return undefined;
	// An unbound aggregate's single path cannot establish the category of all its unseen members.
	if (node.category === "group") return undefined;
	const directoryNode = node.category === "directory" || node.category === "project";
	const source = node.hit?.path ?? node.path;
	const path = directoryNode && source === "" ? "" : normalizeConceptPath(source);
	if (path === undefined) return undefined;
	if (mode === "concepts" && !directoryNode) {
		const concept = lookupConcept(codebase, path);
		if (concept) {
			let color = conceptColors.get(concept);
			if (!color) {
				color = Object.freeze({ ...concept, source: "concept" });
				conceptColors.set(concept, color);
			}
			return color;
		}
	}
	const directory = directoryNode ? path : path.slice(0, Math.max(0, path.lastIndexOf("/")));
	const key = `structure:${JSON.stringify([codebase, directory])}`;
	let color = structuralColors.get(node);
	if (!color || color.key !== key) {
		color = Object.freeze({ key, label: directory || ".", ...conceptPalette(key), source: "structure",
			reason: "Reported source directory, shared across graph views in this codebase. Structural category, not semantic similarity, confidence, or embedding evidence." });
		structuralColors.set(node, color);
	}
	return color;
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
		const identity = binding ? (mode === "concepts" ? binding.shared : binding.structure)!.identity
			: JSON.stringify([member.hit?.codebaseId ?? codebaseId, color.source, color.key]);
		if (result && result.identity !== identity) return undefined;
		result ??= { color, identity };
	}
	return result;
}

/** A mixed or unresolved source aggregate stays neutral, including across codebase boundaries. */
export function graphGroupColor(codebaseId: string | null | undefined, members: readonly GraphNode[], mode: ColorMode = currentMode): GraphColor | undefined {
	return mode === "concepts" || mode === "structure" ? resolveGroup(codebaseId, members, mode)?.color : undefined;
}

/** Retain source membership for new evidence, while palette-only repaints reuse both cached resolutions. */
export function bindGraphGroupColors(node: GraphNode, codebaseId: string | null | undefined, members: readonly GraphNode[]): void {
	const retained = Object.freeze(members.slice());
	groupBindings.set(node, { scope: codebaseId ?? null, members: retained, sharedRevision: conceptRevision(),
		shared: resolveGroup(codebaseId, retained, "concepts"), structure: resolveGroup(codebaseId, retained, "structure") });
}
