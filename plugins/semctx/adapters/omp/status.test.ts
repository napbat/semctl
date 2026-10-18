import { describe, expect, test } from "bun:test";
import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { installSyncStatus, readStatusUpdate } from "./status";

const root = path.resolve("status-fixture");
function status(overrides: Record<string, unknown> = {}) {
	return { version: 1, status: { root, codebase_id: "codebase", running: false, pending_triggers: 0,
		last_error: null, first_index: null, sync_progress: null, watcher: "active", ...overrides } };
}
function harness(cwd = root, hasUI = true) {
	type Handler = (event: Record<string, unknown>, ctx: ExtensionContext) => unknown;
	const handlers = new Map<string, Handler[]>();
	let shown: string | undefined;
	const pi = { on(event: string, handler: Handler) { handlers.set(event, [...(handlers.get(event) ?? []), handler]); } } as unknown as ExtensionAPI;
	const ctx = { cwd, hasUI, ui: {
		setStatus(_key: string, text: string | undefined) { shown = text; },
		theme: { fg: (_tone: string, text: string) => text, symbol: (name: string) => name },
	} } as unknown as ExtensionContext;
	installSyncStatus(pi);
	const emit = (name: string, event: Record<string, unknown>) => { for (const handler of handlers.get(name) ?? []) handler(event, ctx); };
	return { ctx, get shown() { return shown; }, emit,
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
		expect(readStatusUpdate(status({ last_error: error }))?.indicator).toMatchObject({ text: error, tone: "error", symbol: "status.error" });
		expect(readStatusUpdate(status({ first_index: { failed: { reason: error } } }))?.indicator?.tone).toBe("error");
		expect(readStatusUpdate(status({ watcher: { unavailable: "watch handle failed" } }))?.indicator?.tone).toBe("warning");
	});
	test("rejects unbound, malformed and unknown-version observations without guessing a phase", () => {
		for (const value of [undefined, { version: 2, status: status().status }, status({ root: "relative" }), status({ root: "bad\0path" }),
			status({ running: "true" }), status({ pending_triggers: -1 }), status({ first_index: { failed: { reason: 42 } } }),
			status({ first_index: "unknown-new-phase" })]) expect(readStatusUpdate(value)).toBeUndefined();
	});
});

describe("session layout ownership", () => {
	test("other servers, methods and sibling checkouts cannot replace the current indicator", () => {
		const h = harness(); h.notify(status()); const original = h.shown;
		h.notify(status({ last_error: "unrelated" }), "semctx-other");
		h.notify(status({ last_error: "unrelated" }), "semctx", "notifications/other");
		h.notify(status({ root: `${root}-sibling`, last_error: "unrelated" }));
		expect(h.shown).toBe(original);
		h.notify(status({ running: true, sync_progress: { uploading: { uploaded_files: 2, total_files: 9 } } }));
		expect(h.shown).toContain("2/9");
	});
	test("the most specific checkout owns nested cwd and detach returns to its observed parent", () => {
		const child = path.join(root, "nested"), h = harness(path.join(child, "src"));
		h.notify(status({ last_error: "parent failure" }));
		h.notify(status({ root: child }));
		expect(h.shown).toContain("status.info");
		h.notify(status({ last_error: "parent changed" }));
		expect(h.shown).toContain("status.info");
		h.notify({ version: 1, root: child, status: null });
		expect(h.shown).toContain("parent changed");
	});
	test("malformed observations, detach, cwd changes and shutdown clear stale state", () => {
		const h = harness(); h.notify(status());
		h.notify({ version: 2, status: status().status }); expect(h.shown).toBeUndefined();
		h.notify(status()); h.notify({ version: 1, status: null, root }); expect(h.shown).toBeUndefined();
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
