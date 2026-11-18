import { afterAll, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { readDocSources } from "./doc-sources";
import { DOC_SOURCE_LIMITS } from "./graph-data/doc-concepts.js";
import type { Display } from "./inspector";

type Overview = Extract<Display, { kind: "graph" }>;
const roots: string[] = [];
async function directory(): Promise<string> {
	const root = await mkdtemp(path.join(tmpdir(), "semctl-doc-sources-"));
	roots.push(root);
	return root;
}
async function files(root: string, entries: Record<string, string | Buffer>): Promise<void> {
	for (const [file, text] of Object.entries(entries)) {
		await mkdir(path.dirname(path.join(root, file)), { recursive: true });
		await writeFile(path.join(root, file), text);
	}
}
function overview(localRoot: string | null, paths: readonly string[], copy: "checkout" | "canonical" = "checkout"): Overview {
	return { version: 1, kind: "graph", identity: { codebaseId: "docs", name: null, localRoot, copy }, title: "Project overview", query: null,
		nodes: paths.map((file, index) => ({ id: String(index), label: file, detail: null, hit: null, category: "file", path: file })),
		edges: [], roots: [], caveats: [], truncated: true };
}
const read = (display: Display) => readDocSources(display, new AbortController().signal);

describe("checkout doc sources", () => {
	afterAll(async () => { for (const root of roots) await rm(root, { recursive: true, force: true }); });
	test("reads catalog Markdown files in path order and skips other, missing, escaping and linked files", async () => {
		const root = await directory(), outside = await directory();
		await files(root, { "docs/b.md": "# B\n", "docs/a.MD": "\uFEFF# A\n", "README.markdown": "# Read me\n", "src/code.ts": "export {};\n" });
		await files(outside, { "secret.md": "# Secret\n" });
		await symlink(outside, path.join(root, "linked"), "junction");
		// A file link needs extra rights on some Windows hosts; a missing link is still skipped.
		await symlink(path.join(outside, "secret.md"), path.join(root, "docs", "link.md")).catch(() => undefined);
		const display = overview(root, ["docs/b.md", "src/code.ts", "docs/missing.md", "linked/secret.md", "docs/link.md", "README.markdown", "docs/a.MD"]);
		expect(await read(display)).toEqual([
			{ path: "README.markdown", text: "# Read me\n" }, { path: "docs/a.MD", text: "# A\n" }, { path: "docs/b.md", text: "# B\n" },
		]);
		expect(await read({ ...display, nodes: display.nodes.map(node => ({ ...node, category: "directory" as const })) })).toEqual([]);
		expect(await read(overview(root, ["docs/b.md"], "canonical"))).toEqual([]);
		expect(await read(overview(null, ["docs/b.md"]))).toEqual([]);
	});
	test("keeps the file count, file size and total size limits", async () => {
		const root = await directory(), count = DOC_SOURCE_LIMITS.files + 1;
		const small = Object.fromEntries(Array.from({ length: count }, (_, index) => [`notes/${String(index).padStart(4, "0")}.md`, `# ${index}\n`]));
		await files(root, small);
		const listed = await read(overview(root, Object.keys(small)));
		expect(listed.map(doc => doc.path)).toEqual(Object.keys(small).sort().slice(0, DOC_SOURCE_LIMITS.files));
		const full = Buffer.alloc(DOC_SOURCE_LIMITS.fileBytes, 0x61), fits = DOC_SOURCE_LIMITS.totalBytes / DOC_SOURCE_LIMITS.fileBytes;
		const large = Object.fromEntries(Array.from({ length: fits + 1 }, (_, index) => [`large/${String(index).padStart(2, "0")}.md`, full]));
		await files(root, { ...large, "large/0-oversized.md": Buffer.alloc(DOC_SOURCE_LIMITS.fileBytes + 1, 0x61) });
		const bounded = await read(overview(root, ["large/0-oversized.md", ...Object.keys(large)]));
		expect(bounded.map(doc => doc.path)).toEqual(Object.keys(large).sort().slice(0, fits));
		expect(bounded.reduce((sum, doc) => sum + doc.text.length, 0)).toBe(DOC_SOURCE_LIMITS.totalBytes);
	});
	test("an aborted read rejects", async () => {
		const root = await directory();
		await files(root, { "docs/a.md": "# A\n" });
		const controller = new AbortController();
		controller.abort();
		await expect(readDocSources(overview(root, ["docs/a.md"]), controller.signal)).rejects.toThrow();
		expect(await readDocSources(overview(root, []), controller.signal)).toEqual([]);
	});
});
