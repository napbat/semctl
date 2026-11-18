import { afterEach, describe, expect, test } from "bun:test";
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
		expect(affinity?.files.get("docs/a.md")).toEqual({ path: "docs/a.md", query: "content", indexed: true, results: 3 });
		expect(readAffinity({ ...display([]), warnings: ["Providers loading"] }, requested)?.caveats).toEqual(["Providers loading", "Retrieval evidence."]);
		expect(readAffinity(display([{ a: "docs/a.md", b: "docs/c.md", direct: 0, shared: .5 }]), requested)).toBeUndefined();
		expect(readAffinity(display([{ a: "docs/a.md", b: "docs/b.md", direct: 0, shared: 1.5 }]), requested)).toBeUndefined();
		expect(readAffinity(display([{ a: "docs/a.md", b: "docs/a.md", direct: 0, shared: .5 }]), requested)).toBeUndefined();
		expect(readAffinity(display([], ["docs/a.md", "docs/z.md"]), requested)).toBeUndefined();
		expect(readAffinity({ display: { ...display([]).display, kind: "graph" } }, requested)).toBeUndefined();
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
