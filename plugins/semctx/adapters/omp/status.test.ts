import { describe, expect, test } from "bun:test";
import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext, ExtensionStatusOptions } from "@oh-my-pi/pi-coding-agent";
import type { Component, DescribeContext } from "@oh-my-pi/pi-tui";
import { installSyncStatus, readStatusUpdate } from "./status";

const root = path.resolve("status-fixture");
function status(overrides: Record<string, unknown> = {}) {
	return { version: 1, status: { root, codebase_id: "codebase", running: false, pending_triggers: 0,
		last_error: null, first_index: null, sync_progress: null, watcher: "active", ...overrides } };
}
function embedding(overrides: Record<string, unknown> = {}) {
	return { job_id: "current-job", state: "running", files_embedded: 3, files_to_embed: 10,
		files_failed: 0, error: null, ...overrides };
}
function harness(cwd = root, hasUI = true, options: { commands?: boolean; custom?: boolean; text?: boolean } = {}) {
	type Handler = (event: Record<string, unknown>, ctx: ExtensionContext) => unknown;
	type Command = (args: string, ctx: ExtensionContext) => Promise<void>;
	const handlers = new Map<string, Handler[]>(), commands = new Map<string, Command>(), notices: string[] = [];
	let shown: string | undefined, view: Component | undefined, closed = 0, renders = 0;
	let presentation: ExtensionStatusOptions | undefined;
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
		setStatus(_key: string, text: string | undefined, options?: ExtensionStatusOptions) { shown = text; presentation = options; },
		theme: { fg: (_tone: string, text: string) => text, symbol: (name: string) => name },
		notify(message: string) { notices.push(message); },
		...(options.custom === false ? {} : {
			async custom(factory: Parameters<ExtensionContext["ui"]["custom"]>[0]) {
				view = await factory({ terminal: { rows: 14 }, requestRender() { renders++; } } as never,
					{} as never, { matches: (data: string, key: string) => data === key } as never, () => { closed++; });
			},
		}),
	} } as unknown as ExtensionContext;
	installSyncStatus(pi);
	const emit = (name: string, event: Record<string, unknown>, context = ctx) => { for (const handler of handlers.get(name) ?? []) handler(event, context); };
	return { ctx, commands, notices, get shown() { return shown; }, get view() { return view; }, get presentation() { return presentation; },
		get closed() { return closed; }, get renders() { return renders; }, emit,
		command(args = "") { return commands.get("semctl")?.(args, ctx); },
		notify(params: unknown, server = "semctx", method = "notifications/semctl/sync_status") { emit("mcp_notification", { server, method, params }); },
	};
}

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
			expect(completed?.indicator?.text).toMatch(/^(?:preparing|scanning|planning)/);
			expect(completed?.details).toContain("sync_progress");
			const active = readStatusUpdate(status({ running: true, sync_progress, embedding: embedding({ files_embedded: 7 }) }));
			expect(active?.indicator).toMatchObject({ progress: 0.7, symbol: "status.running" });
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

describe("session layout ownership", () => {
	test("composer footprint stays bounded across progress, counts and long failures", () => {
		const h = harness(), labels: string[] = [];
		for (const update of [
			status(),
			status({ running: true, sync_progress: "preparing" }),
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
			labels.push(h.shown!.replace(/^(?:status\.\w+|[○◔◑◕●]) /, ""));
		}
		expect(new Set(labels.map(label => label.length)).size).toBe(1);
		expect(labels.every(label => !/[\r\n]/.test(label))).toBe(true);
		expect(labels.join("")).not.toContain(root);
		expect(labels.join("")).not.toContain("diagnostic");
	});
	test("progress remains distinguishable when the host removes colors", () => {
		const h = harness();
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		const before = h.shown;
		h.notify(status({ embedding: embedding({ files_embedded: 9 }) }));
		expect(h.shown).not.toBe(before);
		expect(h.shown?.length).toBe(before?.length);
	});
	test("other servers, methods and sibling checkouts cannot replace the current indicator", () => {
		const h = harness(); h.notify(status()); const original = h.shown;
		h.notify(status({ last_error: "unrelated" }), "semctx-other");
		h.notify(status({ last_error: "unrelated" }), "semctx", "notifications/other");
		h.notify(status({ root: `${root}-sibling`, last_error: "unrelated" }));
		expect(h.shown).toBe(original);
	});
	test("unrelated or unbound malformed snapshots and cache eviction cannot invalidate the current checkout", () => {
		const h = harness(); h.notify(status({ running: true, sync_progress: { uploading: { uploaded_files: 2, total_files: 9 } } }));
		const current = h.shown;
		h.notify({ version: 2, status: status({ root: `${root}-sibling` }).status });
		h.notify({ version: 1, status: { running: "malformed" } });
		for (let i = 0; i < 40; i++) h.notify(status({ root: `${root}-other-${i}`, last_error: "unrelated" }));
		expect(h.shown).toBe(current);
		h.notify({ version: 2, status: status().status });
		expect(h.presentation).toMatchObject({ tone: "muted", icon: "info" });
		expect(h.presentation?.progress).toBeUndefined();
	});
	test("the most specific checkout keeps an unknown slot after detach instead of inheriting parent progress", () => {
		const child = path.join(root, "nested"), h = harness(path.join(child, "src"));
		h.notify(status({ last_error: "parent failure" }));
		h.notify(status({ root: child }));
		expect(h.shown).toContain("status.info");
		h.notify(status({ last_error: "parent changed" }));
		expect(h.shown).toContain("status.info");
		h.notify({ version: 1, root: child, status: null });
		expect(h.presentation).toMatchObject({ tone: "muted", icon: "info" });
		expect(h.presentation?.progress).toBeUndefined();
	});
	test("missing observations retain an unknown slot; unrelated cwd and shutdown clear visible state", () => {
		const h = harness(); h.notify(status());
		h.notify({ version: 2, status: status().status }); expect(h.presentation?.tone).toBe("muted"); expect(h.presentation?.progress).toBeUndefined();
		h.notify(status()); h.notify({ version: 1, status: null, root }); expect(h.presentation?.tone).toBe("muted"); expect(h.presentation?.progress).toBeUndefined();
		h.notify(status()); h.ctx.cwd = path.resolve("different-checkout"); h.emit("session_switch", {}); expect(h.shown).toBeUndefined();
		h.ctx.cwd = root; h.emit("session_switch", {}); expect(h.shown).toContain("status.info");
		h.emit("session_shutdown", {}); expect(h.shown).toBeUndefined();
		h.emit("session_start", {}); expect(h.shown).toBeUndefined();
	});
	test("noninteractive sessions do not create layout output", () => {
		const h = harness(root, false); h.notify(status()); h.emit("session_shutdown", {});
		expect(h.shown).toBeUndefined();
	});
	if (process.platform === "win32") test("Windows extended roots and ordinary cwd resolve to the same owner", () => {
		const h = harness(root); h.notify(status({ root: `\\\\?\\${root}` }));
		expect(h.shown).toContain("status.info");
	});
});

describe("calm composer status transitions", () => {
	test("unknown totals remain static, while actual fractional progress opts into bounded native motion", () => {
		const h = harness();
		for (const update of [status({ running: true, sync_progress: "preparing" }),
			status({ running: true, sync_progress: { scanning: { root } } }),
			status({ embedding: embedding({ files_embedded: null, files_to_embed: null }) })]) {
			h.notify(update);
			expect(h.presentation?.progress).toBeUndefined();
			expect(h.presentation?.icon).toBeDefined();
			expect(h.presentation?.progressMotion).toBe("brief");
		}
		h.notify(status({ embedding: embedding({ files_embedded: 7 }) }));
		expect(h.presentation?.progress).toBe(0.7);
		h.notify(status({ embedding: embedding({ state: "completed", files_embedded: 10 }) }));
		const complete = h.presentation, fallback = h.shown;
		for (const sync_progress of ["preparing", { scanning: { root } }, { planning: { files: 288 } }]) {
			h.notify(status({ running: true, sync_progress, embedding: embedding({ state: "completed", files_embedded: 10 }) }));
			expect(h.presentation?.progress).toBe(1);
			expect(h.presentation?.tone).toBe(complete?.tone);
			expect(h.presentation?.icon).toBe(complete?.icon);
			expect(h.presentation?.onActivate).toBe(complete?.onActivate);
			expect(h.shown).toBe(fallback);
		}
		h.notify(status({ last_error: "current attempt failed" }));
		expect(h.presentation?.tone).toBe("error");
		expect(h.presentation?.progress).toBeUndefined();
	});
	test("detachment and session reset erase progress but retain only previously observed static slots", async () => {
		const h = harness();
		h.notify({ version: 1, root, status: null }); expect(h.shown).toBeUndefined();
		h.notify(status({ embedding: embedding({ state: "completed", files_embedded: 10 }) }));
		const activate = h.presentation?.onActivate;
		h.notify({ version: 1, root, status: null });
		expect(h.presentation).toMatchObject({ label: "semctl", tone: "muted", icon: "info", onActivate: activate });
		expect(h.presentation?.progress).toBeUndefined();
		await h.command("status");
		expect(h.view?.render(100).join("\n")).not.toContain("10/10");
		h.notify(status({ embedding: embedding({ files_embedded: 5 }) }));
		h.notify({ version: 1, status: null });
		expect(h.presentation?.progress).toBeUndefined();
		expect(h.presentation?.tone).toBe("muted");
		h.ctx.cwd = path.resolve("unrelated"); h.emit("session_switch", {});
		expect(h.shown).toBeUndefined();
		h.ctx.cwd = root; h.emit("session_switch", {});
		expect(h.presentation?.progress).toBeUndefined();
		h.emit("session_shutdown", {}); expect(h.shown).toBeUndefined();
		const beforeActivation = h.view;
		await activate?.(); // A retained old action cannot reopen a shut-down context.
		expect(h.view).toBe(beforeActivation);
	});
	test("fresh context objects do not replace the activation callback", async () => {
		const h = harness();
		h.notify(status({ embedding: embedding({ files_embedded: 3 }) }));
		const activate = h.presentation?.onActivate;
		const next = { ...h.ctx };
		h.emit("mcp_notification", { server: "semctx", method: "notifications/semctl/sync_status",
			params: status({ embedding: embedding({ files_embedded: 4 }) }) }, next);
		expect(h.presentation?.onActivate).toBe(activate);
		await activate?.();
		expect(h.view).toBeDefined();
	});
});

describe("native status inspection", () => {
	test("native activation opens the latest owned diagnostic without using a command or model turn", async () => {
		const h = harness(root, true, { commands: false });
		h.notify(status({ last_error: "older diagnostic" }));
		const activate = h.presentation?.onActivate;
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
		const first = h.view?.describe?.({} as DescribeContext);
		expect(JSON.stringify(first)).toContain("job-one");
		expect(JSON.stringify(first)).toContain("3/10");
		expect(JSON.stringify(first)).not.toContain("uploaded_files");
		await h.command();
		expect(JSON.stringify(h.view?.describe?.({} as DescribeContext))).toBe(JSON.stringify(first));
		h.notify(status({ last_error: full, retry_after_secs: 5, last_job_id: "job-two" }));
		h.notify(status({ root: `${root}-sibling`, last_error: "unrelated diagnostic" }));
		await h.command("details");
		const latest = JSON.stringify(h.view?.describe?.({} as DescribeContext));
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
		expect(JSON.stringify(h.view?.describe?.({} as DescribeContext))).not.toContain("owned diagnostic");
		h.ctx.cwd = root;
		h.notify({ version: 1, root, status: null });
		await h.command();
		expect(JSON.stringify(h.view?.describe?.({} as DescribeContext))).not.toContain("owned diagnostic");
		h.notify(status({ last_error: "owned diagnostic" }));
		h.emit("session_shutdown", {});
		await h.command();
		expect(JSON.stringify(h.view?.describe?.({} as DescribeContext))).not.toContain("owned diagnostic");
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
		expect(old.shown).toContain("status.info");
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
