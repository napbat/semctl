import { randomUUID } from "node:crypto";
import { mkdir, open, rename, unlink } from "node:fs/promises";
import { dirname } from "node:path";

export interface GraphAppearance {
	layout: "map" | "force" | "radial" | "layers" | "components";
	color: "concepts" | "structure" | "neutral";
	detail: "calm" | "balanced" | "detailed";
	labels: "key" | "more" | "hover";
	links: "soft" | "clear";
	motion: "system" | "off";
	popup: "small" | "medium" | "large";
}

export const DEFAULT_GRAPH_APPEARANCE: Readonly<GraphAppearance> = Object.freeze({
	layout: "map", color: "concepts", detail: "balanced", labels: "key", links: "soft", motion: "system", popup: "small",
});

export interface GraphAppearanceState {
	readonly value: Readonly<GraphAppearance>;
	readonly ready: boolean;
	readonly saving: boolean;
	readonly error?: string;
}

/** Injectable file boundary; writeAtomic must replace only this file, never expose partial JSON. */
export interface GraphAppearancePersistence {
	read(file: string): Promise<string>;
	writeAtomic(file: string, contents: string): Promise<void>;
}

const MAX_BYTES = 8192;
const VERSION = 2;
const choices: { readonly [K in keyof GraphAppearance]: readonly GraphAppearance[K][] } = {
	layout: ["map", "force", "radial", "layers", "components"], color: ["concepts", "structure", "neutral"],
	detail: ["calm", "balanced", "detailed"], labels: ["key", "more", "hover"],
	links: ["soft", "clear"], motion: ["system", "off"], popup: ["small", "medium", "large"],
};
const keys = Object.keys(choices) as (keyof GraphAppearance)[];
/** Version 1 files predate the popup size; they hold the other six keys. */
const VERSION_1_KEYS = keys.filter(key => key !== "popup");
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);

/** Without `required`, any subset of supported keys is valid. With it, the value holds exactly those keys. */
function validPatch(value: unknown, required?: readonly (keyof GraphAppearance)[]): value is Partial<GraphAppearance> {
	if (!record(value)) return false;
	const fields = Reflect.ownKeys(value), allowed: readonly string[] = required ?? keys;
	return (!required || fields.length === required.length) && fields.every(key => typeof key === "string" &&
		allowed.includes(key) && Object.hasOwn(Object.getOwnPropertyDescriptor(value, key)!, "value") &&
		(choices[key as keyof GraphAppearance] as readonly unknown[]).includes(value[key]));
}

function decode(contents: string): Readonly<GraphAppearance> {
	if (Buffer.byteLength(contents, "utf8") > MAX_BYTES) throw new Error("Appearance file exceeds the 8 KiB preference limit.");
	let value: unknown;
	try { value = JSON.parse(contents); } catch { throw new Error("Appearance file is not valid JSON."); }
	const version = record(value) ? value.version : undefined;
	const required = version === 1 ? VERSION_1_KEYS : version === VERSION ? keys : undefined;
	if (!record(value) || !required || Object.keys(value).length !== 2 || !Object.hasOwn(value, "appearance") || !validPatch(value.appearance, required)) {
		throw new Error("Appearance file has an unsupported version or invalid preferences.");
	}
	const saved = value.appearance as Partial<GraphAppearance>;
	// A key that the file version does not store keeps its default.
	return Object.freeze(Object.fromEntries(keys.map(key => [key, saved[key] ?? DEFAULT_GRAPH_APPEARANCE[key]])) as unknown as GraphAppearance);
}

const filesystem: GraphAppearancePersistence = {
	async read(file) {
		const handle = await open(file, "r");
		try {
			const bytes = Buffer.alloc(MAX_BYTES + 1);
			let length = 0;
			while (length < bytes.length) {
				const { bytesRead } = await handle.read(bytes, length, bytes.length - length, length);
				if (bytesRead === 0) break;
				length += bytesRead;
			}
			if (length > MAX_BYTES) throw new Error("Appearance file exceeds the 8 KiB preference limit.");
			return bytes.toString("utf8", 0, length);
		} finally { await handle.close(); }
	},
	async writeAtomic(file, contents) {
		await mkdir(dirname(file), { recursive: true });
		const temporary = `${file}.${process.pid}.${randomUUID()}.tmp`;
		try {
			const handle = await open(temporary, "wx", 0o600);
			try { await handle.writeFile(contents, "utf8"); await handle.sync(); }
			finally { await handle.close(); }
			await rename(temporary, file);
		} finally {
			// A failed replacement leaves the previous file intact; remove only our unique temporary.
			await unlink(temporary).catch(() => {});
		}
	},
};

function message(error: unknown): string {
	return (error instanceof Error ? error.message : String(error)).replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").slice(0, 1024);
}

/** Local, versioned preferences. IO failures never reject UI actions or discard in-memory edits. */
export class GraphAppearanceStore {
	private snapshot: GraphAppearanceState;
	private readonly listeners = new Set<() => void>();
	private loading?: Promise<void>;
	private flushing?: Promise<void>;
	private attempted: boolean;
	private loadFailed = false;
	private filePresent = false;
	private revision = 0;
	private savedRevision = 0;
	private beforeLoad: Partial<GraphAppearance> = {};

	constructor(private readonly file?: string, private readonly persistence: GraphAppearancePersistence = filesystem) {
		this.attempted = !file;
		this.snapshot = { value: DEFAULT_GRAPH_APPEARANCE, ready: !file, saving: false };
	}
	get state(): GraphAppearanceState { return this.snapshot; }
	get persistent(): boolean { return Boolean(this.file); }
	get saved(): boolean { return this.filePresent && this.savedRevision === this.revision; }

	subscribe(listener: () => void): () => void {
		this.listeners.add(listener);
		return () => { this.listeners.delete(listener); };
	}
	private publish(patch: Partial<GraphAppearanceState>): void {
		this.snapshot = { ...this.snapshot, ...patch };
		for (const listener of this.listeners) {
			// A consumer's render failure must not interrupt the persistence queue.
			try { listener(); } catch {}
		}
	}

	load(): Promise<void> {
		if (this.loading) return this.loading;
		if (!this.file || (this.attempted && (!this.loadFailed || this.revision > this.savedRevision))) return Promise.resolve();
		this.loading = Promise.resolve().then(async () => {
			try {
				let value = DEFAULT_GRAPH_APPEARANCE;
				try { value = decode(await this.persistence.read(this.file!)); this.filePresent = true; }
				catch (error) {
					if (!(record(error) && error.code === "ENOENT")) throw error;
					this.filePresent = false;
				}
				this.loadFailed = false;
				this.publish({ value: Object.freeze({ ...value, ...this.beforeLoad }), error: undefined });
			} catch (error) {
				this.loadFailed = true;
				this.publish({ error: `Could not load appearance: ${message(error)} Existing file kept; changes remain usable in memory.` });
			} finally {
				this.attempted = true;
				this.beforeLoad = {};
				this.loading = undefined;
				this.publish({ ready: true });
			}
		});
		return this.loading;
	}

	update(patch: Partial<GraphAppearance>): Promise<void> {
		try {
			if (!validPatch(patch)) throw new Error("Expected only supported appearance options.");
		} catch (error) {
			this.publish({ error: `Appearance change rejected: ${message(error)}` });
			return Promise.resolve();
		}
		if (Object.keys(patch).length === 0) return this.retry();
		this.revision++;
		if (!this.attempted || this.loading) this.beforeLoad = { ...this.beforeLoad, ...patch };
		this.publish({ value: Object.freeze({ ...this.state.value, ...patch }) });
		return this.save();
	}
	reset(): Promise<void> { return this.update({ ...DEFAULT_GRAPH_APPEARANCE }); }

	/** Retry a failed save; without explicit edits, retry loading without replacing an unreadable file. */
	retry(): Promise<void> {
		return this.revision > this.savedRevision ? this.save() : this.loadFailed ? this.load() : Promise.resolve();
	}
	/** Wait for already-requested IO only; shutdown never creates or repairs an untouched file. */
	flush(): Promise<void> { return this.flushing ?? this.loading ?? Promise.resolve(); }
	private save(): Promise<void> {
		if (!this.file) {
			this.savedRevision = this.revision;
			this.publish({ ready: true, saving: false, error: undefined });
			return Promise.resolve();
		}
		if (this.flushing) return this.flushing;
		this.flushing = Promise.resolve().then(async () => {
			try {
				await this.load();
				while (this.savedRevision < this.revision) {
					const revision = this.revision;
					const contents = `${JSON.stringify({ version: VERSION, appearance: this.state.value }, null, 2)}\n`;
					try {
						await this.persistence.writeAtomic(this.file!, contents);
						this.savedRevision = revision;
						this.loadFailed = false;
						this.filePresent = true;
						this.publish({ error: undefined });
					} catch (error) {
						this.publish({ error: `Could not save appearance: ${message(error)} Changes are active in memory; retry to save.` });
						// An edit received during this write deserves its own attempt, not a lost queue slot.
						if (this.revision === revision) break;
					}
				}
			} finally {
				this.flushing = undefined;
				this.publish({ saving: false });
			}
		});
		this.publish({ saving: true });
		return this.flushing;
	}
}
