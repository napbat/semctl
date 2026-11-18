import { describe, expect, test } from "bun:test";
import type { CustomTool, ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import type { MCPManager } from "@oh-my-pi/pi-coding-agent/mcp";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { NativeToolView, RenderResultOptions, ToolRenderResult } from "@oh-my-pi/pi-tui/tools";
import { SemctlInspector } from "./inspector";
import { installMcpPresentation } from "./mcp-presentation";

type Manager = Pick<MCPManager, "getTools" | "addToolsChangedListener">;
const options: RenderResultOptions = { expanded: false, isPartial: false };
class StockTool {
	name = "mcp__semctx_search_codebase";
	label = "stock label";
	description = "stock description";
	parameters = { type: "object" };
	approval = "write";
	loadMode = "discoverable";
	mcpReadOnlyHint = true;
	calls = 0;
	constructor(public mcpServerName = "semctx", public mcpToolName = "search_codebase") {}
	formatApprovalDetails() { return this.label; }
	async execute() {
		this.calls++;
		return { content: [{ type: "text", text: this.label }] };
	}
	renderCall() { return { render: () => [this.label] }; }
	renderResult() { return { render: () => [`${this.label} ANSI result`] }; }
	describeCall(_args: unknown, _options: RenderResultOptions): NativeToolView {
		return { tool: { title: this.label }, body: [{ k: "text", p: { text: `${this.label} native call` } }] };
	}
	describeResult(_result: ToolRenderResult, _options: RenderResultOptions, _args?: unknown): NativeToolView {
		return { body: [{ k: "text", p: { text: `${this.label} native result` } }] };
	}
}
function session() {
	const shutdown: (() => void)[] = [];
	const pi = { on(event: string, handler: () => void) { if (event === "session_shutdown") shutdown.push(handler); } } as unknown as ExtensionAPI;
	return { pi, shutdown, close() { for (const callback of shutdown) callback(); } };
}
function manager(...initial: StockTool[]) {
	let tools = initial, removed = 0;
	const listeners = new Set<() => void>();
	const api: Manager = {
		getTools() { return tools as unknown as CustomTool[]; },
		addToolsChangedListener(listener) {
			listeners.add(listener);
			return () => { removed++; listeners.delete(listener); };
		},
	};
	return { api, listeners, get removed() { return removed; }, replace(...next: StockTool[]) {
		tools = next;
		for (const listener of listeners) listener();
	} };
}
function result(server = "semctx", query = "query"): ToolRenderResult {
	return { content: [{ type: "text", text: "unchanged producer output" }], details: {
		serverName: server, mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: {
			version: 1, kind: "search", query, indexWarning: null, hits: [{
				path: "src/main.ts", symbol: "main", qualifiedName: "main", language: "typescript", kind: "function", score: 2,
				startLine: 1, endLine: 1, startByte: null, endByte: null, snippet: "function main() {}",
				snippetStartLine: 1, snippetEndLine: 1, snippetStartByte: null, snippetEndByte: null, focusLine: 1, focusByte: null,
				codebaseId: "cb", binding: { state: "not-probed", reason: null },
			}],
			identity: { codebaseId: "cb", name: null, localRoot: null, copy: "canonical" },
		} },
	} };
}

describe("stock MCP presentation decoration", () => {
	test("only native presentation changes; execution, ANSI, identity, schema and approval remain stock", async () => {
		const tool = new StockTool(), m = manager(tool), s = session();
		const original = Object.getOwnPropertyDescriptors(tool);
		const execute = tool.execute, ansiCall = tool.renderCall, ansiResult = tool.renderResult;
		installMcpPresentation(s.pi, m.api);
		for (const [key, descriptor] of Object.entries(original)) expect(Object.getOwnPropertyDescriptor(tool, key)).toEqual(descriptor);
		expect(tool.execute).toBe(execute);
		expect(tool.renderCall).toBe(ansiCall);
		expect(tool.renderResult).toBe(ansiResult);
		expect(tool.calls).toBe(0);
		expect(tool.formatApprovalDetails()).toBe("stock label");
		expect(tool.renderResult().render()).toEqual(["stock label ANSI result"]);
		expect(tool.describeCall({ query: "native query" }, options).tool).toMatchObject({ title: "Code search", target: "native query" });
		expect(tool.describeResult(result(), options).body?.[0]).toBeInstanceOf(SemctlInspector);
		expect((await tool.execute()).content).toEqual([{ type: "text", text: "stock label" }]);
		expect(tool.calls).toBe(1);
		s.close();
		expect(Object.hasOwn(tool, "describeCall")).toBe(false);
		expect(Object.hasOwn(tool, "describeResult")).toBe(false);
	});
	test("raw identity matching excludes lookalikes and unsupported operations", () => {
		const tools = [new StockTool("semctx"), new StockTool("semctx:semctx"),
			new StockTool("semctx-other"), new StockTool("other:semctx"), new StockTool("semctx", "not_supported")];
		const m = manager(...tools), s = session();
		const originals = tools.map(tool => tool.describeResult);
		installMcpPresentation(s.pi, m.api);
		for (let i = 0; i < tools.length; i++) {
			if (i < 2) expect(tools[i].describeResult(result(tools[i].mcpServerName), options).body?.[0]).toBeInstanceOf(SemctlInspector);
			else expect(tools[i].describeResult).toBe(originals[i]);
		}
		s.close();
	});
	test("native fallback stays bound to its original tool for errors, streaming and malformed carriers", () => {
		const tool = new StockTool(), m = manager(tool), s = session();
		installMcpPresentation(s.pi, m.api);
		const describeResult = tool.describeResult;
		const cases: [ToolRenderResult, RenderResultOptions][] = [
			[{ content: [], details: {} }, options],
			[{ ...result(), isError: true }, options],
			[result("unrelated"), options],
			[result(), { ...options, isPartial: true }],
			[result(), { ...options, cancelled: true }],
		];
		for (const [value, state] of cases) {
			const fallback = describeResult(value, state).body?.[0] as NativeNode;
			expect(fallback).toMatchObject({ k: "text", p: { text: "stock label native result" } });
		}
		s.close();
	});
	test("shared sessions install once and restore only after the final owner shuts down", () => {
		const tool = new StockTool(), m = manager(tool), a = session(), b = session();
		const original = tool.describeResult;
		installMcpPresentation(a.pi, m.api);
		const wrapped = tool.describeResult;
		installMcpPresentation(a.pi, m.api);
		installMcpPresentation(b.pi, m.api);
		expect(a.shutdown).toHaveLength(1);
		expect(m.listeners.size).toBe(1);
		expect(tool.describeResult).toBe(wrapped);
		a.close();
		expect(tool.describeResult).toBe(wrapped);
		expect(m.removed).toBe(0);
		b.close();
		expect(tool.describeResult).toBe(original);
		expect(m.listeners.size).toBe(0);
		expect(m.removed).toBe(1);
		a.close(); b.close();
		expect(m.removed).toBe(1);
	});
	test("reconnect preserves retained match chips until the final session owner shuts down", () => {
		const old = new StockTool(), next = new StockTool(), m = manager(old), a = session(), b = session();
		const originalOld = old.describeResult, originalNext = next.describeResult;
		const cx: DescribeContext = { cols: 100, dark: true, reduceMotion: false, supports: () => true, feature: () => false };
		installMcpPresentation(a.pi, m.api);
		installMcpPresentation(b.pi, m.api);
		const wrappedOld = old.describeResult, value = result();
		const initial = old.describeResult(value, options).body?.[0];
		if (!(initial instanceof SemctlInspector)) throw new Error("Missing initial native inspector");
		expect(JSON.stringify(initial.describe(cx))).toContain('"key":"hit-0"');
		m.replace(next);
		expect(old.describeResult).toBe(wrappedOld);
		expect(next.describeResult).not.toBe(originalNext);
		const wrappedNext = next.describeResult;
		m.replace(next);
		expect(next.describeResult).toBe(wrappedNext);
		expect(old.describeResult(value, options).body?.[0]).toBe(initial);
		expect(JSON.stringify(initial.describe(cx))).toContain('"key":"hit-0"');
		expect(next.describeResult(result(), options).body?.[0]).toBeInstanceOf(SemctlInspector);
		a.close();
		expect(old.describeResult).toBe(wrappedOld);
		expect(old.describeResult(value, options).body?.[0]).toBe(initial);
		b.close();
		expect(old.describeResult).toBe(originalOld);
		expect(next.describeResult).toBe(originalNext);
		expect(Object.hasOwn(old, "describeResult")).toBe(false);
	});
	test("shared instances across managers cannot stack wrappers or restore another owner's hooks", () => {
		const tool = new StockTool(), one = manager(tool), two = manager(tool), a = session(), b = session();
		const original = tool.describeResult;
		installMcpPresentation(a.pi, one.api);
		const wrapped = tool.describeResult;
		installMcpPresentation(b.pi, two.api);
		expect(tool.describeResult).toBe(wrapped);
		one.replace();
		expect(tool.describeResult).toBe(wrapped);
		a.close();
		expect(tool.describeResult).toBe(wrapped);
		b.close();
		expect(tool.describeResult).toBe(original);
	});
	test("cleanup preserves subsequent changes and restores exact original own descriptors", () => {
		const tool = new StockTool(), m = manager(tool), s = session();
		Object.defineProperty(tool, "describeCall", { value: tool.describeCall, writable: true, configurable: true, enumerable: true });
		const original = Object.getOwnPropertyDescriptor(tool, "describeCall");
		installMcpPresentation(s.pi, m.api);
		const external = () => ({ body: [] });
		tool.describeResult = external;
		m.replace(tool);
		expect(tool.describeResult).toBe(external);
		s.close();
		expect(tool.describeResult).toBe(external);
		expect(Object.getOwnPropertyDescriptor(tool, "describeCall")).toEqual(original);
	});
});
