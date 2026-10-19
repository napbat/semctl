import { describe, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { DEFAULT_GRAPH_APPEARANCE, GraphAppearanceStore } from "./graph-appearance";
import { GraphAppearancePanel, type GraphAppearanceKeyMatcher } from "./graph-appearance-panel";
import { asNativeElement } from "./native-elements";

const native: DescribeContext = { cols: 40, dark: true, reduceMotion: false, supports: () => true, feature: () => true };
const match: GraphAppearanceKeyMatcher = (data, key) => data === key;
function nodes(root: NativeNode): NativeNode[] {
	const result = [root];
	for (const child of root.c ?? []) if ("k" in child) result.push(...nodes(child));
	return result;
}
function action(panel: GraphAppearancePanel, act: string): void {
	panel.handleNativeEvent({ type: "action", key: "", act, mods: [] });
}
function control(panel: GraphAppearancePanel, key: string, cx = native) {
	const found = nodes(panel.describe(cx)).find(node => node.key === key);
	if (!found) throw new Error(`Missing native control ${key}`);
	return found;
}
function harness(store = new GraphAppearanceStore()) {
	let renders = 0, closes = 0;
	const panel = new GraphAppearancePanel(store, () => renders++, () => closes++, match);
	return { panel, store, get renders() { return renders; }, get closes() { return closes; } };
}

// All SDK imports here are type-only; the input matcher is a deterministic instance dependency.
describe("native graph appearance panel", () => {

	test("clicks change live preferences immediately and expose independent selected states", async () => {
		const h = harness();
		action(h.panel, "appearance:layout:radial");
		action(h.panel, "appearance:color:neutral");
		action(h.panel, "appearance:detail:detailed");
		action(h.panel, "appearance:labels:hover");
		action(h.panel, "appearance:links:clear");
		action(h.panel, "appearance:motion:off");
		expect(h.store.state.value).toEqual({ layout: "radial", color: "neutral", detail: "detailed", labels: "hover", links: "clear", motion: "off" });
		for (const key of ["layout:radial", "color:neutral", "detail:detailed", "labels:hover", "links:clear", "motion:off"]) {
			expect(asNativeElement(control(h.panel, `appearance:${key}`))?.p.attrs?.["aria-pressed"]).toBe(true);
		}
		expect(asNativeElement(control(h.panel, "appearance:motion:system"))?.p.attrs?.["aria-pressed"]).toBe(false);
		expect(h.renders).toBeGreaterThan(0);
		action(h.panel, "appearance:layout:unknown");
		action(h.panel, "appearance:unknown:off");
		expect(h.store.state.value.layout).toBe("radial");
		action(h.panel, "reset");
		await h.store.flush();
		expect(h.store.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
		h.panel.dispose();
	});

	test("keyboard moves rows, wraps choices in both directions, cycles with Enter and reaches reset/close", () => {
		const h = harness();
		h.panel.handleInput("left");
		expect(h.store.state.value.layout).toBe("components");
		h.panel.handleInput("right");
		expect(h.store.state.value.layout).toBe("force");
		h.panel.handleInput("enter");
		expect(h.store.state.value.layout).toBe("radial");
		h.panel.handleInput("down");
		h.panel.handleInput("right");
		expect(h.store.state.value.color).toBe("structure");
		for (let i = 0; i < 4; i++) h.panel.handleInput("down");
		h.panel.handleInput("enter");
		expect(h.store.state.value.motion).toBe("off");
		h.panel.handleInput("down"); // Reset defaults.
		h.panel.handleInput("enter");
		expect(h.store.state.value).toEqual(DEFAULT_GRAPH_APPEARANCE);
		for (let i = 0; i < 7; i++) h.panel.handleInput("down");
		h.panel.handleInput("enter");
		expect(h.closes).toBe(1);
		h.panel.handleInput("escape");
		expect(h.closes).toBe(2);
		h.panel.dispose();
	});

	test("all four layouts expose their meaning without hover and remain reachable by pointer and keyboard", () => {
		const h = harness();
		const help = () => {
			const node = nodes(h.panel.describe(native)).find(node => node.key === "help");
			return node?.k === "text" ? node.p : undefined;
		};
		const layouts = nodes(h.panel.describe(native)).find(node => node.p?.role === "semctl.appearance.layouts");
		expect(layouts?.c).toHaveLength(4);
		expect(help()?.text).toContain("Nearby nodes do not imply extra relationships");
		h.panel.handleInput("right");
		expect(help()?.text).toContain("undirected hops");
		h.panel.handleInput("right");
		expect(help()?.text).toContain("Cycles share a column");
		h.panel.handleInput("right");
		expect(h.store.state.value.layout).toBe("components");
		expect(help()?.text).toContain("Rows within a block do not imply direction");
		expect(asNativeElement(control(h.panel, "appearance:layout:components"))?.p.attrs?.["aria-pressed"]).toBe(true);
		h.panel.handleInput("right");
		expect(h.store.state.value.layout).toBe("force");
		action(h.panel, "appearance:layout:components");
		expect(h.store.state.value.layout).toBe("components");
		expect(help()?.aria).toBe(help()?.text);
		action(h.panel, "appearance:detail:calm");
		expect(help()?.text).toContain("Overview maps");
		expect(help()?.text).toContain("Exact node navigation stays available");
		h.panel.handleInput("up");
		expect(help()?.text).toContain("Color");
		expect(h.store.state.value.layout).toBe("components");
		h.panel.dispose();
	});

	test("only unselected option controls own hover help while selected facts stay visible", () => {
		const h = harness();
		for (const supportsElements of [true, false]) {
			const cx: DescribeContext = { ...native, supports: kind => supportsElements || String(kind) !== "el" };
			const all = nodes(h.panel.describe(cx));
			for (const node of all.filter(node => node.p?.role === "semctl.appearance.row" || node.p?.role === "semctl.appearance.label")) {
				expect(node.p?.title).toBeUndefined();
			}
			expect(control(h.panel, "appearance:layout:force", cx).p?.title).toBeUndefined();
			expect(control(h.panel, "appearance:layout:radial", cx).p?.title?.split("\n")).toHaveLength(2);
			expect(control(h.panel, "appearance:layout:force", cx).p?.aria).toBeTruthy();
		}
		action(h.panel, "appearance:color:neutral");
		const help = nodes(h.panel.describe(native)).find(node => node.key === "help");
		expect(help?.k === "text" && help.p?.text).toContain("not similarity, confidence, or indexing status");
		expect(help?.p?.aria).toBe(help?.k === "text" ? help.p?.text : undefined);
		h.panel.dispose();
	});

	test("supports stock-style ANSI keys when no matcher is supplied", () => {
		const store = new GraphAppearanceStore();
		let closes = 0;
		const panel = new GraphAppearancePanel(store, () => {}, () => closes++);
		panel.handleInput("\x1b[C");
		expect(store.state.value.layout).toBe("radial");
		panel.handleInput("\x1b[B");
		panel.handleInput("\r");
		expect(store.state.value.color).toBe("structure");
		panel.handleInput("\x1b");
		expect(closes).toBe(1);
		panel.dispose();
	});

	test("motion selection stays explicit across system reduced-motion changes and narrow descriptions", () => {
		const h = harness();
		action(h.panel, "appearance:motion:off");
		for (const reduceMotion of [true, false]) {
			const cx = { ...native, cols: 20, reduceMotion };
			expect(asNativeElement(control(h.panel, "appearance:motion:off", cx))?.p.attrs?.["aria-pressed"]).toBe(true);
			expect(nodes(h.panel.describe(cx)).some(node => node.k === "spinner" || node.k === "shimmer")).toBe(false);
		}
		action(h.panel, "appearance:motion:system");
		expect(h.store.state.value.motion).toBe("system");
		expect(asNativeElement(control(h.panel, "appearance:motion:system", { ...native, reduceMotion: true }))?.p.attrs?.["aria-pressed"]).toBe(true);
		h.panel.dispose();
	});

	test("save errors remain visible and retry saves the selected in-memory value", async () => {
		const writeStarted = Promise.withResolvers<void>(), writeGate = Promise.withResolvers<void>();
		let contents = JSON.stringify({ version: 1, appearance: DEFAULT_GRAPH_APPEARANCE }), calls = 0;
		const store = new GraphAppearanceStore("preferences.json", {
			async read() { return contents; },
			async writeAtomic(_file, next) {
				if (++calls === 1) { writeStarted.resolve(); await writeGate.promise; }
				contents = next;
			},
		});
		const h = harness(store);
		await store.load();
		action(h.panel, "appearance:color:neutral");
		await writeStarted.promise;
		expect(store.state.saving).toBe(true);
		const saving = nodes(h.panel.describe(native)).find(node => node.p?.role === "semctl.appearance.status");
		expect(saving?.k === "text" && saving.p?.text).toMatch(/saving/i);
		writeGate.reject(new Error("disk is read-only"));
		await store.flush();
		const error = nodes(h.panel.describe(native)).find(node => node.p?.role === "semctl.appearance.error");
		expect(error?.p?.tone).toBe("error");
		expect(error?.k === "text" && error.p?.text).toContain("disk is read-only");
		expect(asNativeElement(control(h.panel, "appearance:color:neutral"))?.p.attrs?.["aria-pressed"]).toBe(true);
		expect(control(h.panel, "retry").p?.actions?.click).toBe("retry");
		action(h.panel, "retry");
		await store.flush();
		expect(JSON.parse(contents).appearance.color).toBe("neutral");
		expect(store.state.error).toBeUndefined();
		expect(nodes(h.panel.describe(native)).some(node => node.key === "retry")).toBe(false);
		const saved = nodes(h.panel.describe(native)).find(node => node.p?.role === "semctl.appearance.status");
		expect(saved?.k === "text" && saved.p?.text).toMatch(/saved/i);
		action(h.panel, "reset");
		await store.flush();
		expect(JSON.parse(contents).appearance).toEqual(DEFAULT_GRAPH_APPEARANCE);
		h.panel.dispose();
	});

	test("retry can recover a corrupt load without replacing it, and reset explicitly repairs it", async () => {
		let contents = "not JSON", writes = 0;
		const store = new GraphAppearanceStore("preferences.json", {
			async read() { return contents; },
			async writeAtomic(_file, next) { writes++; contents = next; },
		});
		const h = harness(store);
		await store.load();
		expect(store.state.error).toBeTruthy();
		action(h.panel, "retry");
		await store.flush();
		expect(writes).toBe(0);
		expect(contents).toBe("not JSON");
		for (let i = 0; i < 6; i++) h.panel.handleInput("down");
		h.panel.handleInput("enter"); // Keyboard retry, not an appearance edit.
		await store.flush();
		expect(writes).toBe(0);
		action(h.panel, "reset");
		await store.flush();
		expect(writes).toBe(1);
		expect(store.state.error).toBeUndefined();
		h.panel.dispose();
	});

	test("supported native item fallback remains actionable and keyboard-selectable", () => {
		const h = harness();
		const cx: DescribeContext = { ...native, supports: kind => (kind as string) !== "el" };
		const target = control(h.panel, "appearance:links:clear", cx);
		expect(target.k).toBe("item");
		expect(target.p?.aria).toBeTruthy();
		expect(target.p?.actions?.click).toBe("appearance:links:clear");
		h.panel.handleNativeEvent({ type: "activate", key: "body", item: "appearance:links:clear" });
		expect(h.store.state.value.links).toBe("clear");
		expect(control(h.panel, "appearance:links:clear", cx).p?.tone).toBe("accent");
		h.panel.dispose();
	});

	test("dispose unsubscribes and rejects late native events and keys after close or session reset", async () => {
		const h = harness();
		action(h.panel, "appearance:motion:off");
		h.panel.dispose();
		h.panel.dispose();
		const renders = h.renders;
		action(h.panel, "appearance:motion:system");
		action(h.panel, "reset");
		action(h.panel, "close");
		h.panel.handleNativeEvent({ type: "activate", key: "", item: "appearance:layout:layers" });
		h.panel.handleInput("right");
		h.panel.handleInput("escape");
		expect(h.store.state.value).toEqual({ ...DEFAULT_GRAPH_APPEARANCE, motion: "off" });
		expect(h.closes).toBe(0);
		await h.store.update({ color: "neutral" });
		expect(h.renders).toBe(renders);
	});
});
