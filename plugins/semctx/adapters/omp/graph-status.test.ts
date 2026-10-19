import { describe, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { GraphActivity } from "./graph-activity";
import type { GraphRequestState } from "./overview";
import { graphStatus, graphStatusModel } from "./graph-status";
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
			const warning = nodes(view).map(asNativeElement).find(node => node?.p.attrs?.["data-alert"] !== undefined);
			expect(warning?.p.attrs?.["data-alert"]).toBe(producer.symbol === "status.error");
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
			expect(nodes(view).map(asNativeElement).some(node => node?.p.attrs?.["data-alert"] === true)).toBe(true);
		}
	});
	test("active warnings remain discoverable without dumping producer snapshots into hovers", () => {
		const diagnostic = `Provider rejected embedding\n${"reason ".repeat(60)}\nFull trace`;
		const view = graphStatus(native, { request: settled(), presentation: "icon", activity: { ...idle(), producer: {
			text: "scanning", symbol: "status.running", tone: "warning", diagnostic, details: `Full producer snapshot\n${diagnostic}`,
		} } });
		expect(nodes(view).map(asNativeElement).some(node => node?.p.attrs?.["data-alert"] === true)).toBe(true);
		const tooltip = nodes(view).find(node => node.k === "meter")?.p?.title ?? "";
		expect(tooltip).toContain("Provider rejected embedding");
		expect(tooltip).not.toContain("Full producer snapshot");
		expect(tooltip).not.toContain("Full trace");
		expect(tooltip).toContain("Diagnostics");
		expect(tooltip.split("\n").every(line => line.length <= 180)).toBe(true);
	});
});
