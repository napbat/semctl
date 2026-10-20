import { expect, test } from "bun:test";
import { boundedTooltip, compactTooltip, TOOLTIP_LINE_CHARS, TOOLTIP_LINES } from "./tooltip-text";

test("compact tooltips keep the first three lines in importance order", () => {
	expect(compactTooltip("identity", "fact", "hint", "dropped")).toBe("identity\nfact\nhint");
	expect(compactTooltip("one\ntwo\nthree\nfour")).toBe("one\ntwo\nthree");
	expect(compactTooltip("one\ntwo\nthree\nfour").split("\n")).toHaveLength(TOOLTIP_LINES);
	expect(boundedTooltip(["one", "two", "three", "four", "five"], 4)).toBe("one\ntwo\nthree\nfour");
});

test("long lines end with an ellipsis inside the character cap", () => {
	const title = compactTooltip("x".repeat(200), "short");
	const [first, second] = title.split("\n");
	expect(first).toHaveLength(TOOLTIP_LINE_CHARS);
	expect(first.endsWith("…")).toBe(true);
	expect(second).toBe("short");
	expect(compactTooltip("y".repeat(TOOLTIP_LINE_CHARS))).toBe("y".repeat(TOOLTIP_LINE_CHARS));
	expect(compactTooltip(`${"a".repeat(TOOLTIP_LINE_CHARS - 2)} ${"b".repeat(10)}`)).toBe(`${"a".repeat(TOOLTIP_LINE_CHARS - 2)}…`);
});

test("duplicate lines collapse before the line cap is counted", () => {
	expect(compactTooltip("same", "same", "other", "same\nlast")).toBe("same\nother\nlast");
	expect(compactTooltip("a  b", "a b")).toBe("a b");
});

test("control characters and blank lines never reach the tooltip", () => {
	expect(compactTooltip("na\u0000me\u001b[31m", "\u0007", "  ", "\tpath\u009f  ")).toBe("name[31m\npath");
	expect(compactTooltip("one\r\ntwo")).toBe("one\ntwo");
});

test("empty input yields an empty tooltip", () => {
	expect(compactTooltip()).toBe("");
	expect(compactTooltip(undefined, null, false, "", " \n ")).toBe("");
	expect(boundedTooltip([], 4)).toBe("");
});
