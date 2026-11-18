/**
 * Semantic affinity between files from semctx search profiles (`semctl graph --json affinity`).
 * Each file is represented by the ranked results of one search that describes it. Related files share results or retrieve each other.
 * The overview uses affinity to organize and place subconcepts. Affinity is not a dependency, a confidence or a proof of behavior.
 */
import { normalizeConceptPath } from "./graph-data/palette.js";
import { readWarnings } from "./partial-answers";

export const AFFINITY_MAX_FILES = 64;
export interface AffinityFile { path: string; query: "content" | "outline" | "name"; indexed: boolean; results: number }
export interface AffinityPair { a: string; b: string; direct: number; shared: number }
export interface Affinity { files: ReadonlyMap<string, AffinityFile>; pairs: readonly AffinityPair[]; caveats: readonly string[] }
export type AffinityStatus = "loading" | "ready" | "failed";
export interface AffinityState { status: AffinityStatus; affinity?: Affinity; error?: string }
/** Runs the read-only CLI for one set of codebase-relative paths and returns its parsed JSON. */
export type AffinityRunner = (paths: readonly string[], signal: AbortSignal) => Promise<unknown>;

const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const unit = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1;
const QUERY_SOURCES = ["content", "outline", "name"] as const;
const querySource = (value: unknown): value is AffinityFile["query"] => QUERY_SOURCES.some(source => source === value);

/** Validates the CLI `{ text, display }` output. Every pair must name two requested files. */
export function readAffinity(value: unknown, requested: readonly string[]): Affinity | undefined {
	if (!object(value)) return undefined;
	const display = value.display, warnings = readWarnings(value.warnings);
	if (!warnings || !object(display) || display.version !== 1 || display.kind !== "affinity" || !Array.isArray(display.files) || !Array.isArray(display.pairs) ||
		!Array.isArray(display.caveats) || !display.caveats.every(caveat => typeof caveat === "string")) return undefined;
	const allowed = new Set(requested), files = new Map<string, AffinityFile>();
	for (const file of display.files) {
		if (!object(file) || typeof file.path !== "string" || !allowed.has(file.path) || files.has(file.path) || !querySource(file.query) ||
			typeof file.indexed !== "boolean" || typeof file.results !== "number" || !Number.isSafeInteger(file.results) || file.results < 0) return undefined;
		files.set(file.path, { path: file.path, query: file.query, indexed: file.indexed, results: file.results });
	}
	const pairs: AffinityPair[] = [];
	for (const pair of display.pairs) {
		// The CLI orders by bytes and JavaScript by UTF-16 units, so only distinct endpoints are required.
		if (!object(pair) || typeof pair.a !== "string" || typeof pair.b !== "string" || pair.a === pair.b ||
			!files.has(pair.a) || !files.has(pair.b) || !unit(pair.direct) || !unit(pair.shared)) return undefined;
		pairs.push({ a: pair.a, b: pair.b, direct: pair.direct, shared: pair.shared });
	}
	return { files, pairs, caveats: [...warnings, ...display.caveats] };
}

/** One weight per pair: the stronger of direct retrieval and shared results. */
export function affinityWeight(pair: AffinityPair): number { return Math.max(pair.direct, pair.shared); }

interface Entry { state: AffinityState; controller?: AbortController }
interface Listener { owner: WeakRef<object>; repaint: (owner: object) => void }
const CACHE_LIMIT = 32;
const cache = new Map<string, Entry>();
const listeners = new Set<Listener>();
let runner: AffinityRunner | undefined;
let generation = 0;

function notify(): void {
	for (const entry of listeners) {
		const owner = entry.owner.deref();
		if (owner) entry.repaint(owner); else listeners.delete(entry);
	}
}

/** The session's runner. Replacing or clearing it cancels requests in flight and drops cached results. */
export function setAffinityRunner(next: AffinityRunner | undefined): void {
	generation++;
	for (const entry of cache.values()) entry.controller?.abort();
	cache.clear();
	runner = next;
}

/** Callbacks must not capture their owner. Owners and their registrations can be collected. */
export function subscribeAffinity<T extends object>(owner: T, repaint: (owner: T) => void): () => void {
	const entry: Listener = { owner: new WeakRef<object>(owner), repaint: repaint as (owner: object) => void };
	listeners.add(entry);
	return () => { listeners.delete(entry); };
}

/**
 * The affinity of 2 to 64 files of one codebase. The first call starts one request; later calls return its state.
 * Undefined when no runner exists or the path set is outside the bounds. Subscribers repaint when a request settles.
 */
export function requestAffinity(codebaseId: string | null | undefined, paths: readonly string[]): AffinityState | undefined {
	const unique = [...new Set(paths.map(path => normalizeConceptPath(path)).filter((path): path is string => path !== undefined))].sort();
	if (!runner || !codebaseId || unique.length < 2 || unique.length > AFFINITY_MAX_FILES) return undefined;
	const key = JSON.stringify([codebaseId, unique]), cached = cache.get(key);
	if (cached) { cache.delete(key); cache.set(key, cached); return cached.state; }
	const controller = new AbortController(), entry: Entry = { state: { status: "loading" }, controller }, owner = generation;
	cache.set(key, entry);
	while (cache.size > CACHE_LIMIT) { const [oldest, stale] = cache.entries().next().value!; stale.controller?.abort(); cache.delete(oldest); }
	const settle = (state: AffinityState) => {
		if (owner !== generation || cache.get(key) !== entry) return;
		entry.state = state; entry.controller = undefined;
		notify();
	};
	runner(unique, controller.signal).then(value => {
		const affinity = readAffinity(value, unique);
		settle(affinity ? { status: "ready", affinity } : { status: "failed", error: "semctl returned an invalid affinity result." });
	}, (error: unknown) => {
		if (!controller.signal.aborted) settle({ status: "failed", error: error instanceof Error ? error.message : String(error) });
	});
	return entry.state;
}
