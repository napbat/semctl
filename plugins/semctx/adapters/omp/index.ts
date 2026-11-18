import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { getAgentDir } from "@oh-my-pi/pi-coding-agent";
import { MCPManager } from "@oh-my-pi/pi-coding-agent/mcp";
import { matchesKey } from "@oh-my-pi/pi-tui";
import { encodeTspJson } from "@oh-my-pi/pi-tui/native/encode";
import { createSemctxExtension } from "./extension";
import { installMcpPresentation } from "./mcp-presentation";
import { configureNativeStyles } from "./native-surface";
import { GraphAppearanceStore } from "./graph-appearance";

export default function semctxExtension(pi: ExtensionAPI): void {
	// Tern advertises the styles verb. Stock OMP's encoder supports its framing,
	// but its older TypeScript verb union does not yet include "s".
	configureNativeStyles(sheet => encodeTspJson("s" as Parameters<typeof encodeTspJson>[0], sheet));
	// Interactive startup can preload extension factories before the MCP manager.
	// Session events run after OMP binds its live tools. Installation is idempotent.
	const install = (_event: unknown, ctx: ExtensionContext) => {
		if (!ctx.hasUI) return;
		const manager = MCPManager.instance();
		if (manager) installMcpPresentation(pi, manager);
	};
	pi.on("session_start", install);
	pi.on("session_switch", install);
	createSemctxExtension(undefined, new GraphAppearanceStore(path.join(getAgentDir(), "semctl", "graph-appearance.json")), matchesKey)(pi);
}
