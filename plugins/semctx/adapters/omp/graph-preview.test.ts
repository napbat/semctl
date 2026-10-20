import { afterEach, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { graphPreview } from "./graph-preview";
import type { GraphData } from "./graph";
import { conceptRevision, lookupConcept, resetConcepts, setConceptSnapshot } from "./concept-colors";
import { conceptClass } from "./native-concepts";
import { nativeGraphLayout } from "./graph-native-layout";
import { readDisplay, type Hit } from "./inspector";
import { GRAPH_STYLES, positionClasses, quantizedSegment, routeStrokeClasses } from "./graph-native-styles";
import { asNativeElement, type NativeElement } from "./native-elements";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { projectSummary } from "./graph-projection";
import { graphColorRevision, lookupGraphColor, setGraphColorMode } from "./graph-colors";
import { bindDrawingWeight } from "./graph-data/layout.js";
import { bindIndexingCounts, boundIndexingCounts, indexingCounts } from "./graph-indexing";

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
function elements(root: NativeNode | undefined, className: string): NativeElement[] {
	return nodes(root).map(asNativeElement).filter((node): node is NativeElement => node?.p.class?.split(" ").includes(className) ?? false);
}
function expectCompact(title: string | undefined, lines = 3): void {
	if (title === undefined) throw new Error("Missing tooltip");
	expect(title.split("\n").length).toBeLessThanOrEqual(lines);
	for (const line of title.split("\n")) expect(line.length).toBeLessThanOrEqual(88);
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

test("dot hovers lead with unique identity and one status line without details or incident evidence dumps", () => {
	const data = graph(), diagnostic = `Provider unavailable\n${"Exact backend diagnostic. ".repeat(80)}`;
	data.nodes[0] = { ...data.nodes[0], label: "app.render", detail: "Full detail stays in selection",
		hit: { qualifiedName: "app.render", symbol: "render", path: "src/views/render.ts" } as Hit,
		indexing: "failed", indexingError: diagnostic };
	data.edges = Array.from({ length: 8 }, (_, index) => ({ from: "a", to: "b", label: `actual relation ${index}` }));
	const preview = graphPreview(data, cx, { codebaseId: "hover", activeIds: new Set(["a"]), nodeAction: () => "inspect" });
	const title = targets(preview)[0].p.title!;
	expectCompact(title);
	expect(title.split("\n")[0]).toBe("app.render");
	expect(title.split("\n")[1]).toBe("src/views/render.ts");
	expect(title).toContain("Provider unavailable");
	expect(title).toContain("Active request target");
	expect(title).not.toContain("Exact backend diagnostic");
	expect(title).not.toContain(data.nodes[0].detail!);
	for (const edge of data.edges) expect(title).not.toContain(edge.label);
	expect(title).not.toContain("→");
	expectCompact(preview?.p.title, 2);
});

test("route hovers fold extra evidence into the last line without changing direction, multiplicity or source bindings", () => {
	const data = graph();
	data.edges = Array.from({ length: 8 }, (_, index) => ({ from: "b", to: "a", label: "references", kind: "reference" as const,
		symbol: `render${index}`, moniker: `local::render${index}` }));
	data.edges.push(data.edges[0], data.edges[0]);
	const routes = links(graphPreview(data, cx));
	expect(routes).toHaveLength(1);
	expect(routes[0].p.attrs).toMatchObject({ "data-from": "b", "data-to": "a", "data-kind": "reference", "data-count": 10 });
	const title = routes[0].p.title!;
	expectCompact(title);
	expect(title.split("\n")[0]).toStartWith("B → A");
	expect(title.split("\n")[0]).toContain("10");
	for (const edge of data.edges.slice(0, 2)) {
		expect(title).toContain(edge.symbol!);
		expect(title).toContain(edge.moniker!);
	}
	expect(title).not.toContain(data.edges[2].moniker!);
	expect(title.split("\n").at(-1)).toEndWith("+6 more");
	expect(data.edges).toHaveLength(10);
	const long = graph();
	long.edges = Array.from({ length: 3 }, (_, index) => ({ from: "b", to: "a", label: "references", kind: "reference" as const,
		symbol: `${"symbol".repeat(20)}${index}`, moniker: `local::${"moniker".repeat(20)}${index}` }));
	const folded = links(graphPreview(long, cx))[0].p.title!;
	expectCompact(folded);
	expect(folded.split("\n").at(-1)).toEndWith("+1 more");
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

test("layout changes retain the plane and node keys while remounting routes and territories at current positions", () => {
	const data = graph();
	data.nodes.push({ id: "c", label: "C", detail: null, hit: null, category: "file", path: "src/c.ts" });
	data.edges.push({ from: "a", to: "c", label: "imports" });
	const force = graphPreview(data, cx, { appearance: DEFAULT_GRAPH_APPEARANCE });
	const radial = graphPreview(data, cx, { appearance: { ...DEFAULT_GRAPH_APPEARANCE, layout: "radial" } });
	const rerender = graphPreview(data, { ...cx, dark: false }, { selected: "b", activeIds: new Set(["a"]),
		appearance: { ...DEFAULT_GRAPH_APPEARANCE, layout: "radial", color: "neutral", labels: "more", motion: "off" } });
	const nodeKeys = (preview: NativeNode | undefined) => nodes(preview).map(node => node.key).filter(key => key?.startsWith("node:"));
	expect(nodeKeys(force)).toEqual(["node:a", "node:b", "node:c"]);
	expect(nodeKeys(radial)).toEqual(nodeKeys(force));
	expect(nodeKeys(rerender)).toEqual(nodeKeys(force));
	for (const [preview, layoutChoice] of [[force, "force"], [radial, "radial"], [rerender, "radial"]] as const) {
		const plane = elements(preview, "sg-plane")[0], children = plane.c?.map(child => asNativeElement(child as NativeNode));
		expect(plane.key).toBe("plane");
		expect(children?.map(child => child?.p.class)).toEqual(["sg-territories", "sg-routes", "sg-node", "sg-node", "sg-node"]);
		const layout = nativeGraphLayout(data, "inline", layoutChoice);
		for (const target of targets(preview)) {
			const dot = layout.dots.find(candidate => candidate.node.id === target.p.attrs?.["data-node"])!;
			expect(target.key).toBe("target");
			expect(target.p.class).toContain(positionClasses(dot.x, dot.y, layout.width, layout.height));
		}
	}
	for (const className of ["sg-routes", "sg-territories"]) {
		expect(elements(force, className)[0].key).not.toBe(elements(radial, className)[0].key);
		expect(elements(rerender, className)[0].key).toBe(elements(radial, className)[0].key);
	}
	expect(targets(rerender).map(target => target.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]))
		.toEqual(targets(radial).map(target => target.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]));
	const resized = graphPreview(data, cx, { size: "compact", appearance: DEFAULT_GRAPH_APPEARANCE });
	const refreshed = graphPreview({ ...data, nodes: [...data.nodes], edges: [...data.edges] }, cx, { appearance: DEFAULT_GRAPH_APPEARANCE });
	for (const changed of [resized, refreshed]) {
		expect(elements(changed, "sg-routes")[0].key).not.toBe(elements(force, "sg-routes")[0].key);
		expect(nodeKeys(changed)).toEqual(nodeKeys(force));
	}
});

test("node morph transitions include explicit durations and dense map styles keep lighter low-tier links and compact fills", () => {
	const css = GRAPH_STYLES.map(sheet => sheet.css).join("\n");
	const target = css.match(/\.semctl-graph\[data-motion=full\] \.sg-target\{([^}]+)\}/)?.[1];
	expect(target).toContain("transition:left 560ms spring(560ms,.08),top 560ms spring(560ms,.08)");
	for (const [index, opacity] of [.55, .85, 1.1, 1.4].entries()) expect(css).toContain(`.sg-route.sg-w${index + 1}{--sg-weight-opacity:${opacity}}`);
	for (const [index, scale] of [1, 1.35, 1.8, 2.4].entries()) expect(css).toContain(`.sg-w${index + 1} .sg-stroke{transform:scaleY(${scale})}`);
	expect(css).toContain(".semctl-graph.sg-compact .sg-territory{background:radial-gradient(closest-side,rgb(from var(--sg-tint) r g b / 9%),rgb(from var(--sg-tint) r g b / 4%) 72%,transparent)}");
});

test("shared graph wells set the opaque canvas and dot separation background from theme colors", () => {
	const css = GRAPH_STYLES.map(sheet => sheet.css).join("\n");
	const well = css.match(/\.semctl-graph-well\{([^}]+)\}/)?.[1];
	expect(well).toContain("--sg-well:color-mix(in oklab, var(--panel) 95%, var(--t1))");
	expect(well).toContain("--sg-bg:var(--sg-well)");
	expect(well).toContain("border-radius:8px");
	expect(well).toContain("background:var(--sg-well)");
	expect(well).toContain("padding:6px");
	expect(well).toContain("border:0");
	expect(well).toContain("display:flex");
	expect(well).toContain("justify-content:center");
	expect(css).toContain(".semctl-graph-well .semctl-graph{flex:1 1 auto;width:100%}");
	expect(css.match(/\.semctl-graph\{([^}]+)\}/)?.[1]).toContain("--sg-bg:var(--sg-well,var(--panel))");
});

test("neutral dots have canvas separation and centered hover scales while labels use secondary theme text", () => {
	const css = GRAPH_STYLES.map(sheet => sheet.css).join("\n");
	const dot = css.match(/\.semctl-graph \.sg-dot\{([^}]+)\}/)?.[1];
	expect(css).toContain("color:var(--sg-node);--sg-fg:");
	expect(css).toContain("--sg-node:color-mix(in oklab, var(--sg-fg) 45%, var(--sg-bg))");
	expect(css).toContain(".semctl-graph.sg-neutral .sg-target{color:var(--sg-node)}");
	expect(dot).toContain("box-shadow:0 0 0 1.5px var(--sg-bg)");
	expect(dot).toContain("transform:translate(-50%,-50%) scale(var(--sg-dot-scale,1))");
	expect(css).toContain(".sg-target:hover .sg-dot,.semctl-graph .sg-target:focus-visible .sg-dot{--sg-dot-scale:1.18}");
	expect(css).toContain("transition:transform 120ms ease-out,");
	expect(css).toContain("to{opacity:var(--sg-dot-opacity,1);transform:translate(-50%,-50%) scale(var(--sg-dot-scale,1))}");
	expect(css).toContain(".sg-selected .sg-dot{outline:2px solid var(--sg-accent);outline-offset:2px}");
	expect(css).toContain(".sg-muted .sg-dot{--sg-dot-opacity:.28;opacity:.28}");
	const label = css.match(/\.semctl-graph \.sg-label\{([^}]+)\}/)?.[1];
	expect(label).toContain("font:500 11px/14px");
	expect(label).toContain("color:var(--t2)");
	expect(label).toContain("drop-shadow(1px 0 1px var(--sg-bg))");
	expect(css).toContain(".semctl-graph.sg-compact .sg-label{font-size:10.5px}");
	expect(css).toContain(".sg-target:focus-visible .sg-label{color:var(--t1);font-weight:600}");
});


test("indexing status pips use small separated dots and the node tooltip names their state", () => {
	const css = GRAPH_STYLES.map(sheet => sheet.css).join("\n");
	const pip = css.match(/\.semctl-graph \.sg-indexing-state\{([^}]+)\}/)?.[1];
	expect(pip).toContain("width:5px;height:5px");
	expect(pip).toContain("left:calc(50% + (var(--sg-d) / 2 + 2px) * .62)");
	expect(pip).toContain("top:calc(50% + (var(--sg-d) / 2 + 2px) * .62)");
	expect(pip).toContain("background:transparent");
	expect(pip).toContain("box-shadow:0 0 0 1.5px var(--sg-bg)");
	expect(css).toContain(".sg-indexing-pending .sg-indexing-state{border:1px solid var(--sg-edge)}");
	expect(css).toContain(".sg-indexing-processing .sg-indexing-state{background:var(--sg-accent)}");
	expect(css).toContain(".sg-indexing-failed .sg-indexing-state{background:var(--sg-negative)}");
	const data = graph(); data.nodes[0].indexing = "failed";
	const preview = graphPreview(data, cx);
	expectCompact(preview?.p.title, 2);
	expect(targets(preview)[0].p.title).toContain("Embedding failed");
	expect(elements(targets(preview)[0], "sg-indexing-state")[0].key).toBe("indexing:failed");
	expect(targets(preview)[0].p.aria).toContain("Embedding failed");
});

test("indexing count bindings use node identity without changing protocol fields", () => {
	const node = graph().nodes[0], copy = { ...node }, before = JSON.stringify(node);
	const counts = { ...indexingCounts([]), failed: 1, ready: 3 };
	expect(boundIndexingCounts(node)).toBeUndefined();
	bindIndexingCounts(node, counts);
	expect(boundIndexingCounts(node)).toBe(counts);
	expect(boundIndexingCounts(copy)).toBeUndefined();
	expect(JSON.stringify(node)).toBe(before);
	const updated = { ...counts, ready: 4 };
	bindIndexingCounts(node, updated);
	expect(boundIndexingCounts(node)).toBe(updated);
});

test("bound failed-member shares grade group pips at the quarter and sixty-percent boundaries", () => {
	for (const [failed, members, bucket] of [[12, 61, "faint"], [24, 100, "faint"], [25, 100, "mid"],
		[60, 100, "mid"], [61, 100, "strong"]] as const) {
		const data = graph(), node = data.nodes[0];
		node.category = "group"; node.indexing = "failed";
		const before = targets(graphPreview(data, cx)).find(target => target.p.attrs?.["data-node"] === node.id)!;
		expect(before.p.class).not.toContain("sg-pip-");
		bindIndexingCounts(node, { ...indexingCounts([]), failed, ready: members - failed - 1, unreported: 1 });
		const target = targets(graphPreview(data, cx)).find(target => target.p.attrs?.["data-node"] === node.id)!;
		expect(target.p.class?.match(/\bsg-pip-\w+\b/)?.[0]).toBe(`sg-pip-${bucket}`);
		expect(target.p.class).toContain("sg-indexing-failed");
		expect(target.p.title).toContain(`Failed · ${failed} of ${members} members`);
		expectCompact(target.p.title);
		expect(elements(target, "sg-indexing-state")[0].key).toBe("indexing:failed");
		expect(target.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0])
			.toBe(before.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]);
	}
	const data = graph(); data.nodes[0].indexing = "processing";
	bindIndexingCounts(data.nodes[0], { ...indexingCounts([]), processing: 3 });
	expect(targets(graphPreview(data, cx)).find(target => target.p.attrs?.["data-node"] === data.nodes[0].id)?.p.class).not.toContain("sg-pip-");
	const css = GRAPH_STYLES.map(sheet => sheet.css).join("\n");
	expect(css).toContain(".sg-pip-faint .sg-indexing-state{--sg-pip-opacity:.4;opacity:.4;width:4px;height:4px}");
	expect(css).toContain(".sg-pip-mid .sg-indexing-state{--sg-pip-opacity:.75;opacity:.75}");
	expect(css).toContain(".sg-pip-strong .sg-indexing-state{--sg-pip-opacity:1;opacity:1;width:6px;height:6px}");
	expect(css).toContain("@keyframes semctl-graph-failure{from{opacity:.2}to{opacity:var(--sg-pip-opacity,1)}}");
});

test("focus mutes every unrelated dot while keeping request and indexing cues", () => {
	const data = graph();
	data.nodes.push(...["c", "d", "e"].map((id, index) => ({ id, label: id, detail: null, hit: null,
		indexing: (["failed", "processing", "pending"] as const)[index] })));
	const preview = graphPreview(data, cx, { selected: "a", activeIds: new Set(["c"]),
		feedback: new Map([["d", { id: 1, outcome: "returned" }]]) });
	const rendered = targets(preview);
	for (const target of rendered.slice(0, 2)) expect(target.p.class).not.toContain("sg-muted");
	for (const target of rendered.slice(2)) {
		expect(target.p.class).toContain("sg-muted");
		expect(elements(target, "sg-indexing-state")).toHaveLength(1);
	}
	expect(rendered[2].p.class).toContain("sg-active");
	expect(elements(rendered[3], "sg-feedback")).toHaveLength(1);
});

test("weighted dots use bounded diameter classes and deterministic root-first arrival buckets", () => {
	const data = graph();
	data.nodes.push(...["d", "c"].map(id => ({ id, label: id, detail: null, hit: null, category: "group" as const })));
	data.roots = ["d"];
	data.edges = [{ from: "b", to: "a", label: "calls" }, { from: "b", to: "c", label: "calls" }, { from: "b", to: "d", label: "calls" }];
	data.nodes.forEach((node, index) => bindDrawingWeight(node, index + 1));
	const layout = nativeGraphLayout(data, "inline"), preview = graphPreview(data, cx);
	for (const target of targets(preview)) {
		const dot = layout.dots.find(candidate => candidate.node.id === target.p.attrs?.["data-node"])!;
		const diameter = Math.round(dot.radius * 2);
		expect(diameter).toBeGreaterThanOrEqual(7); expect(diameter).toBeLessThanOrEqual(19);
		expect(target.p.class?.split(" ")).toContain(`sg-s${diameter}`);
		expect(elements(target, "sg-dot")).toHaveLength(1);
		expect(elements(target, "sg-dot")[0].key).toBe("dot");
		expect(target.p.class).not.toContain("sg-large");
	}
	const delays = Object.fromEntries(targets(preview).map(target => [target.p.attrs?.["data-node"], target.p.class?.match(/\bsg-d[0-7]\b/)?.[0]]));
	expect(delays).toEqual({ d: "sg-d0", b: "sg-d1", a: "sg-d2", c: "sg-d3" });
	const reordered = graphPreview({ ...data, nodes: [...data.nodes].reverse(), edges: [...data.edges].reverse() }, cx);
	expect(Object.fromEntries(targets(reordered).map(target => [target.p.attrs?.["data-node"], target.p.class?.match(/\bsg-d[0-7]\b/)?.[0]]))).toEqual(delays);
});

test("route weight classes appear only for routes with bound drawing tiers and retain every hover copy", () => {
	const ordinary = graph();
	for (const route of elements(graphPreview(ordinary, cx), "sg-route")) expect(route.p.class).not.toMatch(/\bsg-w[1-4]\b/);
	const weighted = graph();
	weighted.nodes.push({ id: "c", label: "C", detail: null, hit: null });
	weighted.edges.push({ from: "b", to: "c", label: "imports" });
	bindDrawingWeight(weighted.edges[0], 12);
	const layout = nativeGraphLayout(weighted, "inline"), preview = graphPreview(weighted, cx);
	expect(layout.routes.some(route => route.tier !== undefined)).toBe(true);
	for (const route of layout.routes) {
		const key = `route:${JSON.stringify([route.link.from, route.link.to, route.link.kind])}`;
		const copies = elements(preview, "sg-route").filter(element => element.key === key);
		expect(copies.length).toBeGreaterThanOrEqual(2);
		for (const copy of copies) {
			if (route.tier !== undefined) expect(copy.p.class?.split(" ")).toContain(`sg-w${route.tier}`);
			else expect(copy.p.class).not.toMatch(/\bsg-w[1-4]\b/);
		}
	}
});

test("lexical territories render layout bounds, theme tints and honest tooltips without chips in compact previews", () => {
	const data = graph();
	data.nodes = ["CacheEntryStore", "CacheEntryRead", "CacheEntryEvict", "CacheEntryWrite",
		"QueryExecutionPlan", "QueryExecutionRun", "QueryExecutionParse", "QueryExecutionExplain"].map(label => ({
		id: label, label, detail: null, hit: null, category: "file" as const,
		path: `${label.startsWith("Cache") ? "cache/entry" : "query/execution"}/${label}.ts`,
	}));
	data.roots = [data.nodes[0].id, data.nodes[4].id];
	data.edges = data.nodes.flatMap((node, index) => index % 4 ? [{ from: data.nodes[index - index % 4].id, to: node.id, label: "calls" }] : []);
	const layout = nativeGraphLayout(data, "inline");
	expect(layout.territories.length).toBeGreaterThan(0);
	for (const dark of [false, true]) {
		const preview = graphPreview(data, { ...cx, dark }), territories = elements(preview, "sg-territory");
		expect(territories).toHaveLength(layout.territories.length);
		expectCompact(preview?.p.title, 2);
		expect(preview?.p.title).not.toContain("embedding");
		for (const [index, territory] of layout.territories.entries()) {
			const element = territories[index], chip = elements(element, "sg-territory-label")[0];
			expect(element.key).toBe(`territory:${territory.key}`);
			expect(element.p.class?.split(" ")).toContain(`sg-territory-${territory.kind}`);
			expect(element.p.class).toContain(positionClasses(territory.x, territory.y, layout.width, layout.height));
			expect(element.p.class?.split(" ")).toContain(`sg-tint-${territory.color[dark ? "dark" : "light"].slice(1)}`);
			expect(element.p.class?.split(" ")).toContain(`sg-tw${Math.max(0, Math.min(200, Math.round(territory.rx * 400 / layout.width)))}`);
			expect(element.p.class?.split(" ")).toContain(`sg-th${Math.max(0, Math.min(200, Math.round(territory.ry * 400 / layout.height)))}`);
			if (chip) {
				expect(chip.p.text).toBe(territory.label);
				expect(chip.p.title).toBe(territory.title ?? `${territory.label}\nShared identifier terms`);
				expectCompact(chip.p.title);
			} else {
				expect(element.p.attrs?.["aria-hidden"]).toBe(true);
				expect(element.p.title).toBeUndefined();
			}
		}
	}
	const compact = graphPreview(data, cx, { size: "compact" });
	expect(elements(compact, "sg-territory").length).toBeGreaterThan(0);
	expect(elements(compact, "sg-territory-label")).toHaveLength(0);
	const neutral = graphPreview(data, cx, { appearance: { ...DEFAULT_GRAPH_APPEARANCE, color: "neutral" } });
	expect(neutral?.p.class).toContain("sg-neutral");
	expect(elements(neutral, "sg-territory")).toHaveLength(layout.territories.length);
	for (const layoutChoice of ["radial", "layers", "components"] as const) {
		const preview = graphPreview(data, cx, { appearance: { ...DEFAULT_GRAPH_APPEARANCE, layout: layoutChoice } });
		expect(nativeGraphLayout(data, "inline", layoutChoice).territories).toHaveLength(0);
		expect(elements(preview, "sg-territory")).toHaveLength(0);
		expectCompact(preview?.p.title, 2);
	}
});

test("territory chips choose clear edges or stay absent and node labels avoid every placed chip", () => {
	const data = graph();
	data.nodes = ["a", "b", "c", "d"].map(id => ({ id, label: `Node${id.toUpperCase()}`, detail: null, hit: null, category: "group" as const }));
	data.edges = [];
	const layout = nativeGraphLayout(data, "inline");
	const coordinates: Record<string, readonly [number, number]> = { a: [160, 45], b: [380, 45], c: [380, 95], d: [260, 90] };
	layout.dots = data.nodes.map(node => ({ node, x: coordinates[node.id][0], y: coordinates[node.id][1], radius: 3.5, degree: 0, root: node.id === "a" }));
	const territory = { kind: "lexical" as const, terms: [], members: [], cohesion: 1, rx: 40, ry: 25, color: { light: "#526fbd", dark: "#8ca8ef" } };
	layout.territories = [
		{ ...territory, key: "a-free", label: "free", x: 80, y: 60, rx: 35, ry: 20 },
		{ ...territory, key: "b-bottom", label: "bottom", x: 160, y: 70 },
		{ ...territory, key: "c-conflict", label: "second", x: 80, y: 65 },
		{ ...territory, key: "d-hidden", label: "hidden", x: 380, y: 70 },
		{ ...territory, key: "e-label", label: "label", x: 301, y: 101, ry: 20 },
	];
	const preview = graphPreview(data, cx), rendered = elements(preview, "sg-territory");
	const chipBoxes = layout.territories.flatMap(territory => {
		const element = rendered.find(element => element.key === `territory:${territory.key}`)!;
		const chip = elements(element, "sg-territory-label")[0];
		if (!chip) {
			expect(territory.key).toBe("d-hidden");
			expect(element.p.attrs?.["aria-hidden"]).toBe(true);
			expect(element.p.title).toBeUndefined();
			return [];
		}
		const below = chip.p.class?.includes("sg-territory-label-bottom") ?? false;
		expect(below).toBe(territory.key === "b-bottom" || territory.key === "c-conflict");
		const width = Array.from(territory.label).length * 6 + 12, y = territory.y + (below ? territory.ry : -territory.ry);
		return [{ left: territory.x - width / 2, right: territory.x + width / 2, top: y - 7, bottom: y + 7 }];
	});
	expect(chipBoxes).toHaveLength(4);
	for (const [index, box] of chipBoxes.entries()) {
		for (const dot of layout.dots) expect(dot.x + dot.radius > box.left && dot.x - dot.radius < box.right && dot.y + dot.radius > box.top && dot.y - dot.radius < box.bottom).toBe(false);
		for (const other of chipBoxes.slice(index + 1)) expect(box.left < other.right && box.right > other.left && box.top < other.bottom && box.bottom > other.top).toBe(false);
	}
	for (const appearance of [undefined, { ...DEFAULT_GRAPH_APPEARANCE, labels: "hover" as const }]) {
		for (const target of targets(graphPreview(data, cx, { appearance }))) {
			const dot = layout.dots.find(dot => dot.node.id === target.p.attrs?.["data-node"])!, label = elements(target, "sg-label")[0];
			const width = Array.from(label.p.text ?? "").length * 6.8 + 4, gap = 9 + (dot.radius - 3.5);
			const left = dot.x + (label.p.class?.includes("sg-label-left") ? -gap - width : gap);
			const top = dot.y + (label.p.class?.includes("sg-label-below") ? gap - 2 : -gap - 7);
			for (const chip of chipBoxes) expect(left < chip.right && left + width > chip.left && top < chip.bottom && top + 14 > chip.top).toBe(false);
		}
	}
	expect(elements(targets(preview).find(target => target.p.attrs?.["data-node"] === "d"), "sg-label")[0].p.class).toContain("sg-label-below");
	const before = Object.fromEntries(rendered.map(element => [element.key, elements(element, "sg-territory-label").map(chip => chip.p.class)]));
	layout.territories.reverse(); layout.dots.reverse();
	const reordered = graphPreview(data, cx, { selected: "a", activeIds: new Set(["b"]) });
	expect(Object.fromEntries(elements(reordered, "sg-territory").map(element => [element.key, elements(element, "sg-territory-label").map(chip => chip.p.class)]))).toEqual(before);
});

test("compact labels near the right canvas edge flip left with room for the text halo", () => {
	const data = graph();
	data.nodes = [{ ...data.nodes[0], category: "group", label: "src/daemon" }];
	data.edges = [];
	const layout = nativeGraphLayout(data, "compact");
	layout.dots = [{ ...layout.dots[0], x: 202, y: 80, radius: 3.5 }];
	layout.territories = [];
	const target = targets(graphPreview(data, cx, { size: "compact" })).find(target => target.p.attrs?.["data-node"] === "a");
	const label = elements(target, "sg-label")[0];
	expect(label.p.text).toBe("src/daemon");
	expect(label.p.class).toContain("sg-label-left");
	expect(label.p.class).not.toContain("sg-label-transient");
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
test("catalog indexing status pips persist independently of request receipts and keep geometry and concept colors", () => {
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
		for (const node of summary.graph.nodes) {
			expect(boundIndexingCounts(node)).toBe(summary.groups.find(group => group.id === node.id)?.indexing);
		}
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
