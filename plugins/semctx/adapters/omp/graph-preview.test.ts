import { afterEach, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { graphPreview } from "./graph-preview";
import type { GraphData } from "./graph";
import { lookupConcept, resetConcepts, setConceptSnapshot } from "./concept-colors";
import { conceptClass } from "./native-concepts";
import { nativeGraphLayout } from "./graph-native-layout";
import { readDisplay, type Hit } from "./inspector";
import { quantizedSegment } from "./graph-native-styles";

afterEach(resetConcepts);
type NativeElement = Extract<NativeNode, { k: "el" }>;
const cx: DescribeContext = { cols: 80, dark: true, reduceMotion: false, stylesheets: true, supports: kind => kind === "el", feature: name => name === "styles" };
function graph(): GraphData {
	return { kind: "graph", title: "Graph", query: null, roots: ["a"], caveats: [], truncated: false,
		nodes: [{ id: "a", label: "A", detail: "Entry module", hit: null, category: "file", path: "src/a.ts" },
			{ id: "b", label: "B", detail: null, hit: null, category: "file", path: "src/b.ts" }],
		edges: [{ from: "a", to: "b", label: "calls" }] };
}
function nodes(root: NativeNode | undefined): NativeNode[] {
	if (!root) throw new Error("No native graph preview");
	const result = [root];
	for (const child of root.c ?? []) if (child && typeof child === "object" && "k" in child) result.push(...nodes(child as NativeNode));
	return result;
}
function targets(root: NativeNode | undefined): NativeElement[] {
	return nodes(root).filter((node): node is NativeElement => node.k === "el" && node.p?.attrs?.["data-node"] !== undefined);
}
function links(root: NativeNode | undefined): NativeElement[] {
	return nodes(root).filter((node): node is NativeElement => node.k === "el" && node.p?.attrs?.["data-count"] !== undefined);
}

test("native capabilities are required independently, without image fallback", () => {
	const data = graph();
	expect(graphPreview(data, { ...cx, supports: () => false })).toBeUndefined();
	expect(graphPreview(data, { ...cx, feature: () => false })).toBeUndefined();
	expect(graphPreview(data, { ...cx, stylesheets: undefined })).toBeUndefined();
	expect(graphPreview(data, cx)).toBeDefined();
});

test("untrusted names stay plain text and never become resources, markup, attributes or actions", () => {
	const data = graph(), malicious = '<script src="https://bad.invalid/x">&<image href="x">';
	data.nodes[0].label = malicious;
	data.nodes[0].detail = '<style>.bad{background:url(https://bad.invalid)}</style>';
	data.nodes[0].category = undefined;
	data.nodes[0].path = undefined;
	data.nodes[0].id = 'a" onclick="bad()'; data.roots = [data.nodes[0].id]; data.edges[0].from = data.nodes[0].id;
	const preview = graphPreview(data, cx, { nodeAction: id => id === "b" ? "inspect-b" : undefined });
	const all = nodes(preview);
	for (const node of all) {
		expect(node.k).toBe("el");
		if (node.k !== "el") continue;
		expect(["div", "span", "section", "button"]).toContain(node.p?.tag ?? "");
		for (const attribute of Object.keys(node.p?.attrs ?? {})) expect(/^(data-|aria-|role$)/.test(attribute)).toBe(true);
		expect(node.p).not.toHaveProperty("href");
		expect(node.p).not.toHaveProperty("src");
		expect(node.p).not.toHaveProperty("style");
	}
	const untrusted = targets(preview).find(node => node.p?.attrs?.["data-node"] === data.nodes[0].id)!;
	expect(untrusted.p?.title).toContain(malicious);
	expect(untrusted.p?.actions).toBeUndefined();
	expect(targets(preview).find(node => node.p?.attrs?.["data-node"] === "b")?.p?.actions).toEqual({ click: "inspect-b" });
	for (const sheet of preview?.styles ?? []) expect(sheet.css).not.toContain(malicious);
});


test("hover evidence preserves real direction, type, multiplicity, references and self links", () => {
	const data = graph();
	data.edges = [
		{ from: "a", to: "b", label: "imports" }, { from: "a", to: "b", label: "imports" },
		{ from: "b", to: "a", label: "calls", kind: "reference", symbol: "render", moniker: "local:render" },
		{ from: "a", to: "a", label: "within project root" },
		{ from: "a", to: "b", label: "contains" },
	];
	const preview = graphPreview(data, cx, { selected: "a" }), routes = links(preview);
	expect(routes).toHaveLength(4);
	const imports = routes.find(node => node.p?.attrs?.["data-kind"] === "import");
	expect(imports?.p?.attrs).toMatchObject({ "data-from": "a", "data-to": "b", "data-count": 2 });
	const reference = routes.find(node => node.p?.attrs?.["data-kind"] === "reference");
	expect(reference?.p?.attrs).toMatchObject({ "data-from": "b", "data-to": "a" });
	expect(reference?.p?.title).toContain(data.edges[2].moniker!);
	expect(routes.find(node => node.p?.attrs?.["data-kind"] === "association")?.p?.attrs).toMatchObject({ "data-from": "a", "data-to": "a", "data-count": 1 });
});

test("concept updates refresh native classes and respect a hit's codebase before current context", () => {
	const data = graph(), before = targets(graphPreview(data, cx, { codebaseId: "local" }))[0].p?.class;
	const overview = readDisplay({ ...graph(), version: 1,
		edges: [{ from: "a", to: "b", label: "references", kind: "reference", symbol: "render", moniker: "local:render" }],
		identity: { codebaseId: "local", name: "Local", localRoot: "D:/local", copy: "checkout" } }, "overview", true);
	if (!overview) throw new Error("Invalid local overview fixture");
	setConceptSnapshot(overview);
	const concept = lookupConcept("local", "src/a.ts");
	if (!concept) throw new Error("No concept assignment");
	expect(targets(graphPreview(data, cx, { codebaseId: "local" }))[0].p?.class).toContain(conceptClass(concept, true));
	expect(targets(graphPreview(data, { ...cx, dark: false }, { codebaseId: "local" }))[0].p?.class).toContain(conceptClass(concept, false));
	const foreign = graph(); foreign.nodes[0].hit = { codebaseId: "foreign", path: "src/a.ts" } as Hit;
	expect(targets(graphPreview(foreign, cx, { codebaseId: "local" }))[0].p?.class).toBe(before);
	resetConcepts();
	expect(targets(graphPreview(data, cx, { codebaseId: "local" }))[0].p?.class).toBe(before);
});

test("activity and focus preserve keyed dots and settled geometry, including motion preference", () => {
	const data = graph(), layout = nativeGraphLayout(data, "inline"), idle = graphPreview(data, cx);
	const active = graphPreview(data, cx, { activeIds: new Set(["a"]), selected: "b" });
	expect(nativeGraphLayout(data, "inline")).toBe(layout);
	expect(nodes(active).map(node => node.key).filter(key => key?.startsWith("node:"))).toEqual(nodes(idle).map(node => node.key).filter(key => key?.startsWith("node:")));
	expect(targets(active).map(dot => dot.p?.attrs?.["data-node"])).toEqual(targets(idle).map(dot => dot.p?.attrs?.["data-node"]));
	expect(targets(active)[0].p?.aria).toContain("active");
	expect(targets(active)[1].p?.attrs?.["aria-pressed"]).toBe(true);
	expect(graphPreview(data, { ...cx, reduceMotion: true }, { activeIds: new Set(["a"]) })?.p?.attrs?.["data-motion"]).toBe("reduced");
	expect(idle?.p?.attrs?.["data-motion"]).toBe("full");
	expect(targets(graphPreview(data, cx))[0].p?.aria).not.toContain("active");
});

test("completion feedback keeps concurrent activity and selection distinct and expires cleanly", () => {
	const data = graph(), activeIds = new Set(["a"]);
	const observed = graphPreview(data, cx, { activeIds, selected: "b", feedback: new Map([
		["a", { id: 1, outcome: "returned" as const }],
		["b", { id: 2, outcome: "failed" as const }],
		["foreign", { id: 3, outcome: "returned" as const }],
	]) });
	const observedTargets = targets(observed);
	expect(observedTargets.map(node => node.p?.attrs?.["data-node"])).toEqual(["a", "b"]);
	expect(observedTargets[0].p?.aria).toContain("active");
	expect(observedTargets[0].p?.aria).toContain("returned");
	expect(observedTargets[0].p?.aria).not.toContain("failed");
	expect(observedTargets[1].p?.aria).toContain("failed");
	expect(observedTargets[1].p?.attrs?.["aria-pressed"]).toBe(true);
	const settled = targets(graphPreview(data, cx, { activeIds, selected: "b" }));
	expect(settled[0].p?.aria).toContain("active");
	for (const target of settled) {
		expect(target.p?.aria).not.toContain("returned");
		expect(target.p?.aria).not.toContain("failed");
	}
	expect(settled[1].p?.attrs?.["aria-pressed"]).toBe(true);
});

test("retained graphs share bounded sheets independent of names, activity and history", () => {
	const sheets = new Map<string, string>();
	for (let version = 0; version < 12; version++) {
		const data = graph();
		data.nodes = Array.from({ length: 64 }, (_, index) => ({ id: String(index), label: `<untrusted-${version}>`.repeat(1000), detail: null, hit: null }));
		data.roots = ["0"];
		data.edges = Array.from({ length: 128 }, (_, index) => ({ from: String(index % 64), to: String((index + 1) % 64), label: `edge-${version}`, kind: index % 2 ? "association" : "import" }));
		const preview = graphPreview(data, cx, { selected: String(version), activeIds: new Set([String(version)]), size: "compact" });
		expect(targets(preview)).toHaveLength(64);
		for (const sheet of preview?.styles ?? []) {
			if (sheets.has(sheet.name)) expect(sheet.css).toBe(sheets.get(sheet.name)!);
			sheets.set(sheet.name, sheet.css);
			expect(sheet.css).not.toContain(`<untrusted-${version}>`);
		}
		data.nodes.push({ id: "64", label: "Overflow", detail: null, hit: null });
		expect(graphPreview(data, cx)).toBeUndefined();
	}
	expect([...sheets.values()].reduce((bytes, css) => bytes + new TextEncoder().encode(css).byteLength, 0)).toBeLessThan(256 * 1024);
	const tooManyEdges = graph(); tooManyEdges.edges = Array.from({ length: 129 }, () => ({ from: "a", to: "b", label: "calls" }));
	expect(graphPreview(tooManyEdges, cx)).toBeUndefined();
	const empty = graph(); empty.nodes = []; empty.edges = []; empty.roots = [];
	expect(graphPreview(empty, cx)).toBeUndefined();
});

test("native curve subdivision retains separate reverse routes, self-loop endpoints and bounded segments", () => {
	const data = graph();
	data.edges.push({ from: "b", to: "a", label: "imports" }, { from: "a", to: "a", label: "calls" });
	const layout = nativeGraphLayout(data, "overview");
	expect(layout.routes).toHaveLength(3);
	for (const route of layout.routes) {
		expect(route.points.length).toBeGreaterThan(1);
		expect(route.points.length).toBeLessThanOrEqual(257);
		for (let index = 1; index < route.points.length; index++) {
			const from = route.points[index - 1], to = route.points[index];
			expect(Math.hypot(to[0] - from[0], to[1] - from[1])).toBeLessThanOrEqual(48.01);
			const segment = quantizedSegment(from, to, layout.width, layout.height);
			const x = segment.x / 100 * layout.width, y = segment.y / 100 * layout.height;
			const length = segment.length / 100 * layout.width, angle = segment.angle / 180 * Math.PI;
			expect(Math.hypot(x - from[0], y - from[1])).toBeLessThan(3.5);
			expect(Math.hypot(x + Math.cos(angle) * length - to[0], y + Math.sin(angle) * length - to[1])).toBeLessThan(3.5);
		}
		const source = layout.dots.find(dot => dot.node.id === route.link.from)!, target = layout.dots.find(dot => dot.node.id === route.link.to)!;
		expect(Math.hypot(route.points[0][0] - source.x, route.points[0][1] - source.y)).toBeLessThan(14);
		const end = route.points.at(-1)!;
		expect(Math.hypot(end[0] - target.x, end[1] - target.y)).toBeLessThan(14);
	}
	expect(layout.routes[0].points).not.toEqual(layout.routes[2].points);
});
