import { describe, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { GraphActivity } from "./graph-activity";
import type { GraphRequestState } from "./overview";
import { GRAPH_STATUS_STYLES, graphStatus, graphStatusModel } from "./graph-status";
import { asNativeElement } from "./native-elements";

const idle = (): GraphActivity => ({ operations: [], targets: [], producer: null });
const settled = (): GraphRequestState => ({ loading: false, text: "" });

describe("graph status evidence precedence", () => {
	test("a refresh error overrides producer completion without inventing progress", () => {
		const model = graphStatusModel({ request: { ...settled(), error: "connection refused" }, activity: {
			...idle(), producer: { text: "embedded", symbol: "status.success", tone: "success", progress: 1 },
		} });
		expect(model.mode).toBe("failed");
		expect(model.tone).toBe("error");
		expect(model.progress).toBeUndefined();
	});
	test("active requests take precedence over an expired operation's failure receipt", () => {
		const activity: GraphActivity = { ...idle(), operations: ["imports"],
			recent: [{ id: 1, operation: "find_definition", outcome: "failed", targets: ["old.ts"] }] };
		expect(graphStatusModel({ request: settled(), activity }).mode).toBe("working");
		expect(graphStatusModel({ request: settled(), activity }).tone).toBe("success");
		expect(graphStatusModel({ request: settled(), activity: { ...activity, operations: [] } }).mode).toBe("failed");
	});
	test("a queued producer remains pending with zero progress instead of reporting active work", () => {
		const model = graphStatusModel({ request: settled(), activity: { ...idle(),
			producer: { text: "embedding 0/2", symbol: "status.pending", tone: "accent", progress: 0 },
		} });
		expect(model.mode).toBe("pending");
		expect(model.progress).toBe(0);
		expect(model.active).toBe(0);
	});
	test("inventory text updates retain the reported completion state", () => {
		const model = (text: string) => graphStatusModel({ request: settled(), activity: { ...idle(),
			producer: { text, symbol: "status.success", tone: "success", progress: 1 },
		} });
		const before = model("scanning · last observed job completed"), after = model("planning · 87 files · last observed job completed");
		expect(after.mode).toBe("ready");
		expect(after.label).toBe(before.label);
		expect(after.progress).toBe(1);
	});
	test("producer failure stays visible while a separate request is active", () => {
		const model = graphStatusModel({ request: settled(), activity: { ...idle(), operations: ["imports"],
			producer: { text: "failed", symbol: "status.error", tone: "error", details: "Embedding provider rejected the request" },
		} });
		expect(model.mode).toBe("failed");
		expect(model.active).toBe(1);
	});
	test("unknown loading does not fabricate a progress ratio or request activity", () => {
		const model = graphStatusModel({ request: { loading: true, text: "" }, activity: idle() });
		expect(model.mode).toBe("loading");
		expect(model.progress).toBeUndefined();
		expect(model.active).toBe(0);
	});
});

const native: DescribeContext = { cols: 40, dark: true, reduceMotion: false, supports: () => true, feature: () => true };
function nodes(root: NativeNode): NativeNode[] {
	return [root, ...(root.c ?? []).flatMap(child => "k" in child ? nodes(child) : [])];
}
const visibleText = (root: NativeNode) => nodes(root).filter(node => node.k === "text").map(node => node.p?.text).join("\n");
const warningSlot = (root: NativeNode) => nodes(root).find(node => node.p?.role === "semctl.live.warning");

describe("compact live progress", () => {
	test("phase changes stay in tooltips while unknown progress remains unknown", () => {
		const phases: NonNullable<GraphActivity["producer"]>[] = [
			{ text: "scanning", tone: "accent", symbol: "status.running" },
			{ text: "planning", tone: "accent", symbol: "status.running" },
			{ text: "failed", tone: "error", symbol: "status.error", diagnostic: "Provider rejected embedding" },
		];
		for (const producer of phases) {
			const view = graphStatus(native, { request: settled(), activity: { ...idle(), producer }, presentation: "icon" });
			expect(visibleText(view)).toBe("—");
			expect(nodes(view).find(node => node.k === "meter")?.p?.value).toBeNull();
			expect(nodes(view).some(node => node.p?.title?.includes(producer.text))).toBe(true);
			expect(warningSlot(view) !== undefined).toBe(producer.symbol === "status.error");
			if (producer.diagnostic) expect(nodes(view).some(node => node.p?.title?.includes(producer.diagnostic!))).toBe(true);
		}
	});
	test("zero and incomplete progress do not imply completion and map errors do not erase producer progress", () => {
		for (const [progress, expected] of [[0, "0%"], [0.37, "37%"], [0.999, "99%"], [1, "100%"]] as const) {
			const view = graphStatus(native, { request: { ...settled(), error: "Map refresh disconnected" },
				activity: { ...idle(), producer: { text: "embedding", tone: "accent", progress } }, presentation: "icon" });
			expect(visibleText(view)).toBe(expected);
			expect(nodes(view).find(node => node.k === "meter")?.p?.value).toBe(progress);
			expect(nodes(view).some(node => node.p?.title?.includes("Map refresh disconnected"))).toBe(true);
			expect(warningSlot(view)?.p?.title).toContain("Map refresh disconnected");
		}
	});
	test("active warnings remain discoverable without dumping producer snapshots into hovers", () => {
		const diagnostic = `Provider rejected embedding\n${"reason ".repeat(60)}\nFull trace`;
		const view = graphStatus(native, { request: settled(), presentation: "icon", activity: { ...idle(), producer: {
			text: "scanning", symbol: "status.running", tone: "warning", diagnostic, details: `Full producer snapshot\n${diagnostic}`,
		} } });
		expect(warningSlot(view)).toBeDefined();
		const tooltip = nodes(view).find(node => node.k === "meter")?.p?.title ?? "";
		expect(tooltip).toContain("Provider rejected embedding");
		expect(tooltip).not.toContain("Full producer snapshot");
		expect(tooltip).not.toContain("Full trace");
		expect(tooltip).toContain("Diagnostics");
		expect(tooltip.split("\n").length).toBeLessThanOrEqual(4);
		expect(tooltip.split("\n").every(line => line.length <= 88)).toBe(true);
		for (const node of nodes(view)) if (node.p?.title) {
			expect(node.p.title.split("\n").length).toBeLessThanOrEqual(4);
			expect(node.p.title.split("\n").every(line => line.length <= 88)).toBe(true);
		}
	});
	test("footer slots read ring, percentage, spacer, warning and traffic without overlap", () => {
		const activity: GraphActivity = { ...idle(), producer: { text: "embedding", tone: "warning", progress: 0.5 }, requests: { sent: 3, received: 2, failed: 0 } };
		const alerting = graphStatus(native, { request: settled(), activity, presentation: "icon" });
		expect((alerting.c as NativeNode[]).map(node => node.key ?? node.p?.role)).toEqual([
			"status-slot", "semctl.live.progress-value", "spacer", "warning", "semctl.live.traffic"]);
		const ring = (alerting.c as NativeNode[])[0];
		expect(nodes(ring).filter(node => node !== ring).map(node => node.k)).toEqual(["meter"]);
		expect(warningSlot(ring)).toBeUndefined();
		const calm = graphStatus(native, { request: settled(), activity: { ...activity, producer: { text: "embedding", tone: "accent", progress: 0.5 } }, presentation: "icon" });
		expect(warningSlot(calm)).toBeUndefined();
		expect((calm.c as NativeNode[]).map(node => node.key ?? node.p?.role)).toEqual([
			"status-slot", "semctl.live.progress-value", "spacer", "semctl.live.traffic"]);
		const fallback = graphStatus({ ...native, supports: kind => kind === "meter" }, { request: settled(), activity, presentation: "icon" });
		expect(warningSlot(fallback)?.p?.text).toBe("!");
	});
	test("traffic counters read sent and returned arrows in a tidy 22px row", () => {
		const view = graphStatus(native, { request: settled(), activity: { ...idle(), requests: { sent: 3, received: 2, failed: 1 } }, presentation: "icon" });
		const traffic = nodes(view).find(node => node.p?.role === "semctl.live.traffic")!;
		expect(nodes(traffic).filter(node => node.k === "text").map(node => node.p?.text)).toEqual(["↑ 3", "↓ 2"]);
		expect(traffic.p?.aria).toBe("3 observed requests sent, 2 returned, 1 failed");
		expect(traffic.p?.title).toContain("3 sent · 2 returned · 1 failed");
		expect(GRAPH_STATUS_STYLES.css).toContain('[data-role="semctl.live.traffic"] .sf-text{font-size:11px;font-variant-numeric:tabular-nums}');
		expect(GRAPH_STATUS_STYLES.css).toContain(".semctl-graph-status[data-presentation=icon]{box-sizing:content-box;height:22px;min-height:22px");
		expect(GRAPH_STATUS_STYLES.css).toContain('[data-role="semctl.live.warning"] .ico{width:12px;height:12px}');
	});
	test("percentage digits animate only when motion is allowed", () => {
		const activity: GraphActivity = { ...idle(), producer: { text: "embedding", tone: "accent", progress: 0.4 } };
		const motion = (view: NativeNode) => asNativeElement(view)?.p.attrs?.["data-motion"];
		expect(motion(graphStatus(native, { request: settled(), activity, presentation: "icon" }))).toBe("full");
		expect(motion(graphStatus(native, { request: settled(), activity, presentation: "icon", motion: "off" }))).toBe("reduced");
		expect(motion(graphStatus({ ...native, reduceMotion: true }, { request: settled(), activity, presentation: "icon" }))).toBe("reduced");
		expect(GRAPH_STATUS_STYLES.css).toContain('[data-motion=full] [data-role="semctl.live.progress-value"]{-stencil-text-effect:numeric;-stencil-text-duration:320ms}');
		expect(GRAPH_STATUS_STYLES.css).toMatch(/prefers-reduced-motion:reduce\)\{[^\n]*-stencil-text-effect:none/);
		expect(GRAPH_STATUS_STYLES.css).not.toContain("position:absolute");
	});
});

describe("graph page status", () => {
	const loading = (): GraphRequestState => ({ loading: true, text: "" });
	test("loading and working use a native orbit spinner when motion is allowed", () => {
		const cases: [GraphRequestState, GraphActivity][] = [[loading(), idle()], [settled(), { ...idle(), operations: ["imports"] }]];
		for (const [request, activity] of cases) {
			const view = graphStatus(native, { request, activity });
			const spinner = nodes(view).find(node => node.k === "spinner");
			expect(spinner?.p).toMatchObject({ style: "orbit", tone: "accent" });
			expect(spinner?.p?.aria).toBe(asNativeElement(view)?.p.aria);
			expect(nodes(view).some(node => asNativeElement(node)?.p.class === "semctl-state-ring")).toBe(false);
		}
	});
	test("reduced motion, missing spinner support and settled modes keep the state ring", () => {
		for (const view of [
			graphStatus({ ...native, reduceMotion: true }, { request: loading(), activity: idle() }),
			graphStatus(native, { request: loading(), activity: idle(), motion: "off" }),
			graphStatus({ ...native, supports: kind => kind !== "spinner" }, { request: loading(), activity: idle() }),
			graphStatus(native, { request: settled(), activity: idle() }),
		]) {
			expect(nodes(view).some(node => node.k === "spinner")).toBe(false);
			expect(nodes(view).some(node => asNativeElement(node)?.p.class === "semctl-state-ring")).toBe(true);
		}
	});
});
