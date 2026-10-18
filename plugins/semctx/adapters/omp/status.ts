import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";

const STATUS_KEY = "semctl.sync";
const METHOD = "notifications/semctl/sync_status";
type Obj = Record<string, unknown>;
const object = (value: unknown): value is Obj => value !== null && typeof value === "object" && !Array.isArray(value);
const count = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
const short = (value: string): string => value.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").replace(/\s+/g, " ").trim().slice(0, 72);
interface Indicator {
	text: string;
	tone: "accent" | "muted" | "warning" | "error";
	symbol: "status.running" | "status.info" | "status.warning" | "status.error" | "status.pending";
}
export interface StatusUpdate { root: string; indicator: Indicator | null }

function checkoutRoot(value: unknown): string | undefined {
	if (typeof value !== "string" || value.length === 0 || value.length > 4096 || /[\u0000-\u001f]/.test(value)) return undefined;
	const ordinary = value.startsWith("\\\\?\\UNC\\") ? `\\\\${value.slice(8)}` : value.startsWith("\\\\?\\") ? value.slice(4) : value;
	if (!path.isAbsolute(ordinary)) return undefined;
	const root = path.resolve(ordinary);
	return process.platform === "win32" ? root.toLowerCase() : root;
}

/** Parse only the existing coordinator fields the indicator consumes. No snapshot proves graph freshness. */
export function readStatusUpdate(params: unknown): StatusUpdate | undefined {
	if (!object(params) || params.version !== 1) return undefined;
	if (params.status === null) {
		const root = checkoutRoot(params.root);
		return root === undefined ? undefined : { root, indicator: null };
	}
	const status = params.status;
	if (!object(status)) return undefined;
	const root = checkoutRoot(status.root);
	if (root === undefined || typeof status.running !== "boolean" || !count(status.pending_triggers) ||
		(status.codebase_id !== null && typeof status.codebase_id !== "string") ||
		(status.last_error !== null && typeof status.last_error !== "string")) return undefined;
	const phase = status.first_index, progress = status.sync_progress;
	const gateError = object(phase) && object(phase.failed) && typeof phase.failed.reason === "string" ? phase.failed.reason : undefined;
	const embedding = object(phase) && object(phase.embedding) &&
		(phase.embedding.job_id === null || typeof phase.embedding.job_id === "string");
	if (phase !== null && phase !== "registering" && phase !== "syncing" && phase !== "ready" && !embedding && gateError === undefined) return undefined;
	const error = gateError ?? status.last_error;
	const active = (text: string): StatusUpdate => ({ root, indicator: {
		text: `${text}${error ? " · prior failure" : ""}`, tone: error ? "warning" : "accent", symbol: "status.running",
	} });
	if (status.running) {
		if (progress === "preparing") return active("preparing");
		if (progress === "finalizing") return active("finalizing upload");
		if (object(progress)) {
			if (object(progress.scanning) && typeof progress.scanning.root === "string") return active("scanning");
			if (object(progress.planning) && count(progress.planning.files)) return active(`planning · ${progress.planning.files} files`);
			if (object(progress.uploading) && count(progress.uploading.uploaded_files) && count(progress.uploading.total_files) &&
				progress.uploading.uploaded_files <= progress.uploading.total_files) {
				return active(`uploading ${progress.uploading.uploaded_files}/${progress.uploading.total_files}`);
			}
		}
		if (embedding) return active("embedding");
		return active("indexing");
	}
	if (error) return { root, indicator: { text: short(error), tone: "error", symbol: "status.error" } };
	if (phase === "registering" || phase === "syncing" || embedding) {
		return { root, indicator: { text: phase === "registering" ? "registering" : phase === "syncing" ? "queued" : "embedding", tone: "accent", symbol: "status.pending" } };
	}
	if (status.pending_triggers > 0) return { root, indicator: { text: "queued", tone: "accent", symbol: "status.pending" } };
	if (status.watcher === "active") return { root, indicator: { text: "watching", tone: "muted", symbol: "status.info" } };
	if (object(status.watcher) && typeof status.watcher.unavailable === "string") {
		return { root, indicator: { text: `watch unavailable · ${short(status.watcher.unavailable)}`, tone: "warning", symbol: "status.warning" } };
	}
	return { root, indicator: { text: "status unavailable", tone: "warning", symbol: "status.warning" } };
}

/** Layout status only: native tool cards own every call, result and execution spinner. */
export function installSyncStatus(pi: ExtensionAPI): void {
	const observed = new Map<string, StatusUpdate>();
	const show = (ctx: ExtensionContext) => {
		if (!ctx.hasUI) return;
		const cwd = checkoutRoot(ctx.cwd);
		let selected: StatusUpdate | undefined;
		for (const update of observed.values()) {
			if (!cwd || !update.indicator) continue;
			const relative = path.relative(update.root, cwd);
			if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) continue;
			if (!selected || update.root.length > selected.root.length) selected = update;
		}
		const indicator = selected?.indicator;
		ctx.ui.setStatus(STATUS_KEY, indicator ? ctx.ui.theme.fg(indicator.tone,
			`${ctx.ui.theme.symbol(indicator.symbol)} semctl · ${indicator.text}`) : undefined);
	};
	pi.on("mcp_notification", (event, ctx) => {
		if ((event.server !== "semctx" && event.server !== "semctx:semctx") || event.method !== METHOD) return;
		const update = readStatusUpdate(event.params);
		if (!update) {
			for (const key of observed.keys()) if (key.startsWith(`${event.server}\0`)) observed.delete(key);
			show(ctx);
			return;
		}
		const key = `${event.server}\0${update.root}`;
		if (update.indicator === null) observed.delete(key);
		else {
			observed.set(key, update);
			if (observed.size > 32) observed.delete(observed.keys().next().value!);
		}
		show(ctx);
	});
	pi.on("session_start", (_event, ctx) => show(ctx));
	pi.on("session_switch", (_event, ctx) => show(ctx));
	pi.on("session_shutdown", (_event, ctx) => {
		observed.clear();
		if (ctx.hasUI) ctx.ui.setStatus(STATUS_KEY, undefined);
	});
}
