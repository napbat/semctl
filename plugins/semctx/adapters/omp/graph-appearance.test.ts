import { describe, expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DEFAULT_GRAPH_APPEARANCE, GraphAppearanceStore, type GraphAppearance, type GraphAppearancePersistence } from "./graph-appearance";

function document(patch: Partial<GraphAppearance> = {}): string {
	return JSON.stringify({ version: 2, appearance: { ...DEFAULT_GRAPH_APPEARANCE, ...patch } });
}
/** A version 1 appearance object: the six keys that predate the popup size. */
function versionOne(patch: Partial<GraphAppearance> = {}): Record<string, unknown> {
	return Object.fromEntries(Object.entries({ ...DEFAULT_GRAPH_APPEARANCE, ...patch }).filter(([key]) => key !== "popup"));
}
class MemoryPreferences implements GraphAppearancePersistence {
	readonly writes: { file: string; contents: string }[] = [];
	reads = 0;
	failure?: Error;
	constructor(public contents = document()) {}
	async read(_file: string): Promise<string> { this.reads++; return this.contents; }
	async writeAtomic(file: string, contents: string): Promise<void> {
		this.writes.push({ file, contents });
		if (this.failure) throw this.failure;
		this.contents = contents;
	}
}

// These tests import only Node and the pure store, never OMP's runtime or native bindings.
describe("graph appearance preferences", () => {
	test("missing file uses defaults without writing; atomic saves reopen and touch only the preference file", async () => {
		const directory = await mkdtemp(join(tmpdir(), "semctl-appearance-"));
		try {
			const file = join(directory, "preferences", "graph-appearance.json");
			const store = new GraphAppearanceStore(file);
			expect(store.state.ready).toBe(false);
			await store.load();
			expect(store.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
			expect(store.state.error).toBeUndefined();
			expect(store.state.ready).toBe(true);
			expect(store.saved).toBe(false);
			await store.flush();
			expect(await readdir(directory)).toEqual([]);
			await store.update({ layout: "layers", color: "structure", labels: "hover", motion: "off" });
			expect(store.saved).toBe(true);
			expect(JSON.parse(await readFile(file, "utf8"))).toEqual({ version: 2, appearance: store.state.value });
			expect(await readdir(join(directory, "preferences"))).toEqual(["graph-appearance.json"]);
			const reopened = new GraphAppearanceStore(file);
			await reopened.load();
			expect(reopened.state.value).toEqual(store.state.value);
			expect(reopened.saved).toBe(true);
			await reopened.reset();
			const reset = new GraphAppearanceStore(file);
			await reset.load();
			expect(reset.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
		} finally { await rm(directory, { recursive: true, force: true }); }
	});

	test("loads a user's version 1 file unchanged with a small popup and writes version 2 only on edit", async () => {
		const contents = `${JSON.stringify({ version: 1, appearance: { layout: "radial", color: "concepts", detail: "detailed", labels: "more", links: "soft", motion: "system" } }, null, 2)}\n`;
		const io = new MemoryPreferences(contents), store = new GraphAppearanceStore("graph-appearance.json", io);
		await store.load();
		expect(store.state.value).toEqual({ layout: "radial", color: "concepts", detail: "detailed", labels: "more", links: "soft", motion: "system", popup: "small" });
		expect(Object.keys(store.state.value)).toEqual(Object.keys(DEFAULT_GRAPH_APPEARANCE));
		expect(store.state.error).toBeUndefined();
		expect(store.saved).toBe(true);
		await store.flush();
		expect(io.writes).toHaveLength(0);
		expect(io.contents).toBe(contents);
		await store.update({ popup: "medium" });
		expect(JSON.parse(io.contents)).toEqual({ version: 2, appearance: { layout: "radial", color: "concepts", detail: "detailed", labels: "more", links: "soft", motion: "system", popup: "medium" } });
	});

	test("loads every version 1 layout unchanged and round-trips version 2 with all seven keys", async () => {
		for (const layout of ["force", "radial", "layers", "components"] as const) {
			const appearance = versionOne({ layout, color: "structure", detail: "calm", labels: "hover", links: "clear", motion: "off" });
			const io = new MemoryPreferences(JSON.stringify({ version: 1, appearance }));
			const store = new GraphAppearanceStore("preferences.json", io);
			await store.load();
			expect(store.state.value).toEqual({ ...appearance, popup: "small" } as GraphAppearance);
			expect(store.state.error).toBeUndefined();
			expect(io.writes).toHaveLength(0);
			await store.update({ layout: "map", popup: "large" });
			const saved = { ...appearance, layout: "map", popup: "large" };
			expect(JSON.parse(io.contents)).toEqual({ version: 2, appearance: saved });
			const reopened = new GraphAppearanceStore("preferences.json", io);
			await reopened.load();
			expect(reopened.state.value).toEqual(saved as GraphAppearance);
			expect(reopened.state.error).toBeUndefined();
			expect(reopened.saved).toBe(true);
		}
	});

	test("rejects malformed, incomplete, mixed-version, oversized and future schemas without auto-overwriting", async () => {
		const malformed = [
			"{", "null", "[]", " ".repeat(8193), JSON.stringify({ version: 3, appearance: DEFAULT_GRAPH_APPEARANCE }),
			JSON.stringify({ version: "2", appearance: DEFAULT_GRAPH_APPEARANCE }),
			JSON.stringify({ version: 1, appearance: { layout: "force" } }),
			JSON.stringify({ version: 1, appearance: DEFAULT_GRAPH_APPEARANCE }),
			JSON.stringify({ version: 2, appearance: versionOne() }),
			JSON.stringify({ version: 2, appearance: { ...DEFAULT_GRAPH_APPEARANCE, popup: "huge" } }),
			JSON.stringify({ version: 1, appearance: versionOne({ layout: "random" as GraphAppearance["layout"] }) }),
			JSON.stringify({ version: 1, appearance: { ...versionOne(), motion: false } }),
			JSON.stringify({ version: 2, appearance: { ...DEFAULT_GRAPH_APPEARANCE, foreign: "leave alone" } }),
			JSON.stringify({ version: 2, appearance: DEFAULT_GRAPH_APPEARANCE, unrelated: {} }),
		];
		for (const contents of malformed) {
			const io = new MemoryPreferences(contents), store = new GraphAppearanceStore("only-own-preferences.json", io);
			await store.load();
			expect(store.state.ready).toBe(true);
			expect(store.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
			expect(store.state.error).toBeTruthy();
			await store.flush();
			await store.retry();
			expect(io.writes).toHaveLength(0);
			expect(io.contents).toBe(contents);
			await store.update({ links: "clear" });
			expect(io.writes).toHaveLength(1);
			expect(store.state.value.links).toBe("clear");
			expect(store.state.error).toBeUndefined();
			expect(Object.keys(JSON.parse(io.contents))).toEqual(["version", "appearance"]);
		}
	});

	test("validates runtime updates without accepting unknown keys, getters or unsupported values", async () => {
		const io = new MemoryPreferences(), store = new GraphAppearanceStore("preferences.json", io);
		await store.load();
		const inputs: unknown[] = [null, [], { color: "red" }, { links: 1 }, { motion: undefined }, { layout: "radial", other: true }, { popup: "huge" }, { popup: "sm" },
			Object.defineProperty({}, "layout", { enumerable: true, get() { throw new Error("must not execute getter"); } })];
		for (const input of inputs) {
			await store.update(input as Partial<GraphAppearance>);
			expect(store.state.error).toContain("rejected");
			expect(store.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
		}
		expect(io.writes).toHaveLength(0);
		await store.update({ color: "neutral", popup: "large" });
		expect(store.state.error).toBeUndefined();
		expect(store.state.value.color).toBe("neutral");
		expect(store.state.value.popup).toBe("large");
	});

	test("late loading merges untouched disk preferences with the latest edits and coalesces writes", async () => {
		const read = Promise.withResolvers<string>(), started = Promise.withResolvers<void>();
		const writes: string[] = [];
		const store = new GraphAppearanceStore("preferences.json", {
			read() { started.resolve(); return read.promise; },
			async writeAtomic(_file, contents) { writes.push(contents); },
		});
		const loading = store.load();
		await started.promise;
		const first = store.update({ layout: "radial" });
		const second = store.update({ layout: "layers", links: "clear" });
		expect(store.state.value.layout).toBe("layers");
		expect(store.state.saving).toBe(true);
		read.resolve(document({ layout: "force", color: "structure", labels: "hover", motion: "off" }));
		await Promise.all([loading, first, second]);
		expect(store.state.value).toEqual({ ...DEFAULT_GRAPH_APPEARANCE, layout: "layers", links: "clear", color: "structure", labels: "hover", motion: "off" });
		expect(writes).toHaveLength(1);
		expect(JSON.parse(writes[0]).appearance).toEqual(store.state.value);
		expect(store.state.saving).toBe(false);
	});

	test("reset during loading overrides every old field rather than being replaced by disk state", async () => {
		const read = Promise.withResolvers<string>(), started = Promise.withResolvers<void>();
		const io = new MemoryPreferences();
		const store = new GraphAppearanceStore("preferences.json", {
			read() { started.resolve(); return read.promise; },
			writeAtomic: io.writeAtomic.bind(io),
		});
		const loading = store.load();
		await started.promise;
		const reset = store.reset();
		read.resolve(document({ layout: "radial", color: "neutral", detail: "detailed", labels: "more", links: "clear", motion: "off" }));
		await Promise.all([loading, reset]);
		expect(store.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
		expect(JSON.parse(io.contents).appearance).toEqual(DEFAULT_GRAPH_APPEARANCE);
	});

	test("serializes active saves and collapses rapid edits to the newest complete snapshot", async () => {
		const firstStarted = Promise.withResolvers<void>(), secondStarted = Promise.withResolvers<void>();
		const firstWrite = Promise.withResolvers<void>(), secondWrite = Promise.withResolvers<void>();
		const writes: string[] = [];
		let active = 0, maximum = 0;
		const store = new GraphAppearanceStore("preferences.json", {
			async read() { return document(); },
			async writeAtomic(_file, contents) {
				const index = writes.push(contents);
				maximum = Math.max(maximum, ++active);
				(index === 1 ? firstStarted : secondStarted).resolve();
				try { await (index === 1 ? firstWrite : secondWrite).promise; }
				finally { active--; }
			},
		});
		await store.load();
		const first = store.update({ layout: "radial" });
		await firstStarted.promise;
		const second = store.update({ color: "neutral" });
		const third = store.update({ layout: "layers", motion: "off" });
		expect(writes).toHaveLength(1);
		expect(store.state.value).toEqual({ ...DEFAULT_GRAPH_APPEARANCE, layout: "layers", color: "neutral", motion: "off" });
		const flushing = store.flush();
		firstWrite.resolve();
		await secondStarted.promise;
		expect(writes).toHaveLength(2);
		expect(JSON.parse(writes[1]).appearance).toEqual(store.state.value);
		expect(store.state.saving).toBe(true);
		secondWrite.resolve();
		await Promise.all([first, second, third, flushing]);
		expect(maximum).toBe(1);
		expect(store.state.saving).toBe(false);
		expect(store.saved).toBe(true);
	});

	test("save failure retains edits; retry and reset recover without rejecting UI actions", async () => {
		const io = new MemoryPreferences(), store = new GraphAppearanceStore("preferences.json", io);
		await store.load();
		io.failure = new Error("permission denied");
		await store.update({ color: "neutral", motion: "off" });
		expect(store.state.value.motion).toBe("off");
		expect(store.state.error).toContain("permission denied");
		expect(store.saved).toBe(false);
		expect(store.state.saving).toBe(false);
		await store.retry();
		expect(io.writes).toHaveLength(2);
		expect(store.state.error).toBeTruthy();
		io.failure = undefined;
		await store.retry();
		expect(JSON.parse(io.contents).appearance).toEqual(store.state.value);
		expect(store.state.error).toBeUndefined();
		await store.reset();
		expect(JSON.parse(io.contents).appearance).toEqual(DEFAULT_GRAPH_APPEARANCE);
		const reopened = new GraphAppearanceStore("preferences.json", io);
		await reopened.load();
		expect(reopened.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
	});

	test("a newer edit during a failed write gets a serialized attempt instead of being stranded", async () => {
		const started = Promise.withResolvers<void>(), write = Promise.withResolvers<void>();
		const io = new MemoryPreferences();
		let calls = 0;
		const store = new GraphAppearanceStore("preferences.json", {
			read: io.read.bind(io),
			async writeAtomic(file, contents) {
				if (++calls === 1) { started.resolve(); await write.promise; }
				else await io.writeAtomic(file, contents);
			},
		});
		await store.load();
		const first = store.update({ layout: "radial" });
		await started.promise;
		const latest = store.update({ layout: "layers", motion: "off" });
		write.reject(new Error("first write failed"));
		await Promise.all([first, latest]);
		expect(calls).toBe(2);
		expect(JSON.parse(io.contents).appearance).toEqual(store.state.value);
		expect(store.state.value.layout).toBe("layers");
		expect(store.state.error).toBeUndefined();
	});

	test("load failure can retry without saving, and explicit reset authorizes replacing invalid data", async () => {
		const io = new MemoryPreferences("broken"), store = new GraphAppearanceStore("preferences.json", io);
		await store.load();
		await store.flush();
		expect(io.writes).toHaveLength(0);
		io.contents = document({ color: "neutral" });
		await store.retry();
		expect(store.state.value.color).toBe("neutral");
		expect(store.state.error).toBeUndefined();
		expect(io.writes).toHaveLength(0);
		const invalid = new GraphAppearanceStore("preferences.json", new MemoryPreferences("future schema"));
		await invalid.load();
		await invalid.reset();
		expect(invalid.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
		expect(invalid.state.error).toBeUndefined();
	});

	test("memory-only instances stay isolated, publish live changes, and unsubscribe cleanly", async () => {
		const first = new GraphAppearanceStore(), second = new GraphAppearanceStore();
		let changes = 0;
		const unsubscribe = first.subscribe(() => changes++);
		await first.update({ motion: "off" });
		expect(first.state.value.motion).toBe("off");
		expect(second.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
		expect(first.persistent).toBe(false);
		expect(first.saved).toBe(false);
		expect(first.state.ready).toBe(true);
		expect(changes).toBeGreaterThan(0);
		unsubscribe();
		const before = changes;
		await first.reset();
		expect(changes).toBe(before);
		expect(first.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
	});
});
