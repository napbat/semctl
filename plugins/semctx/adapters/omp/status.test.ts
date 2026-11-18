import { describe, expect, test } from "bun:test";
import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext, ExtensionWidgetContent } from "@oh-my-pi/pi-coding-agent";
import type { Component, DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { installSyncStatus, readStatusUpdate } from "./status";
import { asNativeElement } from "./native-elements";

const root = path.resolve("status-fixture");
const describeContext: DescribeContext = {
	cols: 80, reduceMotion: false, dark: true, supports: () => true, feature: () => false,
};
function status(overrides: Record<string, unknown> = {}) {
	return { version: 1, status: { root, codebase_id: "codebase", running: false, pending_triggers: 0,
		last_error: null, first_index: null, sync_progress: null, watcher: "active", ...overrides } };
}
function embedding(overrides: Record<string, unknown> = {}) {
	return { job_id: "current-job", state: "running", files_embedded: 3, files_to_embed: 10,
		files_failed: 0, error: null, ...overrides };
}
function indicatorNode(node: NativeNode | null | undefined): NativeNode | undefined {
	if (node?.p?.role === "semctl.sync") return node;
	for (const child of node?.c ?? []) {
		if (!("k" in child)) continue;
		const found = indicatorNode(child);
		if (found) return found;
	}
	return undefined;
}
function harness(cwd = root, hasUI = true, options: {
	commands?: boolean; custom?: boolean; text?: boolean;
	openGraph?: (ctx: ExtensionContext, mode: "overview" | "live") => Promise<void>;
} = {}) {
	type Handler = (event: Record<string, unknown>, ctx: ExtensionContext) => unknown;
	type Command = (args: string, ctx: ExtensionContext) => Promise<void>;
	const handlers = new Map<string, Handler[]>(), commands = new Map<string, Command>(), notices: string[] = [];
	let widget: Component | undefined, view: Component | undefined, closed = 0, renders = 0, mounts = 0, unmounts = 0;
	const statusCalls: unknown[][] = [];
	let popupOptions: Parameters<ExtensionContext["ui"]["custom"]>[1];
	const tui = { terminal: { rows: 14, columns: 80, write(_data: string) {} }, requestRender() { renders++; } };
	class Text {
		constructor(readonly text: string) {}
		render(_width: number) { return this.text.split("\n"); }
		invalidate() {}
	}
	const pi = {
		on(event: string, handler: Handler) { handlers.set(event, [...(handlers.get(event) ?? []), handler]); },
		...(options.commands === false ? {} : {
			registerCommand(name: string, value: { handler: Command }) { commands.set(name, value.handler); },
		}),
		pi: options.text === false ? {} : { Text },
	} as unknown as ExtensionAPI;
	const ctx = { cwd, hasUI, ui: {
		setStatus(...args: unknown[]) { statusCalls.push(args); },
		setWidget(_key: string, content: ExtensionWidgetContent, placement?: { placement?: string }) {
			expect(placement?.placement ?? "aboveEditor").toBe("aboveEditor");
			widget?.dispose?.();
			if (typeof content === "function") {
				mounts++;
				widget = content(tui as never, {} as never);
			} else { unmounts++; widget = undefined; }
		},
		theme: { fg: (_tone: string, text: string) => text, symbol: (name: string) => name },
		notify(message: string) { notices.push(message); },
		...(options.custom === false ? {} : {
			async custom(factory: Parameters<ExtensionContext["ui"]["custom"]>[0],
				options?: Parameters<ExtensionContext["ui"]["custom"]>[1]) {
				popupOptions = options;
				view = await factory(tui as never,
					{} as never, { matches: (data: string, key: string) => data === key } as never, () => { closed++; });
			},
		}),
	} } as unknown as ExtensionContext;
	installSyncStatus(pi, options.openGraph);
	const emit = (name: string, event: Record<string, unknown>, context = ctx) => { for (const handler of handlers.get(name) ?? []) handler(event, context); };
	const native = (cx = describeContext) => indicatorNode(widget?.describe?.(cx));
	const meter = () => {
		const node = native();
		const child = node?.c?.[0] as NativeNode | undefined;
		return child?.k === "meter" ? child.p : undefined;
	};
	return { ctx, commands, notices, statusCalls, get shown() { return widget?.render(80).join("\n") || undefined; },
		get widget() { return widget; }, get view() { return view; }, get meter() { return meter(); },
		get popupOptions() { return popupOptions; },
		get closed() { return closed; }, get renders() { return renders; }, get mounts() { return mounts; },
		get unmounts() { return unmounts; }, native, emit,
		activate(act = "status") { return widget?.handleNativeEvent?.({ type: "action", key: "", act, mods: [] }); },
		command(args = "") { return commands.get("semctl")?.(args, ctx); },
		notify(params: unknown, server = "semctx", method = "notifications/semctl/sync_status") { emit("mcp_notification", { server, method, params }); },
	};
}

test("status hover updates changed diagnostics without remounting or dumping the full trace", async () => {
	const h = harness();
	h.notify(status({ last_error: "Provider unavailable" }));
	expect(h.native()?.p?.title).toContain("Provider unavailable");
	const mounts = h.mounts;
	const diagnostic = `Provider quota exhausted\n${"provider detail ".repeat(40)}\nTrace retained in status`;
	h.notify(status({ last_error: diagnostic }));
	expect(h.native()?.p?.title).toContain("Provider quota exhausted");
	expect(h.native()?.p?.title).not.toContain("Provider unavailable");
	expect(h.native()?.p?.title).not.toContain("Trace retained in status");
	expect(h.mounts).toBe(mounts);
	await h.command();
	const body = h.view?.describe?.(describeContext);
	const snapshot = body?.c?.find(child => "k" in child && child.key === "snapshot") as NativeNode | undefined;
	const text = snapshot?.c?.[0] as NativeNode | undefined;
	expect(text?.k === "text" ? text.p?.text : undefined).toContain(diagnostic);
});

describe("coordinator status evidence", () => {
	test("idle and first-index readiness remain informational, not graph-freshness success", () => {
		for (const first_index of [null, "ready"]) {
			expect(readStatusUpdate(status({ first_index, last_job_id: "old-success" }))?.indicator).toMatchObject({ tone: "muted", symbol: "status.info" });
		}
		expect(readStatusUpdate(status({ first_index: "ready", last_error: "new reconcile failed" }))?.indicator).toMatchObject({ tone: "error", symbol: "status.error" });
	});
	test("active upload counts are valid only within the current attempt and their real denominator", () => {
		const current = readStatusUpdate(status({ running: true, sync_progress: { uploading: { uploaded_files: 3, total_files: 10 } } }));
		expect(current?.indicator?.text).toContain("3/10");
		expect(current?.indicator?.symbol).toBe("status.running");
		for (const uploaded_files of [-1, 11, 1.5, Number.NaN]) {
			const invalid = readStatusUpdate(status({ running: true, sync_progress: { uploading: { uploaded_files, total_files: 10 } } }));
			expect(invalid?.indicator?.text).not.toContain("/");
		}
		const stopped = readStatusUpdate(status({ sync_progress: { uploading: { uploaded_files: 3, total_files: 10 } } }));
		expect(stopped?.indicator?.symbol).toBe("status.info");
		expect(stopped?.indicator?.text).not.toContain("3/10");
	});
	test("retained failures remain distinct from active progress and watcher availability", () => {
		const error = "upload rejected";
		expect(readStatusUpdate(status({ running: true, last_error: error, sync_progress: "preparing" }))?.indicator).toMatchObject({ tone: "warning", symbol: "status.running" });
		expect(readStatusUpdate(status({ last_error: error }))?.indicator).toMatchObject({ tone: "error", symbol: "status.error" });
		expect(readStatusUpdate(status({ first_index: { failed: { reason: error } } }))?.indicator?.tone).toBe("error");
		expect(readStatusUpdate(status({ watcher: { unavailable: "watch handle failed" } }))?.indicator?.tone).toBe("warning");
	});
	test("a producer retry delay pauses a failed attempt without hiding its diagnostic", () => {
		const error = `upload rejected: ${"full diagnostic ".repeat(20)}`;
		for (const retry_after_secs of [0, 5, 60]) {
			const paused = readStatusUpdate(status({ last_error: error, retry_after_secs }));
			expect(paused?.indicator).toMatchObject({ tone: "warning", symbol: "status.pending" });
			expect(paused?.indicator?.text).not.toContain(error);
			expect(paused?.details).toContain(error);
			expect(paused?.details).toContain(`"retry_after_secs": ${retry_after_secs}`);
		}
		expect(readStatusUpdate(status({ last_error: error, retry_after_secs: -1 }))?.indicator?.symbol).toBe("status.error");
		expect(readStatusUpdate(status({ running: true, last_error: error, retry_after_secs: 5 }))?.indicator?.symbol).toBe("status.running");
		expect(readStatusUpdate(status({ last_error: error }))?.indicator?.symbol).toBe("status.error");
		expect(readStatusUpdate(status())?.indicator?.symbol).toBe("status.info");
	});
	test("details retain all producer diagnostics but do not expose unrelated notification fields", () => {
		const update = readStatusUpdate(status({
			first_index: { failed: { reason: "first-index failure" } }, last_error: "reconcile failure",
			watcher: { unavailable: "watcher failure" }, source: "private source must not appear",
		}));
		expect(update?.details).toContain("first-index failure");
		expect(update?.details).toContain("reconcile failure");
		expect(update?.details).toContain("watcher failure");
		expect(update?.details).not.toContain("private source must not appear");
		expect(readStatusUpdate(status({ first_index: { failed: { reason: "" } } }))?.indicator?.symbol).toBe("status.error");
	});
	test("human inspection preserves multiline diagnostics while expert details retain the escaped producer JSON", () => {
		const error = "upload rejected\nfull diagnostic tail";
		const update = readStatusUpdate(status({
			running: true, last_error: error, watcher: { unavailable: "watcher diagnostic\nfull watcher tail" },
			sync_progress: { uploading: { uploaded_files: 3, total_files: 10 } },
		}));
		expect(update?.summary).toContain("3/10");
		expect(update?.summary).toContain(error);
		expect(update?.summary).toContain("watcher diagnostic\nfull watcher tail");
		expect(update?.summary).not.toContain("uploaded_files");
		expect(update?.details).toContain('"uploaded_files": 3');
		expect(update?.details).toContain(JSON.stringify(error));
		const controls = readStatusUpdate(status({ last_error: "first\u001bsecond\u0007\nlast" }));
		expect(controls?.summary).not.toContain("\u001b");
		expect(controls?.summary).not.toContain("\u0007");
		expect(controls?.summary).toContain("\nlast");
		expect(controls?.details).toContain(JSON.stringify("first\u001bsecond\u0007\nlast"));
	});
	test("embedding progress uses observed counts and reserves full progress for current terminal completion", () => {
		for (const files_embedded of [0, 1, 3, 9, 10]) {
			const update = readStatusUpdate(status({ embedding: embedding({ files_embedded }) }));
			expect(update?.indicator?.progress).toBe(Math.min(0.99, files_embedded / 10));
			expect(update?.indicator?.symbol).toBe("status.running");
			expect(update?.summary).toContain(`${files_embedded}/10`);
		}
		const complete = readStatusUpdate(status({ first_index: "ready", embedding: embedding({ state: "completed", files_embedded: 10 }) }));
		expect(complete?.indicator).toMatchObject({ tone: "success", symbol: "status.success", progress: 1 });
		expect(complete?.details).toContain('"files_embedded": 10');
		for (const evidence of [
			embedding({ files_embedded: -1 }), embedding({ files_embedded: 11 }),
			embedding({ files_embedded: 1.5 }), embedding({ files_to_embed: -1 }),
			embedding({ state: "completed", files_failed: 1 }), embedding({ state: "completed", error: "failed" }),
			embedding({ state: "unknown-new-state" }),
		]) expect(readStatusUpdate(status({ embedding: evidence }))?.indicator?.progress).not.toBe(1);
		for (const evidence of [
			embedding({ files_embedded: null, files_to_embed: null }),
			embedding({ files_embedded: 0, files_to_embed: 0 }),
		]) expect(readStatusUpdate(status({ embedding: evidence }))?.indicator?.progress).toBeUndefined();
	});
	test("new work, failures, unavailable observation and legacy evidence cannot inherit an old completion", () => {
		const complete = embedding({ state: "completed", files_embedded: 10 });
		for (const changes of [
			{ running: true }, { pending_triggers: 1 }, { trigger_overflow: true },
			{ last_error: "upload failed" }, { retry_after_secs: 5 },
			{ first_index: "registering" }, { first_index: "syncing" },
			{ first_index: { embedding: { job_id: "current-job" } } },
			{ watcher: { unavailable: "watcher failed" } },
			{ last_job_id: "newer-job" },
		]) {
			const update = readStatusUpdate(status({ embedding: complete, ...changes }));
			expect(update?.indicator?.progress).not.toBe(1);
			expect(update?.indicator?.tone).not.toBe("success");
		}
		expect(readStatusUpdate(status({ first_index: "ready", last_job_id: "old-job" }))?.indicator?.tone).toBe("muted");
		const unavailable = readStatusUpdate(status({ embedding: embedding({ state: "unavailable", error: "full observer diagnostic" }) }));
		expect(unavailable?.indicator).toMatchObject({ tone: "warning", symbol: "status.warning" });
		expect(unavailable?.summary).toContain("full observer diagnostic");
		const failed = readStatusUpdate(status({ embedding: embedding({ state: "failed", files_failed: 2, error: "partial job failure" }) }));
		expect(failed?.indicator?.tone).toBe("error");
		expect(failed?.summary).toContain("partial job failure");
	});
	test("inventory milestones retain observed job progress without treating manifest size as changed files", () => {
		for (const sync_progress of ["preparing", { scanning: { root } }, { planning: { files: 288, cached_files: 0 } }]) {
			const completed = readStatusUpdate(status({ running: true, sync_progress,
				first_index: "ready", embedding: embedding({ state: "completed", files_embedded: 10 }) }));
			expect(completed?.indicator).toMatchObject({ progress: 1, tone: "success" });
			expect(completed?.indicator?.text).toContain("last observed job completed");
			expect(completed?.details).toContain("sync_progress");
			const active = readStatusUpdate(status({ running: true, sync_progress, embedding: embedding({ files_embedded: 7 }) }));
			expect(active?.indicator).toMatchObject({ progress: 0.7, symbol: "status.running" });
		}
	});
	test("unknown milestone payloads do not masquerade as inventory completion", () => {
		const complete = embedding({ state: "completed", files_embedded: 10 });
		for (const sync_progress of [{ scanning: { scanned_files: 20 } }, { scanning: null },
			{ scanning: { root: 42 } }, { unknown_phase: {} }]) {
			const update = readStatusUpdate(status({
				first_index: "ready", last_job_id: "current-job", running: true, sync_progress, embedding: complete,
			}));
			expect(update?.indicator?.progress).toBeUndefined();
			expect(update?.indicator?.tone).toBe("accent");
			expect(update?.details).not.toContain('"scanned_files"');
		}
	});
	test("observed delivery prevents old completion from reappearing in a later scan or idle snapshot", () => {
		const completed = embedding({ state: "completed", files_embedded: 10 });
		const initial = readStatusUpdate(status({ embedding: completed }));
		const uploading = readStatusUpdate(status({ running: true, embedding: completed,
			sync_progress: { uploading: { uploaded_files: 1, total_files: 4 } } }), initial);
		expect(uploading?.indicator?.progress).toBe(0.25);
		expect(uploading?.unresolvedJob).toBe("current-job");
		const scanning = readStatusUpdate(status({ running: true, sync_progress: { scanning: { root } }, embedding: completed }), uploading);
		expect(scanning?.indicator?.progress).not.toBe(1);
		const idle = readStatusUpdate(status({ embedding: completed }), scanning);
		expect(idle?.indicator?.progress).not.toBe(1);
		const nextJob = readStatusUpdate(status({ last_job_id: "next-job",
			embedding: embedding({ job_id: "next-job", files_embedded: 5 }) }), idle);
		expect(nextJob?.indicator?.progress).toBe(0.5);
		const done = readStatusUpdate(status({ last_job_id: "next-job",
			embedding: embedding({ job_id: "next-job", state: "completed", files_embedded: 10 }) }), nextJob);
		expect(done?.indicator?.progress).toBe(1);
	});
	test("rejects unbound, malformed and unknown-version observations without guessing a phase", () => {
		for (const value of [undefined, { version: 2, status: status().status }, status({ root: "relative" }), status({ root: "bad\0path" }),
			status({ running: "true" }), status({ pending_triggers: -1 }), status({ first_index: { failed: { reason: 42 } } }),
			status({ first_index: "unknown-new-phase" })]) expect(readStatusUpdate(value)).toBeUndefined();
	});
});

describe("persistent composer widget", () => {
	test("session start mounts a hidden transport component before any producer snapshot", () => {
		const h = harness();
		h.emit("session_start", {});
		expect(h.mounts).toBe(1);
		expect(h.widget?.render(80)).toEqual([]);
		expect(h.native()).toMatchObject({ k: "row", key: "semctl", p: { hidden: true } });
		const widget = h.widget, node = h.native();
		h.emit("session_start", {});
		h.emit("session_switch", {});
		h.notify({ version: 1, root, status: null });
		expect(h.native()).toBe(node);
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		expect(h.widget).toBe(widget);
		expect(h.mounts).toBe(1);
		expect(h.meter?.value).toBe(0.3);
		expect(h.native()?.p?.hidden).toBe(false);
	});
	test("stock widget owns one compact native ring and label, with no structured status calls", () => {
		const h = harness();
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		expect(h.statusCalls).toEqual([]);
		expect(h.mounts).toBe(1);
		expect(h.native()).toMatchObject({
			k: "row", key: "semctl", p: {
				wrap: false, hidden: false, shrink: 0,
				min: { w: "9ch", h: "1lines" }, max: { w: "9ch", h: "1lines" },
			},
			c: [
				{ k: "meter", key: "progress", p: { style: "ring", size: "sm", value: 0.3 } },
				{ k: "text", key: "label", p: { text: "semctl", wrap: "none" } },
			],
		});
		expect(h.native()?.c).toHaveLength(2);
		expect(h.widget?.describe?.({ ...describeContext, supports: () => false })).toBeNull();
		expect(h.shown).toBe("◔ semctl");
	});
	test("styled status distinguishes active work from pending and errors without changing reported progress", () => {
		const h = harness(), cx = { ...describeContext, feature: () => true };
		const scenarios = [
			[status({ running: true, sync_progress: { scanning: { root } } }), "active", null],
			[status({ running: true, sync_progress: { planning: { files: 12 } } }), "active", null],
			[status({ embedding: embedding({ state: "queued", files_embedded: 0 }) }), "pending", 0],
			[status({ embedding: embedding({ files_embedded: 3 }) }), "active", 0.3],
			[status({ last_error: "provider unavailable" }), "error", null],
			[status({ last_error: "retry pending", retry_after_secs: 30 }), "warning", null],
			[status({ embedding: embedding({ state: "completed", files_embedded: 10 }) }), "ready", 1],
		] as const;
		for (const [snapshot, state, progress] of scenarios) {
			h.notify(snapshot);
			const indicator = h.native(cx)!;
			expect(asNativeElement(indicator)?.p.attrs?.["data-state"]).toBe(state);
			const meter = indicator.c?.[0] as NativeNode;
			expect(meter.k === "meter" ? meter.p?.value : undefined).toBe(progress);
		}
		expect(h.mounts).toBe(1);
		expect(h.unmounts).toBe(0);
	});
	test("motion preference changes update a cached native indicator without restarting status observation", () => {
		const h = harness(), cx = { ...describeContext, feature: () => true };
		h.notify(status({ running: true, sync_progress: "preparing" }));
		const full = h.native(cx)!;
		expect(asNativeElement(full)?.p.attrs?.["data-motion"]).toBe("full");
		const reduced = h.native({ ...cx, reduceMotion: true })!;
		expect(asNativeElement(reduced)?.p.attrs?.["data-motion"]).toBe("reduced");
		expect(h.native({ ...cx, reduceMotion: true })).toBe(reduced);
		expect(asNativeElement(h.native(cx)!)?.p.attrs?.["data-motion"]).toBe("full");
		expect(h.mounts).toBe(1);
		expect(h.unmounts).toBe(0);
	});
	test("repeated observations keep component, node identity and render demand stable", () => {
		const h = harness();
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		const widget = h.widget, node = h.native(), renders = h.renders;
		for (let i = 0; i < 20; i++) h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		expect(h.widget).toBe(widget);
		expect(h.native()).toBe(node);
		expect(h.renders).toBe(renders);
		expect(h.mounts).toBe(1);
		expect(h.unmounts).toBe(0);
		h.notify(status({ embedding: embedding({ files_embedded: 9 }) }));
		expect(h.widget).toBe(widget);
		expect(h.meter?.value).toBe(0.9);
		expect(h.native()?.c?.map(child => (child as NativeNode).key)).toEqual(["progress", "label"]);
		expect(h.shown).toBe("◕ semctl");
	});
	test("width and capability changes preserve geometry, while unknown remains a distinct static ring", () => {
		const h = harness();
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		const widget = h.widget, node = h.native();
		for (const cols of [120, 8, 1, 80]) {
			expect(h.native({ ...describeContext, cols })).toBe(node);
			expect(widget?.render(cols).every(row => row.length <= cols)).toBe(true);
		}
		expect(widget?.describe?.({ ...describeContext, supports: () => false })).toBeNull();
		expect(h.native()).toBe(node);
		for (const update of [
			status({ embedding: embedding({ files_embedded: null, files_to_embed: null }) }),
			status({ running: true, sync_progress: { scanning: { root } } }),
			{ version: 1, root, status: null },
		]) {
			h.notify(update);
			expect(h.meter).toMatchObject({
				style: "ring", size: "sm", value: null, shrink: 0,
				min: { w: "2ch", h: "1lines" }, max: { w: "2ch", h: "1lines" },
			});
			expect(h.native()).toMatchObject({
				p: { role: "semctl.sync", min: { h: "1lines" } },
				c: [
					{ key: "progress", p: { role: "semctl.sync.progress" } },
					{ key: "label", p: { text: "semctl", shrink: 0, min: { w: "6ch" }, max: { w: "6ch" } } },
				],
			});
		}
		expect(h.widget).toBe(widget);
		expect(h.mounts).toBe(1);
		expect(h.unmounts).toBe(0);
	});
	test("all real states retain a bounded plain footprint and exact native progress", () => {
		const h = harness(), labels: string[] = [];
		for (const update of [
			status(), status({ running: true, sync_progress: "preparing" }),
			status({ running: true, sync_progress: { uploading: { uploaded_files: 1, total_files: 9 } } }),
			status({ running: true, sync_progress: { uploading: { uploaded_files: 999999, total_files: 1000000 } } }),
			status({ embedding: embedding({ files_embedded: 9 }) }),
			status({ embedding: embedding({ state: "queued", files_embedded: null, files_to_embed: null }) }),
			status({ embedding: embedding({ state: "completed", files_embedded: 10 }) }),
			status({ embedding: embedding({ state: "unavailable", error: "observer failed" }) }),
			status({ last_error: "long diagnostic\n".repeat(100) }),
			status({ last_error: "retry pending", retry_after_secs: 60 }),
			status({ watcher: { unavailable: "long watcher diagnostic".repeat(100) } }),
		]) {
			h.notify(update);
			labels.push(h.shown!);
			expect(h.native()).toMatchObject({ c: [{ key: "progress", k: "meter" }, { key: "label", k: "text" }] });
			expect(h.widget?.render(4).every(row => row.length <= 4)).toBe(true);
		}
		expect(new Set(labels.map(label => label.length)).size).toBe(1);
		expect(labels.every(label => !/[\r\n]/.test(label))).toBe(true);
		expect(labels.join("")).not.toContain(root);
		expect(labels.join("")).not.toContain("diagnostic");
		expect(h.mounts).toBe(1);
	});
	test("inventory observations do not flash known embedding completion or replace the widget", () => {
		const h = harness(), complete = embedding({ state: "completed", files_embedded: 10 });
		const completed = { first_index: "ready", last_job_id: "current-job", embedding: complete };
		h.notify(status(completed));
		const widget = h.widget, fallback = h.shown;
		for (let i = 0; i < 5; i++) for (const sync_progress of [
			"preparing", { scanning: { root } }, { planning: { files: 288, cached_files: 288 } }, null,
		]) {
			h.notify(status({ ...completed, running: sync_progress !== null, sync_progress }));
			expect(h.meter).toMatchObject({ value: 1, tone: "success" });
			expect(h.widget).toBe(widget);
			expect(h.shown).toBe(fallback);
			expect(h.native()?.p?.hidden).toBe(false);
			expect(h.native()?.p).toMatchObject({
				min: { w: "9ch", h: "1lines" }, max: { w: "9ch", h: "1lines" }, shrink: 0,
			});
		}
		expect(h.mounts).toBe(1);
		expect(h.unmounts).toBe(0);
		h.notify(status({ running: true, sync_progress: { uploading: { uploaded_files: 1, total_files: 7 } }, embedding: complete }));
		expect(h.meter?.value).toBe(1 / 7);
		h.notify(status({ embedding: complete }));
		expect(h.meter?.value).not.toBe(1);
		h.notify(status({ last_error: "current attempt failed" }));
		expect(h.meter).toMatchObject({ tone: "error", value: null });
	});
	test("other producers, malformed unbound observations and eviction cannot replace owned evidence", () => {
		const h = harness(); h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		const original = h.native();
		h.notify(status({ last_error: "unrelated" }), "semctx-other");
		h.notify(status({ last_error: "unrelated" }), "semctx", "notifications/other");
		h.notify({ version: 2, status: status({ root: `${root}-sibling` }).status });
		h.notify({ version: 1, status: { running: "malformed" } });
		for (let i = 0; i < 40; i++) h.notify(status({ root: `${root}-other-${i}`, last_error: "unrelated" }));
		expect(h.native()).toBe(original);
		expect(h.mounts).toBe(1);
		h.notify({ version: 2, status: status().status });
		expect(h.meter).toMatchObject({ tone: "muted", value: null });
	});
	test("a detached nested checkout stays unknown instead of inheriting parent progress", () => {
		const child = path.join(root, "nested"), h = harness(path.join(child, "src"));
		h.notify(status({ embedding: embedding({ state: "completed", files_embedded: 10 }) }));
		h.notify(status({ root: child }));
		expect(h.meter).toMatchObject({ tone: "muted", value: null });
		h.notify({ version: 1, root: child, status: null });
		expect(h.meter).toMatchObject({ tone: "muted", value: null });
		expect(h.mounts).toBe(1);
	});
	test("clearing or unknown snapshots erases completion without unmounting the observed slot", async () => {
		const h = harness();
		h.notify({ version: 1, root, status: null });
		expect(h.widget).toBeDefined();
		expect(h.native()?.p?.hidden).toBe(true);
		expect(h.widget?.render(80)).toEqual([]);
		h.notify(status({ embedding: embedding({ state: "completed", files_embedded: 10 }) }));
		const widget = h.widget;
		for (const update of [{ version: 1, root, status: null }, { version: 1, status: null },
			{ version: 2, status: status().status }, status()]) {
			h.notify(update);
			expect(h.meter).toMatchObject({ tone: "muted", value: null });
			expect(h.widget).toBe(widget);
			await h.command("status");
			expect(h.view?.render(100).join("\n")).not.toContain("10/10");
		}
		expect(h.mounts).toBe(1);
		expect(h.unmounts).toBe(0);
	});
	test("context changes hide rather than replace the widget; shutdown disables retained actions", () => {
		const h = harness(); h.notify(status());
		const widget = h.widget;
		h.ctx.cwd = path.resolve("unrelated"); h.emit("session_switch", {});
		expect(h.shown).toBeUndefined();
		expect(h.native()?.p?.hidden).toBe(true);
		h.ctx.cwd = root; h.emit("session_switch", {});
		expect(h.widget).toBe(widget);
		expect(h.shown).toBe("○ semctl");
		h.emit("session_shutdown", {});
		expect(h.shown).toBeUndefined();
		expect(h.unmounts).toBe(1);
		widget?.handleNativeEvent?.({ type: "action", key: "", act: "status", mods: [] });
		expect(h.view).toBeUndefined();
		h.emit("session_start", {}); expect(h.shown).toBeUndefined();
	});
	test("noninteractive sessions do not mount composer output", () => {
		const h = harness(root, false); h.notify(status()); h.emit("session_shutdown", {});
		expect(h.mounts).toBe(0);
		expect(h.shown).toBeUndefined();
	});
	if (process.platform === "win32") test("extended roots and ordinary cwd resolve to the same owner", () => {
		const h = harness(root); h.notify(status({ root: `\\\\?\\${root}` }));
		expect(h.meter).toMatchObject({ tone: "muted", value: null });
	});
});

describe("composer actions", () => {
	test("persistent actions use the latest context and expose status, details, overview and live callbacks", async () => {
		const graphs: { ctx: ExtensionContext; mode: "overview" | "live" }[] = [];
		const h = harness(root, true, { openGraph: async (ctx, mode) => { graphs.push({ ctx, mode }); } });
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		const widget = h.widget, next = { ...h.ctx };
		h.emit("mcp_notification", { server: "semctx", method: "notifications/semctl/sync_status",
			params: status({ last_error: "latest diagnostic" }) }, next);
		expect(h.widget).toBe(widget);
		expect(h.native()?.p?.actions).toEqual({ click: "status", menu: ["status", "details", "graph", "live"] });
		await h.activate("status");
		expect(JSON.stringify(h.view?.describe?.(describeContext))).toContain("latest diagnostic");
		await h.activate("details");
		expect(JSON.stringify(h.view?.describe?.(describeContext))).toContain("Producer snapshot");
		await h.activate("graph");
		await h.activate("live");
		await h.command("graph");
		await h.command("graph live");
		expect(graphs.map(graph => graph.mode)).toEqual(["overview", "live", "overview", "live"]);
		expect(graphs[0].ctx).toBe(next);
		expect(graphs[1].ctx).toBe(next);
		h.emit("session_shutdown", {});
		widget?.handleNativeEvent?.({ type: "action", key: "", act: "graph", mods: [] });
		expect(graphs).toHaveLength(4);
	});
	test("host disposal permits remounting but never occurs for notification updates", () => {
		const h = harness();
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		h.widget?.dispose?.();
		h.emit("session_switch", {});
		expect(h.mounts).toBe(2);
		h.notify(status({ embedding: embedding({ files_embedded: 4 }) }));
		expect(h.mounts).toBe(2);
		expect(h.meter?.value).toBe(0.4);
	});
});

describe("native status inspection", () => {
	test("status and details use a bounded native OMP popup with real text and retained scrolling", async () => {
		const h = harness();
		h.notify(status({ last_error: "native diagnostic\nsecond line" }));
		for (const command of ["status", "details"]) {
			await h.command(command);
			expect(h.popupOptions).toEqual({
				overlay: true, overlayOptions: { width: "80%", maxHeight: "80%", anchor: "center" },
			});
			expect(h.view?.nativeOverlay).toEqual({
				role: "omp.overlay.semctl-status", size: "md", head: `semctl ${command}`,
			});
			expect(h.view?.describeScreen).toBeUndefined();
			const node = h.view?.describe?.(describeContext);
			const snapshot = node?.c?.find(child => "k" in child && child.key === "snapshot") as NativeNode | undefined;
			expect(snapshot).toMatchObject({
				k: "col", key: "snapshot", p: { role: "semctl.status.snapshot", max: { h: "18lines" } },
				c: [{ k: "text", key: "snapshot-text", p: { wrap: "word" } }],
			});
			const body = snapshot?.c?.[0] as NativeNode | undefined;
			expect(body?.k === "text" ? body.p?.text : undefined).toContain("native diagnostic\nsecond line");
			expect(h.view?.render(80).join("\n")).toContain(`semctl ${command}`);
			h.view?.handleInput?.("tui.select.down");
			const scrolled = h.view?.describe?.(describeContext);
			expect(scrolled?.c?.find(child => "k" in child && child.key === "snapshot")).toMatchObject({
				scroll: { by: "line-down", n: 1 },
			});
			h.view?.dispose?.();
		}
	});
	test("native activation opens the latest owned diagnostic without using a command or model turn", async () => {
		const h = harness(root, true, { commands: false });
		h.notify(status({ last_error: "older diagnostic" }));
		const activate = h.activate;
		h.notify(status({ last_error: "current diagnostic" }));
		h.notify(status({ root: `${root}-sibling`, last_error: "unrelated diagnostic" }));
		await activate?.();
		const pages: string[] = [];
		for (let i = 0; i < 4; i++) {
			pages.push(h.view!.render(100).join("\n"));
			h.view!.handleInput?.("tui.select.pageDown");
		}
		expect(pages.join("\n")).toContain("current diagnostic");
		expect(pages.join("\n")).not.toContain("older diagnostic");
		expect(pages.join("\n")).not.toContain("unrelated diagnostic");
	});
	test("commands inspect the latest owned snapshot without requesting work or changing the indicator", async () => {
		const h = harness(), full = `full diagnostic ${"detail ".repeat(30)}`;
		h.notify(status({ running: true, sync_progress: { uploading: { uploaded_files: 3, total_files: 10 } }, last_job_id: "job-one" }));
		const before = h.shown;
		await h.command("status");
		expect(h.shown).toBe(before);
		const first = h.view?.describe?.(describeContext);
		expect(JSON.stringify(first)).toContain("job-one");
		expect(JSON.stringify(first)).toContain("3/10");
		expect(JSON.stringify(first)).not.toContain("uploaded_files");
		await h.command();
		expect(JSON.stringify(h.view?.describe?.(describeContext))).toBe(JSON.stringify(first));
		h.notify(status({ last_error: full, retry_after_secs: 5, last_job_id: "job-two" }));
		h.notify(status({ root: `${root}-sibling`, last_error: "unrelated diagnostic" }));
		await h.command("details");
		const latest = JSON.stringify(h.view?.describe?.(describeContext));
		expect(latest).toContain(full);
		expect(latest).toContain("job-two");
		expect(latest).not.toContain("job-one");
		expect(latest).not.toContain("unrelated diagnostic");
		expect(h.shown).not.toContain(full);
		expect(h.notices).toEqual([]);
	});
	test("expert inspection reaches the diagnostic and JSON tails through paging and returns to the start", async () => {
		const h = harness();
		const diagnostic = `${Array.from({ length: 40 }, (_, i) => `diagnostic row ${i}`).join("\n")}\nDIAGNOSTIC TAIL`;
		h.notify(status({ last_error: diagnostic, last_outcome: "JSON TAIL" }));
		await h.command("details");
		const view = h.view!;
		const firstPage = view.render(80);
		expect(firstPage.join("\n")).not.toContain("DIAGNOSTIC TAIL");
		expect(firstPage.join("\n")).not.toContain("JSON TAIL");
		let sawDiagnosticTail = false, sawJsonTail = false;
		for (let page = 0; page < 100; page++) {
			view.handleInput?.("tui.select.pageDown");
			const visible = view.render(80).join("\n");
			sawDiagnosticTail ||= visible.includes("DIAGNOSTIC TAIL");
			sawJsonTail ||= visible.includes("JSON TAIL");
			if (sawDiagnosticTail && sawJsonTail) break;
		}
		expect(sawDiagnosticTail).toBe(true);
		expect(sawJsonTail).toBe(true);
		for (let page = 0; page < 100; page++) view.handleInput?.("tui.select.pageUp");
		expect(view.render(80)).toEqual(firstPage);
		view.handleNativeEvent?.({ type: "action", key: "close", act: "close", mods: [] });
		expect(h.closed).toBe(1);
		await h.command();
		h.view?.handleInput?.("tui.select.cancel");
		expect(h.closed).toBe(2);
	});
	test("detach, root changes and shutdown remove diagnostics from inspection", async () => {
		const h = harness();
		h.notify(status({ last_error: "owned diagnostic" }));
		h.ctx.cwd = path.resolve("different-checkout");
		await h.command();
		expect(JSON.stringify(h.view?.describe?.(describeContext))).not.toContain("owned diagnostic");
		h.ctx.cwd = root;
		h.notify({ version: 1, root, status: null });
		await h.command();
		expect(JSON.stringify(h.view?.describe?.(describeContext))).not.toContain("owned diagnostic");
		h.notify(status({ last_error: "owned diagnostic" }));
		h.emit("session_shutdown", {});
		await h.command();
		expect(JSON.stringify(h.view?.describe?.(describeContext))).not.toContain("owned diagnostic");
	});
	test("unknown commands and older APIs degrade without opening a replacement UI", async () => {
		const h = harness(root, true, { custom: false });
		h.notify(status({ watcher: { unavailable: "full watcher diagnostic" } }));
		await h.command("details");
		expect(h.notices.join("\n")).toContain("full watcher diagnostic");
		expect(h.view).toBeUndefined();
		const noText = harness(root, true, { text: false });
		await noText.command();
		expect(noText.notices).toHaveLength(1);
		expect(noText.view).toBeUndefined();
		const old = harness(root, true, { commands: false });
		old.notify(status());
		expect(old.shown).toBe("○ semctl");
		expect(old.commands.size).toBe(0);
		const unknown = harness(); await unknown.command("retry");
		expect(unknown.notices).toHaveLength(1);
		expect(unknown.view).toBeUndefined();
	});
	test("noninteractive commands do not add UI", async () => {
		const h = harness(root, false);
		h.notify(status({ last_error: "diagnostic" }));
		await h.command("details");
		expect(h.view).toBeUndefined();
		expect(h.shown).toBeUndefined();
		expect(h.notices).toEqual([]);
	});
});
