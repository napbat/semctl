import { lstat, readFile, realpath } from "node:fs/promises";
import * as path from "node:path";
import type { Display } from "./inspector";
import { DOC_SOURCE_LIMITS, isDocPath, type DocSource } from "./graph-data/doc-concepts.js";
import { normalizeConceptPath } from "./graph-data/palette.js";

const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
const decoder = new TextDecoder();

function inside(root: string, file: string): boolean {
	const relative = path.relative(root, file);
	return relative !== "" && relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

/** Read the catalog Markdown files of a local checkout overview in path order, within `DOC_SOURCE_LIMITS`. */
export async function readDocSources(display: Display, signal: AbortSignal): Promise<DocSource[]> {
	if (display.kind !== "graph" || display.identity.copy !== "checkout" || !display.identity.localRoot) return [];
	const selected = [...new Set(display.nodes.flatMap(node => {
		const relative = node.category === "file" ? normalizeConceptPath(node.path) : undefined;
		return relative && isDocPath(relative) ? [relative] : [];
	}))].sort(compare).slice(0, DOC_SOURCE_LIMITS.files);
	if (!selected.length) return [];
	signal.throwIfAborted();
	const root = await realpath(path.resolve(display.identity.localRoot));
	const docs: DocSource[] = [];
	let total = 0;
	for (const relative of selected) {
		signal.throwIfAborted();
		const file = path.resolve(root, relative);
		if (!inside(root, file)) continue;
		try {
			const stats = await lstat(file);
			if (!stats.isFile() || stats.size > DOC_SOURCE_LIMITS.fileBytes) continue;
			if (total + stats.size > DOC_SOURCE_LIMITS.totalBytes) break;
			// A linked parent directory can lead out of the checkout.
			if (!inside(root, await realpath(file))) continue;
			const bytes = await readFile(file, { signal });
			// The file can change after `lstat`.
			if (bytes.byteLength > DOC_SOURCE_LIMITS.fileBytes) continue;
			if (total + bytes.byteLength > DOC_SOURCE_LIMITS.totalBytes) break;
			total += bytes.byteLength;
			docs.push({ path: relative, text: decoder.decode(bytes) });
		} catch {
			signal.throwIfAborted();
			// A missing or unreadable doc removes only concept evidence. The overview stays complete without it.
		}
	}
	return docs;
}
