import type { Display } from "./inspector";
import { inferConcepts } from "./graph-data/concepts.js";
import { docConcepts, type DocConceptFamily, type DocConceptVia, type DocSource } from "./graph-data/doc-concepts.js";
import type { TermVector } from "./graph-data/embedding.js";
import { conceptPalette, normalizeConceptPath } from "./graph-data/palette.js";

/** `via` is per file; the other fields belong to the concept. */
export interface ConceptColor {
	key: string;
	label: string;
	light: string;
	dark: string;
	reason: string;
	kind: "doc" | "reference";
	title?: string;
	source?: string;
	via?: DocConceptVia;
	terms?: TermVector;
	family?: DocConceptFamily;
}
let validatedOverviews = new WeakMap<Display, number>();
const acceptedGenerations = new Map<string, number>();
const assignments = new Map<string, Map<string, ConceptColor>>();
const groupColors = new WeakMap<ConceptColor, ConceptColor>();
let revision = 0;
interface Listener { owner: WeakRef<object>; repaint: (owner: object) => void }
const listeners = new Set<Listener>();
const finalizedOwners = new FinalizationRegistry<Listener>(entry => { listeners.delete(entry); });

/** The display boundary calls this only after full local overview validation. Acceptance orders all request instances. */
export function acceptConceptOverview(display: Display): void {
	const codebase = display.identity.codebaseId;
	if (!codebase) return;
	const generation = (acceptedGenerations.get(codebase) ?? 0) + 1;
	acceptedGenerations.set(codebase, generation);
	validatedOverviews.set(display, generation);
}
export function conceptRevision(): number { return revision; }
export function lookupConcept(codebaseId: string | null | undefined, path: string | null | undefined): ConceptColor | undefined {
	const normalized = normalizeConceptPath(path);
	return codebaseId && normalized ? assignments.get(codebaseId)?.get(normalized) : undefined;
}
/** The color that a group of the concept shows. It has no per-file `via`. */
export function groupConcept(color: ConceptColor): ConceptColor { return groupColors.get(color) ?? color; }

/** Callbacks must not capture their owner. Owners and their registrations can be collected. */
export function subscribeConcepts<T extends object>(owner: T, repaint: (owner: T) => void): () => void {
	for (const entry of listeners) if (!entry.owner.deref()) listeners.delete(entry);
	const entry = { owner: new WeakRef<object>(owner), repaint: repaint as (owner: object) => void };
	listeners.add(entry);
	finalizedOwners.register(owner, entry, entry);
	return () => { listeners.delete(entry); finalizedOwners.unregister(entry); };
}
/** Repaint category cues without changing evidence/grouping revisions (for profile palette changes). */
export function notifyColorObservers(): void {
	for (const entry of listeners) {
		const owner = entry.owner.deref();
		if (owner) entry.repaint(owner); else listeners.delete(entry);
	}
}
function changed(): void {
	revision++;
	notifyColorObservers();
}

/** Doc concepts assign the files and directories that they name; reference communities fill only the files that remain. */
function conceptAssignments(display: Extract<Display, { kind: "graph" }>, docs: readonly DocSource[]): Map<string, ConceptColor> {
	const paths = new Map<string, string>(), files = new Set<string>();
	for (const node of display.nodes) {
		const path = normalizeConceptPath(node.path);
		if (!path || (node.category !== "file" && node.category !== "directory")) continue;
		paths.set(node.id, path);
		if (node.category === "file") files.add(node.id);
	}
	const next = new Map<string, ConceptColor>();
	if (docs.length) {
		const result = docConcepts(display, docs), concepts = new Map(result.concepts.map(concept => [concept.key, Object.freeze<ConceptColor>({
			key: concept.key, label: concept.label, ...conceptPalette(concept.key),
			reason: `Named by ${concept.source}. Docs and catalog-path evidence, not confidence or semantic similarity.`,
			kind: "doc", title: concept.title, source: concept.source, terms: concept.terms, ...(concept.family ? { family: concept.family } : {}),
		})]));
		const routes = new Map<string, ConceptColor>();
		for (const [id, assignment] of result.assignments) {
			const path = paths.get(id), shared = concepts.get(assignment.key);
			if (!path || !shared) continue;
			const route = JSON.stringify([assignment.key, assignment.via]);
			let color = routes.get(route);
			if (!color) {
				color = Object.freeze({ ...shared, via: assignment.via });
				groupColors.set(color, shared);
				routes.set(route, color);
			}
			next.set(path, color);
		}
	}
	// Indexing gaps truncate the overview without removing catalog paths, but they remove reference evidence.
	if (!display.truncated) {
		const concepts = inferConcepts(display);
		for (const group of concepts.groups) {
			const color = Object.freeze({ key: group.key, label: group.label, ...conceptPalette(group.key), reason: concepts.reason, kind: "reference" as const });
			for (const id of group.members) {
				const path = files.has(id) ? paths.get(id) : undefined;
				if (path && !next.has(path)) next.set(path, color);
			}
		}
	}
	return next;
}

/** Replace one codebase's evidence. Unsupported nodes receive no assignment. A truncated display without docs keeps the last evidence. */
export function setConceptSnapshot(display: Display, docs: readonly DocSource[] = []): void {
	const generation = validatedOverviews.get(display), codebase = display.identity.codebaseId;
	if (generation === undefined || !codebase || generation !== acceptedGenerations.get(codebase) ||
		display.kind !== "graph" || display.loading || (display.truncated && !docs.length) ||
		display.identity.copy !== "checkout" || !display.identity.localRoot) return;
	assignments.set(codebase, conceptAssignments(display, docs));
	changed();
}
export function resetConcepts(): void {
	assignments.clear();
	acceptedGenerations.clear();
	validatedOverviews = new WeakMap();
	changed();
}
