import type { Display } from "./inspector";
import { inferConcepts } from "./graph-data/concepts.js";
import { conceptPalette, normalizeConceptPath } from "./graph-data/palette.js";

export interface ConceptColor { key: string; label: string; light: string; dark: string; reason: string }
const validatedOverviews = new WeakSet<Display>();
const assignments = new Map<string, Map<string, ConceptColor>>();
let revision = 0;
interface Listener { owner: WeakRef<object>; repaint: (owner: object) => void }
const listeners = new Set<Listener>();
const finalizedOwners = new FinalizationRegistry<Listener>(entry => { listeners.delete(entry); });

/** The display boundary calls this only after full local overview validation. */
export function acceptConceptOverview(display: Display): void { validatedOverviews.add(display); }
export function conceptRevision(): number { return revision; }
export function lookupConcept(codebaseId: string | null | undefined, path: string | null | undefined): ConceptColor | undefined {
	const normalized = normalizeConceptPath(path);
	return codebaseId && normalized ? assignments.get(codebaseId)?.get(normalized) : undefined;
}

/** Callbacks must not capture their owner. Owners and their registrations can be collected. */
export function subscribeConcepts<T extends object>(owner: T, repaint: (owner: T) => void): () => void {
	for (const entry of listeners) if (!entry.owner.deref()) listeners.delete(entry);
	const entry = { owner: new WeakRef<object>(owner), repaint: repaint as (owner: object) => void };
	listeners.add(entry);
	finalizedOwners.register(owner, entry, entry);
	return () => { listeners.delete(entry); finalizedOwners.unregister(entry); };
}
function changed(): void {
	revision++;
	for (const entry of listeners) {
		const owner = entry.owner.deref();
		if (owner) entry.repaint(owner); else listeners.delete(entry);
	}
}

/** Replace one codebase's complete evidence; unsupported files receive no assignment. */
export function setConceptSnapshot(display: Display): void {
	if (!validatedOverviews.has(display) || display.kind !== "graph" || display.truncated ||
		display.identity.copy !== "checkout" || !display.identity.localRoot || !display.identity.codebaseId) return;
	const codebaseId = display.identity.codebaseId;
	const files = new Map<string, string>();
	for (const node of display.nodes) {
		const path = normalizeConceptPath(node.path);
		if (node.category === "file" && path) files.set(node.id, path);
	}
	const concepts = inferConcepts(display), next = new Map<string, ConceptColor>();
	for (const group of concepts.groups) {
		const color = Object.freeze({ key: group.key, label: group.label, ...conceptPalette(group.key), reason: concepts.reason });
		for (const id of group.members) {
			const path = files.get(id);
			if (path) next.set(path, color);
		}
	}
	assignments.set(codebaseId, next);
	changed();
}
export function resetConcepts(): void {
	assignments.clear();
	changed();
}
