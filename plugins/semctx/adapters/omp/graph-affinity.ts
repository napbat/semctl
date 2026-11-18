/**
 * Semantic affinity between files from semctx search profiles (`semctl graph --json affinity`).
 * Each file is represented by the ranked results of one search that describes it. Related files share results or retrieve each other.
 * The overview uses affinity to organize and place subconcepts. Affinity is not a dependency, a confidence or a proof of behavior.
 */
import { normalizeConceptPath } from "./graph-data/palette.js";
import { readWarnings } from "./partial-answers";

export const AFFINITY_MAX_FILES = 64;
type SearchEvidence = "self_match" | "other_matches" | "empty" | "loading";
export interface AffinityFile {
	path: string; query: "content" | "outline" | "name"; results: number; search: SearchEvidence;
	/** Compatibility with the scope's self-match count. This is not an index coverage signal. */
	indexed: boolean;
}
export interface AffinityPair { a: string; b: string; direct: number; shared: number }
type AffinityOutcome = "ready" | "partial" | "empty" | "backend-loading";
export interface Affinity {
	files: ReadonlyMap<string, AffinityFile>; pairs: readonly AffinityPair[]; caveats: readonly string[];
	status: AffinityOutcome; retryable: boolean;
}
export type AffinityStatus = "loading" | "failed" | AffinityOutcome;
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
	// Even a loading search has a file entry. A missing entry cannot establish a completed scope.
	if (display.files.length !== allowed.size) return undefined;
	let loading = 0, haveResults = false;
	for (const file of display.files) {
		if (!object(file)) return undefined;
		const { path, query, results } = file;
		if (typeof path !== "string" || !allowed.has(path) || files.has(path) || !querySource(query) ||
			typeof results !== "number" || !Number.isSafeInteger(results) || results < 0) return undefined;
		let search = file.search;
		if (search === undefined && typeof file.indexed === "boolean") {
			// Version 1 used `indexed` for a self-match. Only its per-file caveat distinguished a loading search from an empty one.
			search = file.indexed ? "self_match" : results > 0 ? "other_matches"
				: display.caveats.some(caveat => caveat.startsWith(`Search for ${path} unavailable while the backend loads:`)) ? "loading" : "empty";
		}
		if (search === "loading" || search === "empty") {
			if (results !== 0) return undefined;
		} else if ((search !== "self_match" && search !== "other_matches") || results === 0) return undefined;
		if (search === "loading") loading++;
		haveResults ||= results > 0;
		files.set(path, { path, query, results, search, indexed: search === "self_match" });
	}
	const pairs: AffinityPair[] = [];
	for (const pair of display.pairs) {
		// The CLI orders by bytes and JavaScript by UTF-16 units, so only distinct endpoints are required.
		if (!object(pair) || typeof pair.a !== "string" || typeof pair.b !== "string" || pair.a === pair.b ||
			!files.has(pair.a) || !files.has(pair.b) || !unit(pair.direct) || !unit(pair.shared)) return undefined;
		pairs.push({ a: pair.a, b: pair.b, direct: pair.direct, shared: pair.shared });
	}
	const status = loading > 0 && loading === files.size ? "backend-loading"
		: loading > 0 || warnings.length ? "partial" : haveResults ? "ready" : "empty";
	const retryable = loading > 0 || warnings.some(warning => warning.endsWith("the server still builds the missing part, so a later call can return more"));
	return { files, pairs, caveats: [...warnings, ...display.caveats], status, retryable };
}

/** One weight per pair: the stronger of direct retrieval and shared results. */
export function affinityWeight(pair: AffinityPair): number { return Math.max(pair.direct, pair.shared); }

interface Entry { state: AffinityState; controller?: AbortController; retryAfter?: number }
interface Listener { owner: WeakRef<object>; repaint: (owner: object) => void }
const CACHE_LIMIT = 32;
const LOADING_RETRY_MS = 10_000;
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
 * A new scope request can retry a transient result after a cooldown. The cache does not poll.
 * Undefined when no runner exists or the path set is outside the bounds. Subscribers repaint when a request settles.
 */
export function requestAffinity(codebaseId: string | null | undefined, paths: readonly string[]): AffinityState | undefined {
	const unique = [...new Set(paths.map(path => normalizeConceptPath(path)).filter((path): path is string => path !== undefined))].sort();
	if (!runner || !codebaseId || unique.length < 2 || unique.length > AFFINITY_MAX_FILES) return undefined;
	const key = JSON.stringify([codebaseId, unique]), cached = cache.get(key);
	if (cached) {
		cache.delete(key);
		if (cached.retryAfter === undefined || Date.now() < cached.retryAfter) {
			cache.set(key, cached);
			return cached.state;
		}
	}
	const controller = new AbortController(), entry: Entry = { state: { status: "loading" }, controller }, owner = generation;
	cache.set(key, entry);
	while (cache.size > CACHE_LIMIT) { const [oldest, stale] = cache.entries().next().value!; stale.controller?.abort(); cache.delete(oldest); }
	const settle = (state: AffinityState) => {
		if (owner !== generation || cache.get(key) !== entry) return;
		entry.state = state; entry.controller = undefined;
		if (state.affinity?.retryable) entry.retryAfter = Date.now() + LOADING_RETRY_MS;
		notify();
	};
	runner(unique, controller.signal).then(value => {
		const affinity = readAffinity(value, unique);
		settle(affinity ? { status: affinity.status, affinity } : { status: "failed", error: "semctl returned an invalid affinity result." });
	}, (error: unknown) => {
		if (!controller.signal.aborted) settle({ status: "failed", error: error instanceof Error ? error.message : String(error) });
	});
	return entry.state;
}
