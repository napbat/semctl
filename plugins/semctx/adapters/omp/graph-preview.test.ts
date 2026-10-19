import { afterEach, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { graphPreview } from "./graph-preview";
import type { GraphData } from "./graph";
import { conceptRevision, lookupConcept, resetConcepts, setConceptSnapshot } from "./concept-colors";
import { conceptClass } from "./native-concepts";
import { nativeGraphLayout } from "./graph-native-layout";
import { readDisplay, type Hit } from "./inspector";
import { quantizedSegment, routeStrokeClasses } from "./graph-native-styles";
import { asNativeElement, type NativeElement } from "./native-elements";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { projectSummary } from "./graph-projection";
import { graphColorRevision, lookupGraphColor, setGraphColorMode } from "./graph-colors";

afterEach(() => { resetConcepts(); setGraphColorMode("concepts"); });
const cx: DescribeContext = { cols: 80, dark: true, reduceMotion: false, supports: kind => String(kind) === "el", feature: name => name === "styles" };
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
	return nodes(root).map(asNativeElement).filter((node): node is NativeElement => node?.p.attrs?.["data-node"] !== undefined);
}
function links(root: NativeNode | undefined): NativeElement[] {
	return nodes(root).map(asNativeElement).filter((node): node is NativeElement => node?.p.attrs?.["data-count"] !== undefined);
}

test("native capabilities are required independently, without image fallback", () => {
	const data = graph();
	expect(graphPreview(data, { ...cx, supports: () => false })).toBeUndefined();
	expect(graphPreview(data, { ...cx, feature: () => false })).toBeUndefined();
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
		const element = asNativeElement(node);
		if (!element) throw new Error("Graph contains a non-element node");
		expect(["div", "span", "section", "button"]).toContain(element.p.tag);
		for (const attribute of Object.keys(element.p.attrs ?? {})) expect(/^(data-|aria-|role$)/.test(attribute)).toBe(true);
		expect(element.p).not.toHaveProperty("href");
		expect(element.p).not.toHaveProperty("src");
		expect(element.p).not.toHaveProperty("style");
	}
	const untrusted = targets(preview).find(node => node.p?.attrs?.["data-node"] === data.nodes[0].id)!;
	expect(untrusted.p?.title).toContain(malicious);
	expect(untrusted.p?.actions).toBeUndefined();
	expect(targets(preview).find(node => node.p?.attrs?.["data-node"] === "b")?.p?.actions).toEqual({ click: "inspect-b" });
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

test("dot hovers retain unique source identity and full errors without details or incident evidence dumps", () => {
	const data = graph(), diagnostic = `Provider unavailable\n${"Exact backend diagnostic. ".repeat(80)}`;
	data.nodes[0] = { ...data.nodes[0], label: "app.render", detail: "Full detail stays in selection",
		hit: { qualifiedName: "app.render", symbol: "render", path: "src/views/render.ts" } as Hit,
		indexing: "failed", indexingError: diagnostic };
	data.edges = Array.from({ length: 8 }, (_, index) => ({ from: "a", to: "b", label: `actual relation ${index}` }));
	const preview = graphPreview(data, cx, { codebaseId: "hover", activeIds: new Set(["a"]), nodeAction: () => "inspect" });
	const title = targets(preview)[0].p.title!;
	expect(title.split("\n").filter(line => line === "app.render")).toHaveLength(1);
	expect(title).toContain("src/views/render.ts");
	expect(title).toContain(diagnostic);
	expect(title).not.toContain(data.nodes[0].detail!);
	for (const edge of data.edges) expect(title).not.toContain(edge.label);
	expect(title).not.toContain("→");
	expect(preview?.p.title?.split("\n").length).toBeLessThanOrEqual(5);
});

test("route hovers cap distinct evidence without changing direction, multiplicity or source bindings", () => {
	const data = graph();
	data.edges = Array.from({ length: 8 }, (_, index) => ({ from: "b", to: "a", label: "references", kind: "reference" as const,
		symbol: `render${index}`, moniker: `local::render${index}` }));
	data.edges.push(data.edges[0], data.edges[0]);
	const routes = links(graphPreview(data, cx));
	expect(routes).toHaveLength(1);
	expect(routes[0].p.attrs).toMatchObject({ "data-from": "b", "data-to": "a", "data-kind": "reference", "data-count": 10 });
	const title = routes[0].p.title!;
	expect(title.split("\n")[0]).toBe("B → A");
	for (const edge of data.edges.slice(0, 3)) {
		expect(title).toContain(edge.symbol!);
		expect(title).toContain(edge.moniker!);
	}
	expect(title).not.toContain(data.edges[3].moniker!);
	expect(title).toContain("+5");
	expect(title.split("\n").length).toBeLessThanOrEqual(6);
	expect(data.edges).toHaveLength(10);
});

test("grouped route hovers preserve source relationship counts rather than counting summary edges", () => {
	const data = graph();
	data.nodes[1].path = "other/b.ts";
	data.edges = Array.from({ length: 7 }, () => ({ from: "a", to: "b", label: "imports" }));
	const summary = projectSummary(data), routes = links(graphPreview(summary.graph, cx));
	expect(routes).toHaveLength(1);
	expect(routes[0].p.title).toContain(summary.graph.edges[0].label);
	expect(routes[0].p.title).not.toContain("1 directed relationship");
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
	const foreignColor = lookupGraphColor("local", foreign.nodes[0], "concepts");
	if (!foreignColor) throw new Error("Missing foreign structural fallback");
	expect(targets(graphPreview(foreign, cx, { codebaseId: "local" }))[0].p?.class).toContain(conceptClass(foreignColor, true));
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

test("current targets suppress older receipts while completed targets retain keyed feedback until expiry", () => {
	const data = graph(), activeIds = new Set(["a"]);
	for (const outcome of ["returned", "failed"] as const) {
		const feedback = new Map([
			["a", { id: 1, outcome }],
			["b", { id: 2, outcome }],
			["foreign", { id: 3, outcome }],
		]);
		const completed = targets(graphPreview(data, cx, { selected: "b", feedback }));
		for (const [index, target] of completed.entries()) {
			expect(target.p.title).toContain(`Recent request: ${outcome}`);
			expect(target.p.aria).toContain(`recently ${outcome}`);
			expect(nodes(target).filter(node => node.key?.startsWith("feedback:")).map(node => node.key)).toEqual([`feedback:${index + 1}`]);
		}
		const observed = graphPreview(data, cx, { activeIds, selected: "b", feedback }), observedTargets = targets(observed);
		expect(observedTargets.map(node => node.p.attrs?.["data-node"])).toEqual(["a", "b"]);
		expect(observedTargets[0].p.aria).toContain("active request target");
		expect(observedTargets[0].p.aria).not.toContain("recently");
		expect(observedTargets[0].p.title).toContain("Active request target");
		expect(observedTargets[0].p.title).not.toContain("Recent request:");
		expect(nodes(observedTargets[0]).some(node => node.key?.startsWith("feedback:"))).toBe(false);
		expect(observedTargets[1].p.aria).toContain(`recently ${outcome}`);
		expect(nodes(observedTargets[1]).filter(node => node.key?.startsWith("feedback:")).map(node => node.key)).toEqual(["feedback:2"]);
		expect(observedTargets[1].p.attrs?.["aria-pressed"]).toBe(true);
		expect(targets(graphPreview(data, cx, { feedback }))[0].p.aria).toContain(`recently ${outcome}`);
		expect(feedback.size).toBe(3);
		const settled = targets(graphPreview(data, cx, { activeIds, selected: "b" }));
		expect(settled[0].p.aria).toContain("active");
		for (const target of settled) {
			expect(target.p.aria).not.toContain("recently");
			expect(nodes(target).some(node => node.key?.startsWith("feedback:"))).toBe(false);
		}
		expect(settled[1].p.attrs?.["aria-pressed"]).toBe(true);
	}
});
test("catalog indexing rings persist independently of request receipts and keep geometry and concept colors", () => {
	const baseline = graph();
	baseline.nodes.push(...["c", "d", "e", "f"].map(id => ({ id, label: id, detail: null, hit: null, category: "file" as const, path: `src/${id}.ts` })));
	baseline.edges = [
		{ from: "a", to: "b", label: "references", kind: "reference", symbol: "render", moniker: "fixture::render" },
		{ from: "c", to: "d", label: "references", kind: "reference", symbol: "load", moniker: "fixture::load" },
		{ from: "e", to: "f", label: "references", kind: "reference", symbol: "store", moniker: "fixture::store" },
	];
	const metadata = readDisplay({ ...baseline, version: 1,
		identity: { codebaseId: "indexing-colors", name: null, localRoot: "D:/indexing-colors", copy: "checkout" } }, "overview", true);
	if (!metadata) throw new Error("Invalid overview fixture");
	setConceptSnapshot(metadata);
	const data: GraphData = { ...baseline, nodes: baseline.nodes.map((node, index) => ({
		...node, indexing: (["pending", "failed", "processing", "ready", "unknown"] as const)[index],
		...(index === 1 ? { indexingError: "Provider unavailable" } : {}),
	})) };
	const preview = graphPreview(data, cx, { codebaseId: "indexing-colors", activeIds: new Set(["a"]),
		feedback: new Map([["b", { id: 7, outcome: "returned" }]]) });
	const rendered = targets(preview), original = targets(graphPreview(baseline, cx, { codebaseId: "indexing-colors" }));
	for (const [index, target] of rendered.entries()) {
		const concept = lookupConcept("indexing-colors", data.nodes[index].path);
		if (!concept) throw new Error("Missing concept");
		expect(target.p.class).toContain(conceptClass(concept, cx.dark));
		expect(target.p.class?.match(/sg-x\d+ sg-y\d+/)?.[0]).toBe(original[index].p.class?.match(/sg-x\d+ sg-y\d+/)?.[0]);
	}
	expect(rendered[0].p.class).toContain("sg-active");
	expect(rendered[0].p.class).toContain("sg-indexing-pending");
	expect(rendered[1].p.title).toContain("Provider unavailable");
	expect(rendered[1].p.aria).toContain("recently returned");
	expect(rendered[2].p.class).toContain("sg-indexing-processing");
	for (const target of rendered.slice(3)) expect(target.p.class).not.toContain("sg-indexing-");
	expect(rendered[4].p.aria).toContain("status unknown");
	expect(rendered[5].p.attrs).not.toHaveProperty("data-indexing");
	expect(rendered[5].p.aria).not.toContain("Embedding");
	const expired = targets(graphPreview(data, { ...cx, reduceMotion: true }))[1];
	expect(expired.p.class).toContain("sg-indexing-failed");
	expect(expired.p.aria).not.toContain("recently");
	expect(expired.p.aria).toContain("Embedding failed");
	expect(nodes(expired).map(asNativeElement).filter(node => node?.p.class === "sg-indexing-state")).toHaveLength(1);
	expect(nodes(expired).map(asNativeElement).some(node => node?.p.class?.includes("sg-feedback"))).toBe(false);
});

test("native preview rejects graphs beyond its node and edge budgets", () => {
	const data = graph();
	data.nodes = Array.from({ length: 64 }, (_, index) => ({ id: String(index), label: `Node ${index}`, detail: null, hit: null }));
	data.roots = ["0"];
	data.edges = Array.from({ length: 128 }, (_, index) => ({ from: String(index % 64), to: String((index + 1) % 64), label: "calls" }));
	expect(targets(graphPreview(data, cx))).toHaveLength(64);
	data.nodes.push({ id: "64", label: "Overflow", detail: null, hit: null });
	expect(graphPreview(data, cx)).toBeUndefined();
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
			expect(Math.hypot(x - from[0], y - from[1])).toBeLessThan(.2);
			expect(Math.hypot(x + Math.cos(angle) * length - to[0], y + Math.sin(angle) * length - to[1])).toBeLessThan(.6);
		}
		const source = layout.dots.find(dot => dot.node.id === route.link.from)!, target = layout.dots.find(dot => dot.node.id === route.link.to)!;
		expect(Math.hypot(route.points[0][0] - source.x, route.points[0][1] - source.y)).toBeLessThan(14);
		const end = route.points.at(-1)!;
		expect(Math.hypot(end[0] - target.x, end[1] - target.y)).toBeLessThan(14);
	}
	expect(layout.routes[0].points).not.toEqual(layout.routes[2].points);
});

test("appearance variants keep exact directed evidence, indexing severity, selection and source data", () => {
	const data = graph();
	data.nodes[0].indexing = "pending";
	data.nodes[1].indexing = "failed"; data.nodes[1].indexingError = "Provider unavailable";
	data.edges.push({ from: "a", to: "b", label: "imports" },
		{ from: "b", to: "a", label: "references", kind: "reference", symbol: "render", moniker: "local:render" },
		{ from: "b", to: "b", label: "calls" });
	const original = structuredClone(data);
	const options = { size: "compact" as const, selected: "b", activeIds: new Set(["a"]),
		feedback: new Map([["b", { id: 12, outcome: "returned" as const }]]) };
	const baseline = graphPreview(data, cx, options), evidence = links(baseline).map(node => [node.p.attrs, node.p.title]);
	const layouts: GraphAppearance["layout"][] = ["force", "radial", "layers"], colors: GraphAppearance["color"][] = ["concepts", "structure", "neutral"];
	const details: GraphAppearance["detail"][] = ["calm", "balanced", "detailed"], labels: GraphAppearance["labels"][] = ["key", "more", "hover"];
	for (const [index, layout] of layouts.entries()) for (const [colorIndex, color] of colors.entries()) {
		const appearance: GraphAppearance = { ...DEFAULT_GRAPH_APPEARANCE, layout, color, detail: details[colorIndex], labels: labels[index],
			links: colorIndex % 2 ? "clear" : "soft", motion: index % 2 ? "off" : "system" };
		const cached = nativeGraphLayout(data, "compact", layout), preview = graphPreview(data, cx, { ...options, appearance });
		expect(nativeGraphLayout(data, "compact", layout)).toBe(cached);
		expect(links(preview).map(node => [node.p.attrs, node.p.title])).toEqual(evidence);
		const rendered = targets(preview);
		expect(rendered.map(node => node.p.attrs?.["data-node"])).toEqual(["a", "b"]);
		expect(rendered[0].p.class).toContain("sg-indexing-pending"); expect(rendered[0].p.class).toContain("sg-active");
		expect(rendered[1].p.class).toContain("sg-indexing-failed"); expect(rendered[1].p.title).toContain("Provider unavailable");
		expect(rendered[1].p.attrs?.["aria-pressed"]).toBe(true); expect(rendered[1].p.aria).toContain("recently returned");
		expect(preview?.p.class).toContain(`sg-links-${appearance.links}`);
		expect(preview?.p.attrs?.["data-motion"]).toBe(appearance.motion === "off" ? "reduced" : "full");
		expect(preview?.p.attrs?.["data-layout"]).toBe(layout);
	}
	expect(data).toEqual(original);
});

test("structural colors use the stable existing palette, neutral leaves status and selection distinct", () => {
	const data = graph(); data.nodes[1].indexing = "failed";
	const appearance = { ...DEFAULT_GRAPH_APPEARANCE, color: "structure" as const };
	const before = targets(graphPreview(data, cx, { appearance, codebaseId: "stable" })), expected = lookupGraphColor("stable", data.nodes[0], "structure");
	if (!expected) throw new Error("Missing structural color");
	expect(before[0].p.class).toContain(conceptClass(expected, true));
	const moved = { ...data, nodes: [...data.nodes].reverse(), edges: [...data.edges, { from: "b", to: "a", label: "calls" }] };
	const again = targets(graphPreview(moved, { ...cx, dark: false }, { appearance, codebaseId: "stable", selected: "a", activeIds: new Set(["a"]) }));
	expect(again.find(node => node.p.attrs?.["data-node"] === "a")?.p.class).toContain(conceptClass(expected, false));
	const neutral = graphPreview(data, cx, { selected: "b", appearance: { ...appearance, color: "neutral" } });
	expect(neutral?.p.class).toContain("sg-neutral");
	for (const node of targets(neutral)) expect(node.p.class).not.toContain("semctl-concept-");
	expect(targets(neutral)[1].p.class).toContain("sg-indexing-failed");
	expect(targets(neutral)[1].p.class).toContain("sg-selected");
});

test("key/more labels obey collision-aware budgets while hover labels remain available for every dot", () => {
	const data = graph();
	data.nodes = Array.from({ length: 24 }, (_, index) => ({ id: String(index), label: String.fromCharCode(65 + index), hit: null, detail: null, category: "group" }));
	data.roots = ["0"]; data.edges = data.nodes.slice(1).map(node => ({ from: "0", to: node.id, label: "calls" }));
	const options = { size: "compact" as const, appearance: DEFAULT_GRAPH_APPEARANCE };
	const key = nodes(graphPreview(data, cx, options)).map(asNativeElement).filter(node => node?.key === "label");
	const more = nodes(graphPreview(data, cx, { ...options, appearance: { ...DEFAULT_GRAPH_APPEARANCE, labels: "more" } })).map(asNativeElement).filter(node => node?.key === "label");
	const hover = nodes(graphPreview(data, cx, { ...options, appearance: { ...DEFAULT_GRAPH_APPEARANCE, labels: "hover" } })).map(asNativeElement).filter(node => node?.key === "label");
	expect(key).toHaveLength(24); expect(more).toHaveLength(24); expect(hover).toHaveLength(24);
	const keyVisible = key.filter(node => !node?.p.class?.includes("sg-label-transient")).length;
	const moreVisible = more.filter(node => !node?.p.class?.includes("sg-label-transient")).length;
	expect(keyVisible).toBeGreaterThan(0); expect(keyVisible).toBeLessThanOrEqual(3);
	expect(moreVisible).toBeGreaterThan(keyVisible); expect(moreVisible).toBeLessThanOrEqual(6);
	expect(hover.every(node => node?.p.class?.includes("sg-label-transient"))).toBe(true);
});

test("passive activity and receipt changes preserve every settled label and node position", () => {
	const data = graph();
	data.nodes = Array.from({ length: 24 }, (_, index) => ({ id: String(index), label: String.fromCharCode(65 + index), hit: null, detail: null, category: "group" }));
	data.roots = ["0"]; data.edges = data.nodes.slice(1).map(node => ({ from: "0", to: node.id, label: "calls" }));
	const labelState = (preview: NativeNode | undefined) => targets(preview).map(target => ({
		id: target.p.attrs?.["data-node"], position: target.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0],
		labels: nodes(target).map(asNativeElement).filter(node => node?.key === "label").map(node => ({ key: node?.key, text: node?.p.text, class: node?.p.class })),
	}));
	const appearances = [undefined, ...(["force", "radial", "layers", "components"] as const).flatMap(layout =>
		(["key", "more", "hover"] as const).map(labels => ({ ...DEFAULT_GRAPH_APPEARANCE, layout, labels })))];
	const feedback = new Map(data.nodes.map((node, index) => [node.id, { id: index, outcome: index % 2 ? "failed" as const : "returned" as const }]));
	for (const size of ["inline", "compact", "overview"] as const) for (const appearance of appearances) {
		const options = { size, appearance }, idle = labelState(graphPreview(data, cx, options));
		expect(idle).toHaveLength(data.nodes.length);
		for (const target of idle) {
			expect(target.position).toBeDefined();
			expect(target.labels).toHaveLength(1);
		}
		for (const parity of [0, 1]) {
			const activeIds = new Set(data.nodes.filter((_, index) => index % 2 === parity).map(node => node.id));
			expect(labelState(graphPreview(data, cx, { ...options, activeIds }))).toEqual(idle);
			expect(labelState(graphPreview(data, cx, { ...options, feedback }))).toEqual(idle);
			expect(labelState(graphPreview(data, cx, { ...options, activeIds, feedback }))).toEqual(idle);
		}
	}
});

test("deliberate selection promotes a hover label without moving its dot or hiding other hover details", () => {
	const data = graph(), appearance = { ...DEFAULT_GRAPH_APPEARANCE, labels: "hover" as const };
	const idle = targets(graphPreview(data, cx, { appearance }));
	const selected = targets(graphPreview(data, cx, { appearance, selected: "b", nodeAction: () => "inspect" }));
	const label = (target: NativeNode) => nodes(target).map(asNativeElement).find(node => node?.key === "label");
	expect(idle.every(target => label(target)?.p.class?.includes("sg-label-transient"))).toBe(true);
	expect(label(selected[1])?.p.class).not.toContain("sg-label-transient");
	expect(label(selected[1])?.p.text).toBe(label(idle[1])?.p.text);
	expect(label(selected[0])?.p.class).toContain("sg-label-transient");
	expect(selected.map(node => node.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0])).toEqual(idle.map(node => node.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]));
	expect(selected[1].p.attrs?.["aria-pressed"]).toBe(true);
	expect(selected[1].p.title).toContain("src/b.ts");
	expect(selected[1].p.actions).toEqual({ click: "inspect" });
});

test("motion preferences gate active effects without changing targets or treating queued embeddings as processing", () => {
	const data = graph();
	data.nodes[0].indexing = "pending"; data.nodes[1].indexing = "unknown";
	const idle = targets(graphPreview(data, cx));
	for (const target of idle) {
		expect(target.p.class).not.toContain("sg-active");
		expect(target.p.class).not.toContain("sg-indexing-processing");
	}
	expect(idle[0].p.class).toContain("sg-indexing-pending");
	expect(idle[1].p.attrs?.["data-indexing"]).toBe("unknown");
	const options = { activeIds: new Set(["b"]) }, ordinary = graphPreview(data, cx, options);
	const explicit = graphPreview(data, cx, { ...options, appearance: DEFAULT_GRAPH_APPEARANCE });
	expect(targets(explicit)).toEqual(targets(ordinary));
	expect(targets(explicit)[1].p.class).toContain("sg-active");
	expect(targets(explicit)[1].p.aria).toContain("status unknown");
	expect(targets(explicit)[1].p.class).not.toContain("sg-indexing-processing");
	expect(explicit?.p.attrs?.["data-motion"]).toBe("full");
	for (const [context, appearance] of [
		[cx, { ...DEFAULT_GRAPH_APPEARANCE, motion: "off" }],
		[{ ...cx, reduceMotion: true }, DEFAULT_GRAPH_APPEARANCE],
	] as const) {
		const reduced = graphPreview(data, context, { ...options, appearance });
		expect(reduced?.p.attrs?.["data-motion"]).toBe("reduced");
		expect(targets(reduced)).toEqual(targets(explicit));
	}
});

test("detail budgets retain every source node and account truthfully for every folded, drawn or omitted edge", () => {
	const data = graph();
	data.nodes = Array.from({ length: 30 }, (_, index) => ({ id: String(index), label: `File ${index}`, hit: null, detail: null,
		category: "file", path: `pkg${index}/unit/file.ts`, indexing: (["failed", "pending", "ready"] as const)[index % 3] }));
	data.roots = ["0", "29"]; data.caveats = ["Do not infer missing external references."];
	data.edges = data.nodes.flatMap(from => data.nodes.map(to => ({ from: from.id, to: to.id, label: "calls" })));
	const original = structuredClone(data);
	for (const [detail, nodeLimit, routeLimit] of [["calm", 12, 24], ["balanced", 16, 48], ["detailed", 24, 72]] as const) {
		const summary = projectSummary(data, null, detail);
		expect(summary.graph.nodes).toHaveLength(nodeLimit); expect(summary.graph.edges).toHaveLength(routeLimit);
		expect(summary.allGroups).toHaveLength(30); expect(summary.groupOf.size).toBe(30);
		expect(summary.groups.flatMap(group => group.members.map(node => node.id)).sort()).toEqual(data.nodes.map(node => node.id).sort());
		expect(summary.allGroups.flatMap(group => group.members.map(node => node.id)).sort()).toEqual(data.nodes.map(node => node.id).sort());
		expect(summary.groups.reduce((sum, group) => sum + group.internal, 0) + summary.drawnLinks + summary.omitted).toBe(data.edges.length);
		expect(summary.groups.reduce((sum, group) => sum + group.indexing.failed, 0)).toBe(10);
		const expectedRoots = data.roots.map(id => {
			const group = summary.groupOf.get(id);
			if (group === undefined) throw new Error(`Root ${id} has no projected group`);
			return group;
		});
		expect(summary.graph.roots).toEqual([...new Set(expectedRoots)]);
		expect(summary.omitted).toBeGreaterThan(0); expect(summary.graph.truncated).toBe(true);
		expect(summary.graph.caveats).toContain(data.caveats[0]);
		expect(summary.graph.caveats).toContain(`${summary.omitted} cross-group links are not drawn. Select a group or node to inspect exact relationships.`);
	}
	expect(projectSummary(data)).toEqual(projectSummary(data, null, "balanced"));
	expect(data).toEqual(original);
});

test("shared colors match source occurrences and projected members while mixed remainders remain neutral", () => {
	const data = graph(), codebase = "shared-projection";
	data.nodes.push({ id: "c", label: "Other", path: "other/c.ts", category: "file", hit: null, detail: null });
	data.edges.push({ from: "b", to: "c", label: "imports" });
	const summary = projectSummary(data, codebase);
	const groupId = summary.groupOf.get("a"), group = summary.graph.nodes.find(node => node.id === groupId)!;
	for (const mode of ["concepts", "structure"] as const) {
		const source = lookupGraphColor(codebase, data.nodes[0], mode);
		if (!source) throw new Error("Missing source category");
		expect(lookupGraphColor(codebase, group, mode)?.key).toBe(source.key);
		const occurrence = { id: "query-local-id", label: "render", hit: { codebaseId: codebase, path: data.nodes[0].path } as Hit, detail: null };
		expect(lookupGraphColor(codebase, occurrence, mode)?.key).toBe(source.key);
		const projected = targets(graphPreview(summary.graph, cx, { codebaseId: codebase, appearance: { ...DEFAULT_GRAPH_APPEARANCE, color: mode } }));
		expect(projected.find(node => node.p.attrs?.["data-node"] === groupId)?.p.class).toContain(conceptClass(source, true));
	}
	const large = graph();
	large.nodes = Array.from({ length: 30 }, (_, index) => ({ id: String(index), label: `File ${index}`,
		category: "file", path: `pkg${index}/file.ts`, hit: null, detail: null }));
	large.edges = []; large.roots = ["0"];
	const folded = projectSummary(large, codebase), remainder = folded.graph.nodes.find(node => node.id === "other-groups")!;
	expect(remainder).toBeDefined();
	for (const mode of ["concepts", "structure"] as const) expect(lookupGraphColor(codebase, remainder, mode)).toBeUndefined();
});

test("profile color changes repaint ordinary graph previews without changing evidence or geometry", () => {
	const data = graph(), layout = nativeGraphLayout(data, "inline"), concept = conceptRevision(), revision = graphColorRevision();
	const shared = graphPreview(data, cx, { codebaseId: "profile" });
	expect(targets(shared)[0].p.class).toContain("semctl-concept-");
	setGraphColorMode("neutral");
	const neutral = graphPreview(data, cx, { codebaseId: "profile" });
	expect(graphColorRevision()).toBeGreaterThan(revision); expect(conceptRevision()).toBe(concept);
	expect(nativeGraphLayout(data, "inline")).toBe(layout);
	expect(targets(neutral)[0].p.class).not.toContain("semctl-concept-");
	expect(links(neutral).map(node => [node.p.attrs, node.p.title])).toEqual(links(shared).map(node => [node.p.attrs, node.p.title]));
	expect(targets(neutral).map(node => node.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0])).toEqual(targets(shared).map(node => node.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]));
	const explicit = graphPreview(data, cx, { codebaseId: "profile", appearance: { ...DEFAULT_GRAPH_APPEARANCE, color: "structure" } });
	expect(targets(explicit)[0].p.class).toContain("semctl-concept-");
});

test("ordinary tool graph dots retain transient hover/focus labels beyond their static label budget", () => {
	const data = graph();
	data.nodes = Array.from({ length: 12 }, (_, index) => ({ id: String(index), label: `N${index}`, hit: null, detail: null }));
	data.roots = ["0"]; data.edges = data.nodes.slice(1).map(node => ({ from: "0", to: node.id, label: "calls" }));
	const labels = nodes(graphPreview(data, cx)).map(asNativeElement).filter(node => node?.key === "label");
	expect(labels).toHaveLength(12);
	expect(labels.some(node => node?.p.class?.includes("sg-label-transient"))).toBe(true);
});

test("native curve strip spans preserve total and cumulative quantized arclength from actual route points", () => {
	const data = graph(); data.edges.push({ from: "b", to: "a", label: "imports" }, { from: "a", to: "a", label: "within project root" });
	const layout = nativeGraphLayout(data, "overview");
	for (const route of layout.routes) {
		const { segments, total } = routeStrokeClasses(route.points, layout.width, layout.height);
		expect(segments).toHaveLength(route.points.length - 1);
		let expectedArc = 0;
		for (const [index, classes] of segments.entries()) {
			const fields = Object.fromEntries([...classes.matchAll(/sg-([a-z]+)(\d+)/g)].map(match => [match[1], Number(match[2])]));
			const arc = fields.cc + fields.c / 400 + fields.fc / 3200, length = fields.l / 400 + fields.fl / 3200;
			const from = route.points[index], to = route.points[index + 1];
			const quantized = quantizedSegment(from, to, layout.width, layout.height);
			const actualLength = Math.hypot(to[0] - from[0], to[1] - from[1]);
			expect(length * 100).toBeCloseTo(quantized.length, 12);
			expect(Math.abs(length * layout.width - actualLength)).toBeLessThanOrEqual(layout.width / 6400 + 1e-9);
			const nextArc = expectedArc + quantized.length / 100;
			for (const width of [147.5, 280, 760]) {
				expect(arc * width).toBeCloseTo(expectedArc * width, 8);
				expect((arc + length) * width).toBeCloseTo(nextArc * width, 8);
			}
			expectedArc = nextArc;
		}
		const fields = Object.fromEntries([...total.matchAll(/sg-([a-z]+)(\d+)/g)].map(match => [match[1], Number(match[2])]));
		const totalArc = fields.tc + fields.t / 400 + fields.ft / 3200;
		expect(totalArc).toBeGreaterThan(0);
		for (const width of [147.5, 280, 760]) expect(totalArc * width).toBeCloseTo(expectedArc * width, 8);
	}
});

test("positive active context only marks incident links and distinguishes progress, return and failure", () => {
	const data = graph();
	data.nodes.push({ id: "c", label: "C", detail: null, hit: null, category: "file", path: "src/c.ts" },
		{ id: "d", label: "D", detail: null, hit: null, category: "file", path: "src/d.ts" });
	data.edges.push({ from: "b", to: "c", label: "imports" }, { from: "c", to: "d", label: "within project root" });
	const layout = nativeGraphLayout(data, "inline");
	const preview = graphPreview(data, cx, { codebaseId: "activity", activeIds: new Set(["a"]), selected: "d",
		feedback: new Map([["b", { id: 1, outcome: "returned" }], ["d", { id: 2, outcome: "failed" }]]) });
	const observed = links(preview).filter(node => node.p.attrs?.["data-active-context"] === true);
	expect(observed).toHaveLength(1);
	expect(observed[0].p.attrs).toMatchObject({ "data-from": "a", "data-to": "b" });
	expect(observed[0].p.class).toContain("sg-observed");
	for (const route of links(preview)) {
		expect(nodes(route).map(asNativeElement).filter(node => node?.p.class === "sg-arrow")).toHaveLength(1);
		if (!route.c) throw new Error("Missing native route segments");
		expect(nodes(route).map(asNativeElement).filter(node => node?.p.class === "sg-hit").length).toBe(route.c.length);
	}
	const rendered = targets(preview);
	expect(rendered[1].p.aria).toContain("recently returned"); expect(rendered[1].p.aria).not.toContain(", active");
	expect(rendered[3].p.aria).toContain("recently failed"); expect(rendered[3].p.aria).toContain("selected");
	expect(nativeGraphLayout(data, "inline")).toBe(layout);
});

test("whole-route strip encoding retains carries and a single short connector rather than miniature intervals", () => {
	const width = 760, height = 320;
	for (const points of [
		[[20, 40], [700, 40], [700, 280], [20, 280]],
		[[100, 100], [103, 100]],
	]) {
		const geometry = routeStrokeClasses(points, width, height);
		const fields = Object.fromEntries([...geometry.total.matchAll(/sg-([a-z]+)(\d+)/g)].map(match => [match[1], Number(match[2])]));
		const total = (fields.tc + fields.t / 400 + fields.ft / 3200) * width;
		const expected = points.slice(1).reduce((sum, to, index) => sum + quantizedSegment(points[index], to, width, height).length / 100 * width, 0);
		expect(total).toBeCloseTo(expected, 8);
		expect(geometry.segments).toHaveLength(points.length - 1);
		if (points.length === 2) { expect(total).toBeGreaterThan(0); expect(total).toBeLessThan(4); }
		else expect(fields.tc).toBeGreaterThanOrEqual(2);
	}
});

test("every chord and hover copy shares its whole route strip while keeping arrowheads and exact evidence", () => {
	const data = graph();
	data.edges = [{ from: "a", to: "b", label: "imports" }, { from: "b", to: "a", label: "calls" },
		{ from: "a", to: "a", label: "within project root" }];
	const layout = nativeGraphLayout(data, "inline"), preview = graphPreview(data, cx);
	const all = nodes(preview).map(asNativeElement).filter((node): node is NativeElement => node !== undefined);
	for (const route of links(preview)) {
		const actual = layout.routes.find(candidate => candidate.link.from === route.p.attrs?.["data-from"] &&
			candidate.link.to === route.p.attrs?.["data-to"] && candidate.link.kind === route.p.attrs?.["data-kind"]);
		if (!actual) throw new Error("Missing evidence route");
		const expected = routeStrokeClasses(actual.points, layout.width, layout.height), copies = all.filter(node => node.key === route.key);
		expect(copies.length).toBeGreaterThanOrEqual(2);
		for (const copy of copies) {
			expect(copy.p.class?.match(/sg-t\d+ sg-tc\d+ sg-ft\d+/)?.[0]).toBe(expected.total);
			const strokes = nodes(copy).map(asNativeElement).filter((node): node is NativeElement => node?.p.class === "sg-stroke");
			expect(strokes).toHaveLength(expected.segments.length);
			for (const stroke of strokes) expect(nodes(stroke).map(asNativeElement).filter(node => node?.p.class === "sg-ink")).toHaveLength(1);
			expect(nodes(copy).map(asNativeElement).filter(node => node?.p.class === "sg-arrow")).toHaveLength(1);
		}
		expect(route.p.title).toContain(data.edges[actual.link.indices[0]].label);
	}
});

test("quantized segment lengths reconstruct the physical Euclidean chord distance", () => {
	for (const { width, height, from, to } of [
		{ width: 560, height: 216, from: [100, 80], to: [126, 80] },
		{ width: 560, height: 216, from: [100, 80], to: [103, 80] },
		{ width: 280, height: 176, from: [20, 20], to: [106, 113] },
		{ width: 760, height: 320, from: [5, 14], to: [700, 300] },
	] as const) {
		const actual = Math.hypot(to[0] - from[0], to[1] - from[1]);
		const segment = quantizedSegment(from, to, width, height), reconstructed = segment.length / 100 * width;
		expect(Math.abs(reconstructed - actual)).toBeLessThanOrEqual(width / 6400 + 1e-9);
	}
});
