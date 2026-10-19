import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import type { DescribeContext, NativeNode, NativeScroll, NativeUiEvent } from "@oh-my-pi/pi-tui";

const STATUS_KEY = "semctl.sync";
const METHOD = "notifications/semctl/sync_status";
type Obj = Record<string, unknown>;
const object = (value: unknown): value is Obj => value !== null && typeof value === "object" && !Array.isArray(value);
const count = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
interface Indicator {
	text: string;
	tone: "accent" | "muted" | "warning" | "error" | "success";
	symbol: "status.running" | "status.info" | "status.warning" | "status.error" | "status.pending" | "status.success";
	progress?: number;
}
export interface StatusUpdate {
	root: string;
	indicator: Indicator | null;
	summary?: string;
	details?: string;
	/** A newer delivery was observed while this older completed job was retained. */
	unresolvedJob?: string;
}

function checkoutRoot(value: unknown): string | undefined {
	if (typeof value !== "string" || value.length === 0 || value.length > 4096 || /[\u0000-\u001f]/.test(value)) return undefined;
	const ordinary = value.startsWith("\\\\?\\UNC\\") ? `\\\\${value.slice(8)}` : value.startsWith("\\\\?\\") ? value.slice(4) : value;
	if (!path.isAbsolute(ordinary)) return undefined;
	const root = path.resolve(ordinary);
	return process.platform === "win32" ? root.toLowerCase() : root;
}

/** Parse only the existing coordinator fields the indicator consumes. No snapshot proves graph freshness. */
export function readStatusUpdate(params: unknown, previous?: StatusUpdate): StatusUpdate | undefined {
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
	const pendingTriggers = status.pending_triggers;
	const phase = status.first_index, progress = status.sync_progress;
	const gateError = object(phase) && object(phase.failed) && typeof phase.failed.reason === "string" ? phase.failed.reason : undefined;
	const embeddingJob = object(phase) && object(phase.embedding) ? phase.embedding.job_id : undefined;
	const embedding = embeddingJob === null || typeof embeddingJob === "string";
	if (phase !== null && phase !== "registering" && phase !== "syncing" && phase !== "ready" && !embedding && gateError === undefined) return undefined;
	const failed = gateError !== undefined || status.last_error !== null;
	const retry = count(status.retry_after_secs) ? status.retry_after_secs : undefined;
	const evidence = status.embedding;
	const job = object(evidence) && typeof evidence.job_id === "string" && evidence.job_id.length > 0 &&
		["queued", "running", "completed", "failed", "unavailable"].includes(String(evidence.state)) &&
		[evidence.files_embedded, evidence.files_to_embed, evidence.files_failed].every(value => value === null || count(value)) &&
		(evidence.error === null || typeof evidence.error === "string") &&
		!(count(evidence.files_embedded) && count(evidence.files_to_embed) && evidence.files_embedded > evidence.files_to_embed) &&
		(typeof status.last_job_id !== "string" || status.last_job_id === evidence.job_id) ? {
			job_id: evidence.job_id, state: String(evidence.state), files_embedded: evidence.files_embedded,
			files_to_embed: evidence.files_to_embed, files_failed: evidence.files_failed, error: evidence.error,
		} : undefined;
	const jobFailed = job !== undefined && (job.state === "failed" || (count(job.files_failed) && job.files_failed > 0) ||
		(job.state !== "unavailable" && typeof job.error === "string"));
	// A full circle requires terminal completion, not just a completed file counter.
	const embedded = job !== undefined && count(job.files_embedded) && count(job.files_to_embed) && job.files_to_embed > 0
		? Math.min(0.99, job.files_embedded / job.files_to_embed) : undefined;
	const priorUnresolved = previous?.root === root ? previous.unresolvedJob : undefined;
	const delivering = status.running && (progress === "finalizing" ||
		(object(progress) && object(progress.uploading) && count(progress.uploading.total_files) && progress.uploading.total_files > 0));
	const unresolvedJob = job && job.state !== "unavailable"
		? job.state === "completed" && (delivering || priorUnresolved === job.job_id) ? job.job_id : undefined
		: priorUnresolved;
	// Retain only coordinator fields. Unknown notification fields must not become source/UI content.
	const snapshot: Obj = {
		root: status.root, codebase_id: status.codebase_id, running: status.running,
		pending_triggers: status.pending_triggers, last_error: status.last_error,
		first_index: gateError !== undefined ? { failed: { reason: gateError } } :
			embedding ? { embedding: { job_id: embeddingJob } } : phase,
	};
	if (job) snapshot.embedding = job;
	for (const key of ["leases", "last_job_id", "last_outcome", "trigger_overflow", "retry_after_secs"]) {
		const value = status[key];
		if (value === null || typeof value === "string" || typeof value === "boolean" || count(value)) snapshot[key] = value;
	}
	if (progress === null || progress === "preparing" || progress === "finalizing") snapshot.sync_progress = progress;
	else if (object(progress)) {
		if (object(progress.scanning) && typeof progress.scanning.root === "string") snapshot.sync_progress = { scanning: { root: progress.scanning.root } };
		else if (object(progress.planning) && count(progress.planning.files)) snapshot.sync_progress = { planning: { files: progress.planning.files } };
		else if (object(progress.uploading) && count(progress.uploading.uploaded_files) && count(progress.uploading.total_files) &&
			progress.uploading.uploaded_files <= progress.uploading.total_files) {
			snapshot.sync_progress = { uploading: { uploaded_files: progress.uploading.uploaded_files, total_files: progress.uploading.total_files } };
		}
	}
	if (status.watcher === "active") snapshot.watcher = status.watcher;
	else if (object(status.watcher) && typeof status.watcher.unavailable === "string") snapshot.watcher = { unavailable: status.watcher.unavailable };
	const next = status.running || job?.state === "running" || job?.state === "queued"
		? "Wait for the current attempt. This view does not request more work." :
		retry !== undefined ? `The producer reports a retry delay of ${retry}s in this snapshot. No retry was requested by this command.` :
		failed || jobFailed ? "Inspect the diagnostic. Use local Read/Grep while indexing is unavailable. This command does not retry or reindex." :
		job?.state === "unavailable" ? "The producer could not observe the current job. Inspect its diagnostic; this command does not poll or restart work." :
		status.watcher !== "active" ? "Inspect watcher availability. Check the semctl process and checkout access; this command does not restart the watcher." :
		unresolvedJob ? "A newer upload was observed after this job. Wait for its job observation; the retained completed job does not prove current embedding completion." :
		job?.state === "completed" ? "The last observed current job completed. This is not a live source-freshness guarantee." :
		"No completion observation is available. This snapshot does not prove that embedding completed.";
	const json = JSON.stringify(snapshot, null, 2);
	const update = (indicator: Indicator): StatusUpdate => {
		const watcher = snapshot.watcher;
		const summary = [
			"Latest received producer snapshot · not a freshness check",
			`State: ${snapshot.running ? "running" : indicator.text}`,
			...(snapshot.running ? [`Progress: ${indicator.text}`] : []),
			`Checkout: ${snapshot.root}`,
			`Watcher: ${watcher === "active" ? "active" : object(watcher) ? "unavailable" : "unreported"}`,
			...(job ? [`Embedding: ${job.state}${count(job.files_embedded) && count(job.files_to_embed) ? ` · ${job.files_embedded}/${job.files_to_embed} files` : ""}`] : []),
			...(typeof snapshot.last_job_id === "string" ? [`Job: ${snapshot.last_job_id}`] : []),
			...(pendingTriggers > 0 ? [`Pending triggers: ${pendingTriggers}`] : []),
			...(gateError !== undefined ? [`\nFirst-index diagnostic:\n${gateError}`] : []),
			...(typeof snapshot.last_error === "string" ? [`\nLast reconcile diagnostic:\n${snapshot.last_error}`] : []),
			...(object(watcher) ? [`\nWatcher diagnostic:\n${watcher.unavailable}`] : []),
			...(typeof job?.error === "string" ? [`\nEmbedding diagnostic:\n${job.error}`] : []),
			`\nNext: ${next}`,
		].join("\n").replace(/\r\n?/g, "\n").replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g, " ");
		return { root, indicator, unresolvedJob, summary, details: `${summary}\n\nProducer snapshot\n${json}` };
	};
	const active = (text: string, progress?: number): StatusUpdate => update({
		text, progress, tone: failed || jobFailed ? "warning" : "accent", symbol: "status.running",
	});
	// Preparing, scanning and planning describe inventory, not changed-file counts.
	// Preserve job evidence only while no newer delivery has been observed.
	const inventory = progress === "preparing" ? "preparing" :
		object(progress) && object(progress.scanning) && typeof progress.scanning.root === "string" ? "scanning" :
		object(progress) && object(progress.planning) && count(progress.planning.files) ? `planning · ${progress.planning.files} files` : undefined;
	if (status.running && inventory && !failed && !jobFailed && retry === undefined && status.pending_triggers === 0 &&
		status.trigger_overflow !== true && phase !== "registering" && phase !== "syncing" && !embedding && status.watcher === "active") {
		if (job?.state === "completed" && !unresolvedJob) return update({
			text: `${inventory} · last observed job completed`, progress: 1, tone: "success", symbol: "status.success",
		});
		if (job?.state === "running" || job?.state === "queued") return update({
			text: `${inventory} · embedding${count(job.files_embedded) && count(job.files_to_embed) ? ` ${job.files_embedded}/${job.files_to_embed}` : ""}`,
			progress: embedded, tone: "accent", symbol: job.state === "running" ? "status.running" : "status.pending",
		});
	}
	if (status.running) {
		if (progress === "preparing") return active("preparing");
		if (progress === "finalizing") return active("finalizing upload");
		if (object(progress) && object(progress.scanning) && typeof progress.scanning.root === "string") return active("scanning");
		if (object(progress) && object(progress.uploading) && count(progress.uploading.uploaded_files) &&
			count(progress.uploading.total_files) && progress.uploading.uploaded_files <= progress.uploading.total_files) {
			const total = progress.uploading.total_files;
			return active(`upload ${progress.uploading.uploaded_files}/${total}`,
				total > 0 ? Math.min(0.99, progress.uploading.uploaded_files / total) : undefined);
		}
		if (object(progress) && object(progress.planning) && count(progress.planning.files)) return active(`planning · ${progress.planning.files} files`);
		if (embedding) return active("embedding");
		return active("syncing");
	}
	if (retry !== undefined) return update({ text: "paused", tone: "warning", symbol: "status.pending" });
	if (failed || jobFailed) return update({ text: "failed", tone: "error", symbol: "status.error" });
	if (phase === "registering" || phase === "syncing" || status.pending_triggers > 0 || status.trigger_overflow === true) {
		return update({ text: phase === "registering" ? "registering" : "queued", tone: "accent", symbol: "status.pending" });
	}
	if (job?.state === "running" || job?.state === "queued") {
		const text = `embedding${count(job.files_embedded) && count(job.files_to_embed) ? ` ${job.files_embedded}/${job.files_to_embed}` : ""}`;
		return update({ text, progress: embedded, tone: "accent", symbol: job.state === "running" ? "status.running" : "status.pending" });
	}
	if (embedding) return update({ text: "embedding", tone: "accent", symbol: "status.pending" });
	if (job?.state === "unavailable") return update({ text: "embedding unavailable", tone: "warning", symbol: "status.warning" });
	if (object(status.watcher) && typeof status.watcher.unavailable === "string") {
		return update({ text: "watch unavailable", tone: "warning", symbol: "status.warning" });
	}
	if (status.watcher === "active" && job?.state === "completed" && !unresolvedJob) {
		return update({ text: "current job embedded", progress: 1, tone: "success", symbol: "status.success" });
	}
	if (status.watcher === "active") return update({ text: "completion unknown", tone: "muted", symbol: "status.info" });
	return update({ text: "status unavailable", tone: "warning", symbol: "status.warning" });
}

/** Layout status only: native tool cards own every call, result and execution spinner. */
export function installSyncStatus(pi: ExtensionAPI, openGraph?: (ctx: ExtensionContext, mode: "overview" | "live") => Promise<void>): void {
	const observed = new Map<string, StatusUpdate>();
	let activationContext: ExtensionContext | undefined;
	const onActivate = async () => {
		if (activationContext) await statusCommand.handler("status", activationContext);
	};
	const missingSnapshot = (root: string): StatusUpdate => ({
		root, indicator: { text: "snapshot unavailable", tone: "muted", symbol: "status.info" },
		summary: "No current producer snapshot is available for this checkout. Earlier completion and progress observations were cleared.",
		details: "No current producer snapshot is available for this checkout. Earlier completion and progress observations were cleared.\n\nNext: Check that the semctx MCP server is attached to this checkout. This view does not request status, indexing or retry.",
	});
	const selectedStatus = (ctx: ExtensionContext): StatusUpdate | undefined => {
		const cwd = checkoutRoot(ctx.cwd);
		let selected: StatusUpdate | undefined;
		for (const update of observed.values()) {
			if (!cwd || !update.indicator) continue;
			const relative = path.relative(update.root, cwd);
			if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) continue;
			if (!selected || update.root.length > selected.root.length) selected = update;
		}
		return selected;
	};
	const show = (ctx: ExtensionContext) => {
		if (!ctx.hasUI || typeof ctx.ui.setStatus !== "function") return;
		const indicator = selectedStatus(ctx)?.indicator;
		activationContext = ctx;
		// Native hosts render exact progress. Text-only terminals keep one compact glyph.
		const mark = indicator?.progress !== undefined && indicator.tone !== "warning" && indicator.tone !== "error"
			? ["○", "◔", "◑", "◕", "●"][indicator.progress === 1 ? 4 : Math.floor(indicator.progress * 4)]
			: indicator ? ctx.ui.theme.symbol(indicator.symbol) : "";
		ctx.ui.setStatus(STATUS_KEY, indicator ? ctx.ui.theme.fg(indicator.tone, `${mark} semctl`) : undefined,
			indicator ? {
				label: "semctl",
				progress: indicator.progress,
				progressMotion: "brief",
				icon: { "status.info": "info", "status.warning": "warn", "status.error": "x-circle",
					"status.pending": "clock", "status.success": "check", "status.running": "sync" }[indicator.symbol],
				tone: indicator.tone,
				title: `${indicator.text}\nOpen status and project overview`,
				onActivate,
			} : undefined);
	};
	const statusCommand = {
		description: "Inspect semctl status, diagnostics, and project relationships",
		getArgumentCompletions: (prefix: string) => ["status", "details", ...(openGraph ? ["graph", "graph live"] : [])].filter(value => value.startsWith(prefix)).map(value => ({ value, label: value })),
		handler: async (args: string, ctx: ExtensionContext) => {
			if (!ctx.hasUI) return;
			const command = args.trim();
			if ((command === "graph" || command === "graph live") && openGraph) { await openGraph(ctx, command === "graph live" ? "live" : "overview"); return; }
			if (command && command !== "status" && command !== "details") {
				if (typeof ctx.ui.notify === "function") ctx.ui.notify(`Usage: /semctl [status|details${openGraph ? "|graph [live]" : ""}]`, "info");
				return;
			}
			const selected = selectedStatus(ctx);
			const details = (command === "details" ? selected?.details : selected?.summary) ??
				"No producer snapshot is available for this checkout.\n\nNext: Check that the semctx MCP server is attached to this checkout. This command does not request status, source, indexing or retry.";
			if (typeof ctx.ui.custom !== "function" || typeof pi.pi?.Text !== "function") {
				if (typeof ctx.ui.notify === "function") ctx.ui.notify(details, "info");
				return;
			}
			// Fullscreen places the native body in the host's main scroller, not its non-scrolling editor dock.
			const next = await ctx.ui.custom<"graph" | "live" | undefined>((tui, _theme, keys, done) => {
				const body = new pi.pi.Text(details, 0, 0);
				const heading = new pi.pi.Text(command === "details" ? "semctl details" : "semctl status", 0, 0);
				const hint = new pi.pi.Text(`↑/↓ scroll · PgUp/PgDn page · Enter/Esc close${openGraph ? " · G graph" : ""}`, 0, 0);
				let offset = 0, scroll: NativeScroll = { by: "start", n: 0 };
				const height = () => Math.max(1, Math.min(24, tui.terminal.rows - 6));
				return {
					invalidate() { body.invalidate(); heading.invalidate(); hint.invalidate(); },
					render(width: number) {
						const rows = body.render(width), size = height();
						offset = Math.max(0, Math.min(offset, rows.length - size));
						return [...heading.render(width), ...rows.slice(offset, offset + size), ...hint.render(width)];
					},
					describe(_cx: DescribeContext): NativeNode {
						return { k: "col", p: { gap: "sm" }, c: [
							{ k: "row", p: { gap: "sm" }, c: [
								{ k: "text", p: { spans: [{ t: command === "details" ? "semctl details" : "semctl status", s: "strong" }] } },
								{ k: "spacer", p: { grow: 1 } },
								...(openGraph ? [
									{ k: "text" as const, key: "graph", p: { spans: [{ t: "Project graph", s: "accent" }],
										actions: { click: "graph" }, aria: "Open semctl project and relationship overview" } },
									{ k: "text" as const, key: "live", p: { spans: [{ t: "Live popup", s: "accent" }],
										actions: { click: "live" }, aria: "Toggle the native live graph popup" } },
								] : []),
								{ k: "text", key: "close", p: { spans: [{ t: "Close", s: "accent" }],
									actions: { click: "close" }, aria: "Close semctl status" } },
							] },
							{ k: "col", key: "snapshot", scroll, c: [body] },
							hint,
						] };
					},
					handleNativeEvent(event: NativeUiEvent) {
						if (event.type !== "action") return;
						if (event.act === "close") done(undefined);
						else if (event.act === "graph" && openGraph) done("graph");
						else if (event.act === "live" && openGraph) done("live");
					},
					handleInput(data: string) {
						if (keys.matches(data, "tui.select.cancel") || keys.matches(data, "tui.select.confirm")) { done(undefined); return; }
						if (openGraph && data.toLowerCase() === "g") { done("graph"); return; }
						let by: NativeScroll["by"], delta: number;
						if (keys.matches(data, "tui.select.up")) { by = "line-up"; delta = -1; }
						else if (keys.matches(data, "tui.select.down")) { by = "line-down"; delta = 1; }
						else if (keys.matches(data, "tui.select.pageUp")) { by = "page-up"; delta = -height(); }
						else if (keys.matches(data, "tui.select.pageDown")) { by = "page-down"; delta = height(); }
						else return;
						offset += delta;
						scroll = { by, n: scroll.n + 1 };
						tui.requestRender();
					},
				};
			}, { overlay: true, overlayOptions: { fullscreen: true } });
			if (next === "graph") await openGraph?.(ctx, "overview");
			else if (next === "live") await openGraph?.(ctx, "live");
		},
	};
	if (typeof pi.registerCommand === "function") pi.registerCommand("semctl", statusCommand);
	pi.on("mcp_notification", (event, ctx) => {
		if ((event.server !== "semctx" && event.server !== "semctx:semctx") || event.method !== METHOD) return;
		const params = event.params;
		if (object(params) && params.version === 1 && params.status === null && params.root === undefined) {
			for (const [key, old] of observed) if (key.startsWith(`${event.server}\0`)) observed.set(key, missingSnapshot(old.root));
			show(ctx);
			return;
		}
		const root = object(params) ? checkoutRoot(object(params.status) ? params.status.root : params.root) : undefined;
		const update = readStatusUpdate(params, root === undefined ? undefined : observed.get(`${event.server}\0${root}`));
		if (!update) {
			// A malformed observation without a trustworthy root cannot invalidate another checkout.
			if (root !== undefined && observed.has(`${event.server}\0${root}`)) observed.set(`${event.server}\0${root}`, missingSnapshot(root));
			show(ctx);
			return;
		}
		const key = `${event.server}\0${update.root}`;
		if (update.indicator === null) {
			if (observed.has(key)) observed.set(key, missingSnapshot(update.root));
		}
		else {
			observed.set(key, update);
			if (observed.size > 32) {
				const owner = selectedStatus(ctx);
				for (const [oldKey, oldUpdate] of observed) {
					if (oldUpdate === owner) continue;
					observed.delete(oldKey);
					break;
				}
			}
		}
		show(ctx);
	});
	pi.on("session_start", (_event, ctx) => show(ctx));
	pi.on("session_switch", (_event, ctx) => show(ctx));
	pi.on("session_shutdown", (_event, ctx) => {
		observed.clear();
		activationContext = undefined;
		if (ctx.hasUI && typeof ctx.ui.setStatus === "function") ctx.ui.setStatus(STATUS_KEY, undefined);
	});
}
