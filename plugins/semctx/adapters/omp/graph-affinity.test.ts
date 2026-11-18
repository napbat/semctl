import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { readAffinity, requestAffinity, setAffinityRunner, subscribeAffinity, type AffinityRunner } from "./graph-affinity";

const display = (pairs: unknown[], files = ["docs/a.md", "docs/b.md"]) => ({ text: "", display: { version: 1, kind: "affinity",
	files: files.map(path => ({ path, query: "content", indexed: path.endsWith("a.md"), results: 3 })), pairs, caveats: ["Retrieval evidence."] } });
const deferred = () => Promise.withResolvers<unknown>();

afterEach(() => setAffinityRunner(undefined));

describe("semantic affinity", () => {
	test("a result is accepted only when every file and pair names a requested path with unit scores", () => {
		const requested = ["docs/a.md", "docs/b.md"];
		const affinity = readAffinity(display([{ a: "docs/a.md", b: "docs/b.md", direct: .25, shared: .5 }]), requested);
		expect(affinity?.pairs).toEqual([{ a: "docs/a.md", b: "docs/b.md", direct: .25, shared: .5 }]);
		expect(affinity?.files.get("docs/a.md")).toEqual({ path: "docs/a.md", query: "content", indexed: true, results: 3, search: "self_match" });
		expect(affinity?.files.get("docs/b.md")?.search).toBe("other_matches");
		expect(readAffinity({ ...display([]), warnings: ["Providers loading"] }, requested)?.caveats).toEqual(["Providers loading", "Retrieval evidence."]);
		expect(readAffinity(display([{ a: "docs/a.md", b: "docs/c.md", direct: 0, shared: .5 }]), requested)).toBeUndefined();
		expect(readAffinity(display([{ a: "docs/a.md", b: "docs/b.md", direct: 0, shared: 1.5 }]), requested)).toBeUndefined();
		expect(readAffinity(display([{ a: "docs/a.md", b: "docs/a.md", direct: 0, shared: .5 }]), requested)).toBeUndefined();
		expect(readAffinity(display([], ["docs/a.md", "docs/z.md"]), requested)).toBeUndefined();
		expect(readAffinity(display([], ["docs/a.md"]), requested)).toBeUndefined();
		expect(readAffinity({ display: { ...display([]).display, kind: "graph" } }, requested)).toBeUndefined();
	});
	test("legacy zero result counts respect per-file loading caveats", () => {
		const paths = ["docs/a.md", "docs/b.md"];
		const legacy = display([]);
		legacy.display.files = paths.map(path => ({ path, query: "name", indexed: false, results: 0 }));
		legacy.display.caveats.push("Search for docs/b.md unavailable while the backend loads: GraphLoading");
		const affinity = readAffinity(legacy, paths);
		expect(affinity?.status).toBe("partial");
		expect(affinity?.retryable).toBe(true);
		expect(affinity?.files.get(paths[0])?.search).toBe("empty");
		expect(affinity?.files.get(paths[1])?.search).toBe("loading");
	});
	test("typed search evidence preserves partial results and distinguishes empty, loading and invalid results", () => {
		const [a, b] = ["docs/a.md", "docs/b.md"];
		const otherMatches = { path: a, query: "content", search: "other_matches", indexed: false, results: 3 };
		const loading = { path: b, query: "name", search: "loading", indexed: false, results: 0 };
		const pair = { a, b, direct: .5, shared: 0 };
		const result = (files: unknown[], pairs: unknown[] = []) => ({ display: { ...display(pairs).display, files } });
		const affinity = readAffinity(result([otherMatches, loading], [pair]), [a, b]);
		expect(affinity?.status).toBe("partial");
		expect(affinity?.pairs).toEqual([pair]);
		expect(readAffinity(result([otherMatches, { ...loading, results: 2 }]), [a, b])).toBeUndefined();
		expect(readAffinity(result([otherMatches, { ...loading, search: "unknown" }]), [a, b])).toBeUndefined();
		expect(readAffinity(result([loading, { ...loading, path: a }]), [a, b])?.status).toBe("backend-loading");
		const empty = { ...loading, search: "empty" };
		expect(readAffinity(result([empty, { ...empty, path: a }]), [a, b])?.status).toBe("empty");
	});
	test("partial warnings stay in caveats and only known transient warnings permit a retry", () => {
		const paths = ["docs/a.md", "docs/b.md"];
		const permanent = "the graph answer is partial (missing provider); a retry returns the same result";
		const pending = "the graph answer is partial (restoring); the server still builds the missing part, so a later call can return more";
		expect(readAffinity({ ...display([]), warnings: [permanent] }, paths)).toMatchObject({
			status: "partial", retryable: false, caveats: [permanent, "Retrieval evidence."],
		});
		expect(readAffinity({ ...display([]), warnings: [pending] }, paths)).toMatchObject({ status: "partial", retryable: true });
	});
	test("one file set runs once, settles for subscribers and stays cached in any path order", async () => {
		const calls: string[][] = [], pending = deferred();
		setAffinityRunner(paths => { calls.push([...paths]); return pending.promise; });
		const owner = { repaints: 0 };
		subscribeAffinity(owner, current => { current.repaints++; });
		expect(requestAffinity("hv", ["docs/b.md", "docs/a.md", "docs/a.md"])).toEqual({ status: "loading" });
		expect(requestAffinity("hv", ["docs/a.md", "docs/b.md"])).toEqual({ status: "loading" });
		expect(calls).toEqual([["docs/a.md", "docs/b.md"]]);
		pending.resolve(display([{ a: "docs/a.md", b: "docs/b.md", direct: 0, shared: .4 }]));
		await pending.promise; await Promise.resolve();
		expect(owner.repaints).toBe(1);
		expect(requestAffinity("hv", ["docs/b.md", "docs/a.md"])?.status).toBe("ready");
		expect(requestAffinity("other", ["docs/a.md", "docs/b.md"])?.status).toBe("loading");
		expect(calls).toHaveLength(2);
	});
	test("a loading response is cached briefly, then retried on demand without polling", async () => {
		const clock = spyOn(Date, "now").mockReturnValue(0);
		try {
			const paths = ["docs/a.md", "docs/b.md"];
			const pending = deferred(), next = deferred();
			let runs = 0;
			setAffinityRunner(() => (++runs === 1 ? pending : next).promise);
			expect(requestAffinity("hv", paths)?.status).toBe("loading");
			pending.resolve({ display: { ...display([]).display,
				files: paths.map(path => ({ path, query: "name", indexed: false, results: 0, search: "loading" })) } });
			await pending.promise; await Promise.resolve();
			expect(requestAffinity("hv", paths)?.status).toBe("backend-loading");
			expect(runs).toBe(1);
			clock.mockReturnValue(10_000);
			expect(runs).toBe(1);
			expect(requestAffinity("hv", paths)?.status).toBe("loading");
			expect(requestAffinity("hv", paths)?.status).toBe("loading");
			expect(runs).toBe(2);
			next.resolve(display([]));
			await next.promise; await Promise.resolve();
			clock.mockReturnValue(20_000);
			expect(requestAffinity("hv", paths)?.status).toBe("ready");
			expect(runs).toBe(2);
		} finally {
			clock.mockRestore();
		}
	});
	test("no request starts without a runner, a codebase, or a file count from 2 to 64", () => {
		expect(requestAffinity("hv", ["docs/a.md", "docs/b.md"])).toBeUndefined();
		let runs = 0;
		setAffinityRunner(() => { runs++; return Promise.withResolvers<unknown>().promise; });
		expect(requestAffinity(null, ["docs/a.md", "docs/b.md"])).toBeUndefined();
		expect(requestAffinity("hv", ["docs/a.md"])).toBeUndefined();
		expect(requestAffinity("hv", Array.from({ length: 65 }, (_, n) => `docs/${n}.md`))).toBeUndefined();
		expect(requestAffinity("hv", Array.from({ length: 64 }, (_, n) => `docs/${n}.md`))?.status).toBe("loading");
		expect(runs).toBe(1);
	});
	test("a failed run and an invalid result report failure; a replaced runner cancels requests in flight", async () => {
		const failing = deferred();
		setAffinityRunner(() => failing.promise);
		requestAffinity("hv", ["docs/a.md", "docs/b.md"]);
		failing.reject(new Error("server unavailable"));
		await failing.promise.catch(() => undefined); await Promise.resolve();
		expect(requestAffinity("hv", ["docs/a.md", "docs/b.md"])).toEqual({ status: "failed", error: "server unavailable" });
		setAffinityRunner(async () => ({ display: { kind: "graph" } }));
		requestAffinity("hv", ["docs/a.md", "docs/b.md"]);
		await Promise.resolve(); await Promise.resolve();
		expect(requestAffinity("hv", ["docs/a.md", "docs/b.md"])?.status).toBe("failed");
		let signal: AbortSignal | undefined;
		const late = deferred(), runner: AffinityRunner = (_paths, abort) => { signal = abort; return late.promise; };
		setAffinityRunner(runner);
		requestAffinity("hv", ["docs/a.md", "docs/b.md"]);
		setAffinityRunner(runner);
		expect(signal?.aborted).toBe(true);
		late.resolve(display([]));
		await late.promise; await Promise.resolve();
		expect(requestAffinity("hv", ["docs/a.md", "docs/b.md"])?.status).toBe("loading");
	});
});
