import type { CustomTool as Tool, ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import type { MCPManager } from "@oh-my-pi/pi-coding-agent/mcp";
import type { ToolRenderer } from "@oh-my-pi/pi-tui/tools";
import { createInspectorRenderer, supportsInspector } from "./inspector";
import { requestNativeRender } from "./native-surface";

type Manager = Pick<MCPManager, "getTools" | "addToolsChangedListener">;
type NativeHook = "describeCall" | "describeResult";
interface Hook {
	key: NativeHook;
	original: PropertyDescriptor | undefined;
	installed: Tool[NativeHook];
}
interface Decoration {
	owners: Set<ManagerState>;
	hooks: Hook[];
}
interface ManagerState {
	manager: Manager;
	owners: Set<ExtensionAPI>;
	tools: WeakRef<Tool>[];
	unsubscribe?: () => void;
}
const managers = new WeakMap<Manager, ManagerState>();
const decorations = new WeakMap<Tool, Decoration>();

function releaseTool(tool: Tool, state: ManagerState): void {
	const decoration = decorations.get(tool);
	if (!decoration || !decoration.owners.delete(state) || decoration.owners.size > 0) return;
	for (const hook of decoration.hooks) {
		// Another extension may have replaced a hook after us. It owns that change.
		if (tool[hook.key] !== hook.installed) continue;
		if (hook.original) Object.defineProperty(tool, hook.key, hook.original);
		else delete tool[hook.key];
	}
	decorations.delete(tool);
}

function refresh(state: ManagerState): void {
	if (state.owners.size === 0) return;
	// Retained transcript adapters may still own disconnected tool instances.
	// Keep their hooks until shutdown, without rooting abandoned instances here.
	let live = 0;
	for (const reference of state.tools) if (reference.deref()) state.tools[live++] = reference;
	state.tools.length = live;
	for (const tool of state.manager.getTools()) {
		const server = tool.mcpServerName, operation = tool.mcpToolName;
		if (typeof server !== "string" || typeof operation !== "string" || !supportsInspector(server, operation)) continue;
		const decoration = decorations.get(tool);
		if (decoration?.owners.has(state)) continue;
		state.tools.push(new WeakRef(tool));
		if (decoration) {
			decoration.owners.add(state);
			continue;
		}
		// Bind the stock hooks before replacing anything: MCP methods read their
		// original instance. ANSI hooks themselves remain completely untouched.
		const fallback: ToolRenderer = {
			renderCall: tool.renderCall?.bind(tool) ?? (() => undefined),
			renderResult: tool.renderResult?.bind(tool) ?? (() => undefined),
			describeCall: tool.describeCall?.bind(tool),
			describeResult: tool.describeResult?.bind(tool),
		};
		const renderer = createInspectorRenderer(server, operation)({ requestRender: requestNativeRender, fallback });
		const hooks: Hook[] = [];
		for (const key of ["describeCall", "describeResult"] as const) {
			const installed = renderer[key];
			if (!installed) continue;
			const original = Object.getOwnPropertyDescriptor(tool, key);
			hooks.push({ key, original, installed });
			Object.defineProperty(tool, key, original ? { ...original, value: installed } : {
				value: installed, configurable: true, writable: true, enumerable: false,
			});
		}
		decorations.set(tool, { owners: new Set([state]), hooks });
	}
}

/** Decorate stock MCP presentation only; execution and approval stay with OMP. */
export function installMcpPresentation(pi: ExtensionAPI, manager: Manager): void {
	let state = managers.get(manager);
	if (state?.owners.has(pi)) return;
	if (!state) {
		state = { manager, owners: new Set(), tools: [] };
		managers.set(manager, state);
		const shared = state;
		state.unsubscribe = manager.addToolsChangedListener(() => refresh(shared));
	}
	state.owners.add(pi);
	refresh(state);
	const shared = state;
	pi.on("session_shutdown", () => {
		if (!shared.owners.delete(pi) || shared.owners.size > 0) return;
		shared.unsubscribe?.();
		for (const reference of shared.tools) {
			const tool = reference.deref();
			if (tool) releaseTool(tool, shared);
		}
		shared.tools.length = 0;
		managers.delete(manager);
	});
}
