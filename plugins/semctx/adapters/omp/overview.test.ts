import { describe, expect, test } from "bun:test";
import * as path from "node:path";
import type { ExecOptions, ExecResult, ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { readDisplay, SemctlInspector, type Display } from "./inspector";
import { readStatusUpdate } from "./status";
import { GRAPH_OPERATIONS, GraphRequest, createGraphOverview, graphArguments, type GraphOverviewDependencies } from "./overview";

const cwd = path.resolve("graph-overview-checkout");
const native: DescribeContext = { cols: 88, dark: true, reduceMotion: false, supports: () => true, feature: () => false };
type Result = ExecResult;
const result = (stdout: string, extra: Partial<Result> = {}): Result => ({ stdout, stderr: "", code: 0, killed: false, ...extra });
interface Deferred<T> { promise: Promise<T>; resolve(value: T): void; reject(error: Error): void }
function deferred<T>(): Deferred<T> { return Promise.withResolvers<T>(); }
const deps: GraphOverviewDependencies = {
	readDisplay: () => undefined,
	createInspector: () => ({ render: () => [], invalidate() {} }),
};
function pendingExecutor() {
	const pending: { command: string; args: string[]; options: ExecOptions | undefined; response: Deferred<Result> }[] = [];
	const exec: ExtensionAPI["exec"] = (command, args, options) => {
		const response = deferred<Result>(); pending.push({ command, args, options, response }); return response.promise;
	};
	return { exec, pending };
}
function nodes(root: NativeNode): NativeNode[] {
	const all = [root];
	if (root.k === "section" && root.p?.collapsed) return all;
	for (const child of root.c ?? []) {
		if ("k" in child) all.push(...nodes(child));
		else if (child.describe) { const body = child.describe(native); if (body) all.push(...nodes(body)); }
	}
	return all;
}
function content(root: NativeNode): string {
	return nodes(root).map(n => n.k === "text" ? n.p?.text ?? "" : "").join("\n");
}
const projects = GRAPH_OPERATIONS.find(operation => operation.id === "projects")!;

describe("read-only graph requests", () => {
	test("executes shell-free CLI JSON with cwd, timeout and cancellation signal only on an explicit run", async () => {
		const host = pendingExecutor(); let changes = 0;
		const request = new GraphRequest(host.exec, deps, () => changes++);
		expect(host.pending).toHaveLength(0);
		const operation = GRAPH_OPERATIONS.find(op => op.id === "trace")!;
		const work = request.run(operation, ["name; not a shell command"], cwd, () => cwd);
		expect(request.state.loading).toBe(true);
		expect(host.pending[0].command).toBe("semctl");
		expect(host.pending[0].args).toEqual(["graph", "--json", "trace", "--", "name; not a shell command"]);
		expect(host.pending[0].options?.cwd).toBe(cwd);
		expect(host.pending[0].options?.timeout).toBe(60_000);
		host.pending[0].response.resolve(result(JSON.stringify({ text: "Original complete text", display: null })));
		await work;
		expect(request.state).toEqual({ loading: false, text: "Original complete text", display: undefined, error: undefined });
		expect(changes).toBe(2);
	});
	test("replacement aborts old request and an abort-ignoring late result cannot replace newer state", async () => {
		const host = pendingExecutor();
		const request = new GraphRequest(host.exec, deps, () => {});
		const first = request.run(projects, [], cwd, () => cwd);
		const second = request.run(projects, [], cwd, () => cwd);
		expect(host.pending[0].options?.signal?.aborted).toBe(true);
		host.pending[1].response.resolve(result(JSON.stringify({ text: "new", display: null })));
		await second;
		host.pending[0].response.resolve(result(JSON.stringify({ text: "old", display: null })));
		await first;
		expect(request.state.text).toBe("new");
		const third = request.run(projects, [], cwd, () => cwd);
		request.cancel();
		expect(host.pending[2].options?.signal?.aborted).toBe(true);
		host.pending[2].response.reject(new Error("late cancellation"));
		await third;
		expect(request.state.loading).toBe(false);
		expect(request.state.error).toBeUndefined();
	});
	test("cwd identity rejects stale data and unrelated checkout metadata", async () => {
		const host = pendingExecutor();
		let current = cwd;
		const request = new GraphRequest(host.exec, deps, () => {});
		const work = request.run(projects, [], cwd, () => current);
		current = path.resolve("another-checkout");
		host.pending[0].response.resolve(result(JSON.stringify({ text: "wrong cwd", display: null })));
		await work;
		expect(request.state.text).not.toBe("wrong cwd");
		const foreign: Display = { version: 1, kind: "batch", identity: { codebaseId: "foreign", name: null, copy: "checkout", localRoot: current }, symbols: [] };
		const scoped = new GraphRequest(async () => result(JSON.stringify({ text: "do not open", display: foreign })), { ...deps, readDisplay: () => foreign }, () => {});
		await scoped.run(projects, [], cwd, () => cwd);
		expect(scoped.state.error).toContain("different checkout");
		expect(scoped.state.display).toBeUndefined();
	});
	test("preserves complete original text when preview is missing or invalid and surfaces CLI stderr", async () => {
		for (const display of [null, { malformed: true }]) {
			const request = new GraphRequest(async () => result(JSON.stringify({ text: "Full result\nwith caveats and all omitted nodes", display }), { stderr: "server warning" }), deps, () => {});
			await request.run(projects, [], cwd, () => cwd);
			expect(request.state.text).toBe("Full result\nwith caveats and all omitted nodes");
			expect(request.state.error).toContain("server warning");
			if (display) expect(request.state.error).toContain("showing original result");
		}
	});
	test("does not turn execution/JSON failures into empty successful graphs; refresh clears an error", async () => {
		const cases = [
			result("original stdout diagnostic", { code: 2, stderr: "not authenticated" }),
			result("not JSON"), result(JSON.stringify({ wrong: "shape" })), result("", { killed: true, code: -1 }),
		];
		for (const failure of cases) {
			const request = new GraphRequest(async () => failure, deps, () => {});
			await request.run(projects, [], cwd, () => cwd);
			expect(request.state.loading).toBe(false);
			expect(request.state.error).toBeTruthy();
			expect(request.state.display).toBeUndefined();
		}
		let failed = true;
		const request = new GraphRequest(async () => { if (failed) throw new Error("spawn failed"); return result(JSON.stringify({ text: "recovered" })); }, deps, () => {});
		await request.run(projects, [], cwd, () => cwd);
		expect(request.state.error).toBe("spawn failed");
		failed = false;
		await request.run(projects, [], cwd, () => cwd);
		expect(request.state.error).toBeUndefined();
		expect(request.state.text).toBe("recovered");
	});
});

type View = Omit<Component, "describe" | "handleInput" | "handleNativeEvent"> & { describe(cx: DescribeContext): NativeNode; handleInput(data: string): void; handleNativeEvent(event: NativeUiEvent): void };
function harness(extra: Partial<GraphOverviewDependencies> = {}) {
	const host = pendingExecutor();
	const views: View[] = [], answers: (string | undefined)[] = [], prompts: string[] = [], notifications: string[] = [];
	let renders = 0;
	const ctx = { cwd, hasUI: true, ui: {
		custom(factory: (tui: unknown, theme: unknown, keys: unknown, done: (value: string | undefined) => void) => View) {
			const pending = Promise.withResolvers<string | undefined>();
			views.push(factory({ terminal: { rows: 24 }, hasModalOverlay: () => false, requestRender: () => renders++ }, {}, { matches: (data: string, action: string) => data === action }, pending.resolve));
			return pending.promise;
		},
		input: async (title: string) => { prompts.push(title); return answers.shift(); },
		notify: (message: string) => notifications.push(message),
	} } as unknown as ExtensionContext;
	const pi = { exec: host.exec } as ExtensionAPI;
	const overview = createGraphOverview(pi, { ...deps, ...extra });
	return { ...host, overview, ctx, views, answers, prompts, notifications, get renders() { return renders; } };
}
async function settle(): Promise<void> { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); }
function action(view: View, act: string): void { view.handleNativeEvent({ type: "action", key: "", act, mods: [] }); }

describe("native fullscreen overview", () => {
	test("defaults to complete indexed overview, shows configured CLI evidence and cancels immediately on Escape", async () => {
		const h = harness(); const open = h.overview.open(h.ctx);
		expect(h.pending[0].args).toEqual(["graph", "--json", "overview", "--"]);
		expect(content(h.views[0].describe(native))).toContain("configured semctl CLI");
		expect(content(h.views[0].describe(native))).toContain(cwd);
		expect(nodes(h.views[0].describe(native)).some(n => n.k === "spinner")).toBe(true);
		expect(nodes(h.views[0].describe({ ...native, reduceMotion: true })).some(n => n.k === "spinner")).toBe(false);
		h.views[0].handleInput("tui.select.cancel");
		await open;
		expect(h.pending[0].options?.signal?.aborted).toBe(true);
	});
	test("native selection prompts required endpoints, uses exact CLI family and never starts extra work on observations", async () => {
		const h = harness(); const open = h.overview.open(h.ctx);
		h.answers.push("fromSymbol", "toSymbol");
		action(h.views[0], "analyses");
		h.views[0].handleNativeEvent({ type: "activate", key: "analyses", item: "call-path" });
		await settle();
		expect(h.prompts).toEqual(["Shortest call path · From symbol", "Shortest call path · To symbol"]);
		expect(h.pending[0].options?.signal?.aborted).toBe(true);
		expect(h.pending[1].args).toEqual(["graph", "--json", "call-path", "--", "fromSymbol", "toSymbol"]);
		h.overview.observe({ type: "tool_call", toolName: "mcp__semctx_trace", toolCallId: "one", input: {} }, h.ctx);
		expect(h.pending).toHaveLength(2);
		h.overview.reset(); await open;
		expect(h.pending[1].options?.signal?.aborted).toBe(true);
	});
	test("all analysis families are selectable native items, not inert labels", async () => {
		const h = harness(); const open = h.overview.open(h.ctx);
		action(h.views[0], "analyses");
		const items = nodes(h.views[0].describe(native)).filter(n => n.k === "item");
		expect(items.map(n => n.key)).toEqual(GRAPH_OPERATIONS.map(op => op.id));
		for (const family of ["overview", "projects", "trace", "call-graph", "call-path", "reaches", "flows-into", "flows-between", "type-hierarchy", "cycles", "duplicates", "unused", "imports", "symbol-edges", "external-links", "outline"]) expect(items.some(n => n.key === family)).toBe(true);
		h.views[0].handleInput("tui.select.cancel"); // Close chooser, not the overview.
		expect(h.pending[0].options?.signal?.aborted).toBe(false);
		h.views[0].handleInput("tui.select.cancel"); await open;
	});
	test("correlates only actual semctx requests and scoped producer snapshots; reduced motion never invents activity", async () => {
		const h = harness({ readStatusUpdate: params => {
			if (!params || typeof params !== "object" || !("root" in params) || !("text" in params)) return undefined;
			return { root: String(params.root), indicator: { text: String(params.text), tone: "accent", symbol: "status.running" }, summary: String(params.text) };
		} });
		const open = h.overview.open(h.ctx);
		h.pending[0].response.resolve(result(JSON.stringify({ text: "Projects complete" })));
		await settle();
		h.overview.observe({ type: "tool_call", toolName: "mcp__other_trace", toolCallId: "not-semctx" }, h.ctx);
		expect(content(h.views[0].describe(native))).not.toContain("Observed semctx requests");
		h.overview.observe({ type: "tool_call", toolName: "write", input: { path: "xd://mcp__semctx_call_graph" }, toolCallId: "call" }, h.ctx);
		expect(content(h.views[0].describe(native))).toContain("Observed semctx requests: call_graph");
		h.overview.observe({ type: "tool_result", toolCallId: "other" }, h.ctx);
		expect(content(h.views[0].describe(native))).toContain("Observed semctx requests: call_graph");
		h.overview.observe({ type: "tool_result", toolCallId: "call" }, h.ctx);
		expect(content(h.views[0].describe(native))).not.toContain("Observed semctx requests");
		h.overview.observe({ server: "semctx", method: "notifications/semctl/sync_status", params: { root: path.resolve("unrelated"), text: "private other checkout" } }, h.ctx);
		expect(content(h.views[0].describe(native))).not.toContain("private other checkout");
		h.overview.observe({ server: "semctx", method: "notifications/semctl/sync_status", params: { root: cwd, text: "7 files embedded, job running" } }, h.ctx);
		expect(content(h.views[0].describe(native))).toContain("Last producer observation: 7 files embedded, job running");
		expect(nodes(h.views[0].describe(native)).some(n => n.k === "spinner")).toBe(false);
		expect(h.pending).toHaveLength(1);
		h.overview.reset(); await open;
	});
	test("replacement, checkout switching and shutdown reset abort pending CLI work and close native views", async () => {
		const h = harness();
		const first = h.overview.open(h.ctx), second = h.overview.open(h.ctx);
		await first;
		expect(h.pending[0].options?.signal?.aborted).toBe(true);
		h.overview.observe({ type: "session_switch" }, { ...h.ctx, cwd: path.resolve("different") });
		await second;
		expect(h.pending[1].options?.signal?.aborted).toBe(true);
		const third = h.overview.open(h.ctx);
		h.overview.reset(); await third;
		expect(h.pending[2].options?.signal?.aborted).toBe(true);
	});
	test("native errors stay visible and page scrolling and read-only refresh work", async () => {
		const h = harness(); const open = h.overview.open(h.ctx);
		h.pending[0].response.resolve(result("", { code: 1, stderr: "graph service unavailable" })); await settle();
		expect(content(h.views[0].describe(native))).toContain("graph service unavailable");
		h.views[0].handleInput("tui.select.pageDown");
		expect(nodes(h.views[0].describe(native)).find(n => n.key === "body")?.scroll?.by).toBe("page-down");
		action(h.views[0], "refresh");
		expect(h.pending).toHaveLength(2);
		expect(content(h.views[0].describe(native))).not.toContain("graph service unavailable");
		h.overview.reset(); await open;
	});
	test("only correlated requests in the graph codebase highlight native nodes", async () => {
		const display: Display = { version: 1, kind: "graph", identity: { codebaseId: "current", localRoot: cwd, name: null, copy: "checkout" },
			title: "Projects", query: null, nodes: [{ id: "root", label: "run", detail: null, hit: null }], edges: [], roots: ["root"], caveats: [], truncated: false };
		const h = harness({ readDisplay, createInspector: (data, render) => new SemctlInspector(data, render) });
		const open = h.overview.open(h.ctx);
		h.pending[0].response.resolve(result(JSON.stringify({ text: "Complete projects", display }))); await settle();
		const highlighted = () => nodes(h.views[0].describe(native)).some(n =>
			n.k === "text" && n.p?.spans?.some(span => span.t === "run" && span.s === "accent"));
		h.overview.observe({ type: "tool_call", toolName: "mcp__semctx_trace", input: { symbol: "run" }, toolCallId: "unscoped" }, h.ctx);
		expect(highlighted()).toBe(false);
		h.overview.observe({ type: "tool_result", toolCallId: "unscoped" }, h.ctx);
		h.overview.observe({ type: "tool_call", toolName: "mcp__semctx_trace", input: { symbol: "run", codebase: "foreign" }, toolCallId: "foreign" }, h.ctx);
		expect(highlighted()).toBe(false);
		h.overview.observe({ type: "tool_call", toolName: "write", input: { path: "xd://mcp__semctx_trace",
			content: JSON.stringify({ symbol: "run", codebase: "current" }) }, toolCallId: "matched" }, h.ctx);
		expect(highlighted()).toBe(true);
		h.overview.observe({ type: "tool_result", toolCallId: "foreign" }, h.ctx);
		expect(highlighted()).toBe(true);
		h.overview.observe({ type: "tool_result", toolCallId: "matched" }, h.ctx);
		expect(highlighted()).toBe(false);
		h.overview.reset(); await open;
	});
	test("real producer progress stays below full until completion and details remain folded", async () => {
		const h = harness({ readStatusUpdate });
		const open = h.overview.open(h.ctx);
		for (const state of ["running", "completed"]) {
			for (const files_embedded of state === "running" ? [3, 9, 10] : [10]) {
				h.overview.observe({ server: "semctx", method: "notifications/semctl/sync_status", params: { version: 1,
					status: { root: cwd, codebase_id: "current", last_error: null, sync_progress: null,
						running: false, pending_triggers: 0, watcher: "active", first_index: "ready",
						last_job_id: "diagnostic-job", embedding: { job_id: "diagnostic-job", state, files_embedded,
							files_to_embed: 10, files_failed: 0, error: null } } } }, h.ctx);
				const body = h.views[0].describe(native);
				const meter = nodes(body).find(node => node.k === "meter");
				if (!meter || meter.k !== "meter") throw new Error("Missing producer meter");
				if (state === "completed") expect(meter.p?.value).toBe(1);
				else { expect(meter.p?.value).toBeGreaterThan(0); expect(meter.p?.value).toBeLessThan(1); }
				expect(content(body)).not.toContain("diagnostic-job");
			}
		}
		h.views[0].handleNativeEvent({ type: "toggle", key: "producer", collapsed: false });
		expect(content(h.views[0].describe(native))).toContain("diagnostic-job");
		h.overview.reset(); await open;
	});
	test("old hosts without native custom/input/exec receive CLI guidance instead of a TypeError", async () => {
		const h = harness();
		const noCustom = { ...h.ctx, ui: { ...h.ctx.ui, custom: undefined } } as unknown as ExtensionContext;
		await h.overview.open(noCustom);
		expect(h.pending).toHaveLength(0);
		expect(h.notifications.at(-1)).toContain("semctl graph --json overview");
		await createGraphOverview({} as ExtensionAPI, deps).open(h.ctx);
		expect(h.pending).toHaveLength(0);
		const noInput = { ...h.ctx, ui: { ...h.ctx.ui, input: undefined } } as unknown as ExtensionContext;
		const open = h.overview.open(noInput);
		action(h.views[0], "analyses");
		h.views[0].handleNativeEvent({ type: "activate", key: "analyses", item: "trace" });
		await settle();
		expect(h.notifications.at(-1)).toContain("semctl graph --json trace");
		expect(h.pending.every(call => call.args[2] === "overview")).toBe(true);
		h.overview.reset(); await open;
	});
});

describe("native live graph popup", () => {
	test("keeps the last valid map through refresh failure and cancels queued work on checkout reset", async () => {
		const display: Display = { version: 1, kind: "graph", identity: { codebaseId: "popup-current", localRoot: cwd, name: null, copy: "checkout" },
			title: "Project overview", query: null, nodes: [{ id: "a", label: "a.ts", category: "file", path: "src/a.ts", detail: null, hit: null }],
			edges: [], roots: ["a"], caveats: [], truncated: false };
		const h = harness({ readDisplay });
		await h.overview.togglePopup(h.ctx);
		h.pending[0].response.resolve(result(JSON.stringify({ text: "map", display }))); await settle();
		h.views[0].handleNativeEvent({ type: "toggle", key: "groups", collapsed: false });
		const exposesSource = (root: NativeNode) => nodes(root).some(node => typeof node.p?.title === "string" && node.p.title.includes("src/a.ts"));
		expect(exposesSource(h.views[0].describe(native))).toBe(true);
		action(h.views[0], "refresh");
		action(h.views[0], "refresh");
		expect(exposesSource(h.views[0].describe(native))).toBe(true);
		expect(h.pending).toHaveLength(2);
		h.pending[1].response.resolve(result("", { code: 1, stderr: "offline" })); await settle();
		expect(h.pending).toHaveLength(3);
		h.pending[2].response.resolve(result("", { code: 1, stderr: "still offline" })); await settle();
		const failed = h.views[0].describe(native);
		expect(exposesSource(failed)).toBe(true);
		expect(nodes(failed).some(node => typeof node.p?.title === "string" && node.p.title.includes("still offline"))).toBe(true);
		action(h.views[0], "refresh");
		h.ctx.cwd = path.resolve("another-popup-checkout");
		h.overview.observe({ type: "tool_result", toolCallId: "untracked" }, h.ctx);
		expect(h.pending[3].options?.signal?.aborted).toBe(true);
		h.pending[3].response.resolve(result(JSON.stringify({ text: "stale", display }))); await settle();
		await h.overview.togglePopup(h.ctx);
		expect(exposesSource(h.views[1].describe(native))).toBe(false);
		h.overview.reset();
	});
	test("reuses the cached map after toggling without rerunning the graph CLI", async () => {
		const h = harness();
		await h.overview.togglePopup(h.ctx);
		h.pending[0].response.resolve(result(JSON.stringify({ text: "empty", display: null }))); await settle();
		action(h.views[0], "close");
		await h.overview.togglePopup(h.ctx);
		expect(h.pending).toHaveLength(1);
		h.overview.reset();
	});
});

describe("native observation engine", () => {
	test("clamps observed producer progress and removes invalid snapshots without starting extra work", async () => {
		const h = harness({ readStatusUpdate: params => {
			if (!params || typeof params !== "object" || !("root" in params) || !("progress" in params)) return undefined;
			return { root: String(params.root), indicator: { text: "Observed producer", tone: "accent", symbol: "status.running", progress: Number(params.progress) } };
		} });
		const open = h.overview.open(h.ctx);
		for (const [progress, expected] of [[2, 1], [-1, 0], [0.4, 0.4]]) {
			h.overview.observe({ server: "semctx", method: "notifications/semctl/sync_status", params: { root: cwd, progress } }, h.ctx);
			const meter = nodes(h.views[0].describe(native)).find(node => node.k === "meter");
			if (!meter || meter.k !== "meter") throw new Error("Missing producer meter");
			expect(meter.p?.value).toBe(expected);
		}
		h.overview.observe({ server: "semctx", method: "notifications/semctl/sync_status", params: { root: cwd, progress: Number.NaN } }, h.ctx);
		expect(nodes(h.views[0].describe(native)).some(node => node.k === "meter")).toBe(false);
		h.overview.observe({ server: "semctx", method: "notifications/semctl/sync_status", params: { root: cwd } }, h.ctx);
		expect(content(h.views[0].describe(native))).not.toContain("Observed producer");
		expect(h.pending).toHaveLength(1);
		h.overview.reset(); await open;
	});
	test("a retained completed job cannot fill the live ring after a newer upload", async () => {
		const h = harness({ readStatusUpdate });
		await h.overview.togglePopup(h.ctx);
		const job = { job_id: "old", state: "completed", files_embedded: 10, files_to_embed: 10, files_failed: 0, error: null };
		const status = { root: cwd, codebase_id: "local", running: false, pending_triggers: 0, last_error: null,
			first_index: "ready", sync_progress: null, watcher: "active", last_job_id: "old", embedding: job };
		const observe = (changes: Record<string, unknown>) => {
			h.overview.observe({ server: "semctx", method: "notifications/semctl/sync_status",
				params: { version: 1, status: { ...status, ...changes } } }, h.ctx);
			return nodes(h.views[0].describe(native)).filter(node => node.k === "meter").map(node => node.p?.value);
		};
		try {
			expect(observe({ running: true, sync_progress: { uploading: { uploaded_files: 3, total_files: 3 } } })).toEqual([0.99]);
			expect(observe({})).toEqual([]);
			expect(observe({ last_job_id: "new", embedding: { ...job, job_id: "new", state: "running", files_embedded: 0, files_to_embed: 5 } })).toEqual([0]);
			expect(observe({ last_job_id: "new", embedding: { ...job, job_id: "new", files_embedded: 5, files_to_embed: 5 } })).toEqual([1]);
		} finally { h.overview.reset(); }
	});
	test("popup Overview opens the native analysis surface and keeps catalog work single-flight", async () => {
		const h = harness();
		await h.overview.togglePopup(h.ctx);
		expect(h.pending).toHaveLength(1);
		action(h.views[0], "expand");
		await settle();
		expect(h.views).toHaveLength(2);
		expect(h.pending[1].command).toBe("semctl");
		expect(h.pending[1].args).toEqual(["graph", "--json", "overview", "--"]);
		action(h.views[1], "analyses");
		h.views[1].handleNativeEvent({ type: "activate", key: "analyses", item: "imports" });
		await settle();
		expect(h.pending[2].args).toEqual(["graph", "--json", "imports", "--"]);
		expect(h.pending[1].options?.signal?.aborted).toBe(true);
		const catalog = h.overview.warm(h.ctx);
		expect(h.pending).toHaveLength(3);
		h.overview.reset();
		expect(h.pending[0].options?.signal?.aborted).toBe(true);
		expect(h.pending[2].options?.signal?.aborted).toBe(true);
		h.pending[0].response.resolve(result(JSON.stringify({ text: "late catalog" })));
		await catalog;
	});
	test("unrelated events and repeated unchanged producer observations do not rerender or run graph work", async () => {
		const h = harness({ readStatusUpdate: params => params && typeof params === "object" && "root" in params
			? { root: String(params.root), indicator: { text: "Reported producer state", tone: "accent", symbol: "status.running" } } : undefined });
		await h.overview.togglePopup(h.ctx);
		h.pending[0].response.resolve(result(JSON.stringify({ text: "empty", display: null }))); await settle();
		const before = h.renders;
		for (const event of [
			{ type: "tool_call", toolName: "mcp__other_trace", toolCallId: "other", input: {} },
			{ type: "tool_result", toolCallId: "not-observed" },
			{ type: "turn_start" },
			{ server: "other", method: "notifications/semctl/sync_status", params: {} },
		]) h.overview.observe(event, h.ctx);
		expect(h.renders).toBe(before);
		const update = { server: "semctx", method: "notifications/semctl/sync_status", params: { root: cwd } };
		h.overview.observe(update, h.ctx);
		const reported = h.renders;
		expect(reported).toBeGreaterThan(before);
		h.overview.observe(update, h.ctx);
		h.overview.observe({ ...update, params: { root: path.resolve("unrelated-checkout") } }, h.ctx);
		expect(h.renders).toBe(reported);
		expect(h.pending).toHaveLength(1);
		h.overview.reset();
	});
});

describe("read-only graph argument validation", () => {
	test("rejects unknown operations, wrong arity, controls and invalid line ranges", () => {
		for (const [operation, args] of [
			["index", []], ["source", ["a.ts"]], ["projects", ["extra"]], ["trace", []],
			["trace", ["x\u0000y"]], ["trace", ["x".repeat(4097)]], ["trace", [42]], ["trace", ["   "]],
			["symbol-at-position", ["a.ts", "0"]], ["symbol-at-position", ["a.ts", "4294967296"]],
			["expand", ["a.ts", "3", "2"]],
		] as const) expect(() => graphArguments(operation, args)).toThrow();
		expect(graphArguments("trace", [" name; $(not-shell) "]).args).toEqual(["name; $(not-shell)"]);
		expect(graphArguments("expand", ["a.ts", "1", "4294967295"]).args).toEqual(["a.ts", "1", "4294967295"]);
	});
	test("invalid requests never reach the executor", async () => {
		const host = pendingExecutor(), request = new GraphRequest(host.exec, deps, () => {});
		await request.run(GRAPH_OPERATIONS.find(operation => operation.id === "trace")!, [], cwd, () => cwd);
		expect(host.pending).toHaveLength(0);
		expect(request.state.error).toContain("argument count");
	});
});

function indexedOverview(count: number) {
	return { version: 1, kind: "graph", identity: { codebaseId: "A", name: null, localRoot: cwd, copy: "checkout" },
		title: "Indexed project overview", query: null,
		nodes: Array.from({ length: count }, (_, i) => ({ id: String(i), label: `file-${i}.rs`, detail: null, hit: null, category: "file", path: `file-${i}.rs` })),
		edges: Array.from({ length: Math.max(0, count - 1) }, (_, i) => ({ from: String(i), to: String(i + 1), label: "imports" })),
		roots: count ? ["0"] : [], caveats: ["Complete indexed structure; resolved imports only."], truncated: false };
}
describe("explicit local overview display budgets", () => {
	test("preserves thousands of compact nodes only at the explicit local overview boundary", async () => {
		const value = indexedOverview(3000);
		expect(Buffer.byteLength(JSON.stringify(value))).toBeGreaterThan(32 * 1024);
		expect(readDisplay(value, "overview")).toBeUndefined();
		expect(readDisplay(value, "overview", true)).toBeDefined();
		expect(readDisplay(value, "imports")).toBeUndefined();
		expect(readDisplay(value, "imports", true)).toBeUndefined();
		const observed: { operation: string; localOverview?: boolean }[] = [];
		const request = new GraphRequest(async () => result(JSON.stringify({ text: "3000 indexed files", display: value })), {
			...deps, readDisplay(value, operation, localOverview) { observed.push({ operation, localOverview }); return readDisplay(value, operation, localOverview); },
		}, () => {});
		await request.run(GRAPH_OPERATIONS[0], [], cwd, () => cwd);
		expect(observed).toEqual([{ operation: "overview", localOverview: true }]);
		expect(request.state.display?.kind).toBe("graph");
		expect(request.state.error).toBeUndefined();
		await request.run(projects, [], cwd, () => cwd);
		expect(observed.at(-1)).toEqual({ operation: "list_projects", localOverview: false });
		expect(request.state.display).toBeUndefined();
	});
	test("rejects oversized and corrupt local graphs without weakening ordinary topology validation", () => {
		expect(readDisplay(indexedOverview(12_001), "overview", true)).toBeUndefined();
		const excessiveEdges = indexedOverview(1);
		excessiveEdges.edges = Array.from({ length: 48_001 }, () => ({ from: "0", to: "0", label: "imports" }));
		expect(readDisplay(excessiveEdges, "overview", true)).toBeUndefined();
		const excessiveBytes = { ...indexedOverview(1), nodes: [{ ...indexedOverview(1).nodes[0], detail: "x".repeat(3 * 1024 * 1024) }] };
		expect(readDisplay(excessiveBytes, "overview", true)).toBeUndefined();
		const dangling = indexedOverview(2); dangling.edges[0].to = "absent";
		expect(readDisplay(dangling, "overview", true)).toBeUndefined();
		const duplicate = indexedOverview(2); duplicate.nodes[1].id = "0";
		expect(readDisplay(duplicate, "overview", true)).toBeUndefined();
		for (const path of ["../escape", "/absolute", "C:/absolute", "src//file", "src/./file", "src/\u0000file"]) {
			const corrupt = indexedOverview(1); corrupt.nodes[0].path = path;
			expect(readDisplay(corrupt, "overview", true)).toBeUndefined();
		}
		expect(readDisplay({ ...indexedOverview(1), nodes: [{ ...indexedOverview(1).nodes[0], category: ["file"] }] }, "overview", true)).toBeUndefined();
	});
	test("resolved reference metadata keeps exact evidence and rejects fabricated or malformed bindings", () => {
		const edge = { from: "0", to: "1", label: "references", kind: "reference", symbol: "WireFormat", moniker: "file-1.rs#10" };
		const read = (candidate: unknown) => readDisplay({ ...indexedOverview(2), edges: [candidate] }, "overview", true);
		expect(read(edge)).toBeDefined();
		for (const candidate of [
			{ ...edge, kind: "similarity" }, { ...edge, kind: ["reference"] },
			{ ...edge, symbol: "" }, { ...edge, symbol: ["WireFormat"] },
			{ ...edge, symbol: undefined }, { ...edge, moniker: undefined },
			{ ...edge, moniker: "unsafe\u0000identity" }, { ...edge, symbol: "x".repeat(4097) },
			{ ...edge, kind: "import" }, { ...edge, to: "missing" },
		]) expect(read(candidate)).toBeUndefined();
		const directory = indexedOverview(2);
		directory.nodes[1].category = "directory";
		expect(readDisplay({ ...directory, edges: [edge] }, "overview", true)).toBeUndefined();
		expect(readDisplay(indexedOverview(2), "imports")).toBeDefined();
	});
});

test("a legacy host cannot turn the live popup into a keyboard-blocking overlay", async () => {
	const h = harness();
	const legacy = { ...h.ctx, ui: { ...h.ctx.ui, custom: ((factory, ...args) => h.ctx.ui.custom((tui, ...rest) =>
		factory(new Proxy(tui, { get: (target, key) => key === "hasModalOverlay" ? undefined : Reflect.get(target, key) }), ...rest), ...args)) as ExtensionContext["ui"]["custom"] } };
	await h.overview.togglePopup(legacy);
	expect(h.views).toHaveLength(0);
	expect(h.pending).toHaveLength(0);
	await h.overview.togglePopup(h.ctx);
	expect(h.views).toHaveLength(1);
	expect(h.pending).toHaveLength(1);
	h.overview.reset();
});
