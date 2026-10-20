import { describe, expect, test } from "bun:test";
import { scoreScale, scoreTitle, strengthScale, strengths, verificationTitle } from "./matches.js";

describe("shared match indicators", () => {
	test("preserves relative strength and original rank", () => {
		const rows = [{ score: 8 }, { score: 2 }, { score: 0 }];
		const scales = strengths(rows);
		expect(scales).toEqual([1, 0.25, 0]);
		expect(scoreTitle(rows[1].score, scales[1], 1)).toContain("Rank 2");
		expect(scoreTitle(rows[1].score, scales[1], 1)).toContain("Score 2");
	});
	test("retains unknown strength for missing, non-finite or all-zero scores without inventing confidence", () => {
		for (const scores of [[0, 0], [2, null], [-2, null], [2, Infinity], [-2, -Infinity]]) {
			const rows = scores.map(score => ({ score }));
			expect(strengths(rows)).toEqual(scores.map(() => null));
			expect(scoreScale(rows)).toBeNull();
			for (const row of rows) expect(strengthScale(row)).toBeNull();
		}
		expect(strengths([])).toEqual([]);
		expect(scoreScale([])).toBeNull();
		const title = scoreTitle(0, null, 0);
		expect(title).toContain("Score 0");
		expect(title).toContain("unavailable");
		expect(title).toContain("not confidence");
	});
	test("signed score sets use a floored range scale between the lowest and highest scores", () => {
		const mixed = [{ score: 2 }, { score: -1 }];
		expect(scoreScale(mixed)).toBe("range");
		expect(strengths(mixed)).toEqual([1, 0.1]);
		expect(strengthScale(mixed[0])).toBe("range");
		const rows = [{ score: -1.59 }, { score: -6.14 }, { score: -3.865 }];
		const scales = strengths(rows);
		expect(scales[0]).toBe(1);
		expect(scales[1]).toBe(0.1);
		expect(scales[2]).toBeCloseTo(0.55, 12);
		for (const row of rows) expect(strengthScale(row)).toBe("range");
		for (const scores of [[-3, -3], [-1.5, -1.5 + 1e-13]]) {
			const equal = scores.map(score => ({ score }));
			expect(strengths(equal)).toEqual([null, null]);
			expect(scoreScale(equal)).toBeNull();
			expect(strengthScale(equal[0])).toBeNull();
		}
	});
	test("tooltips name the ratio or range basis and never claim confidence", () => {
		const ratio = [{ score: 8 }, { score: 2 }], range = [{ score: -1 }, { score: -5 }];
		expect(scoreScale(ratio)).toBe("ratio");
		const ratioTitle = scoreTitle(2, strengths(ratio)[1], 1, strengthScale(ratio[1]));
		expect(ratioTitle).toContain("Bar: relative to highest result score");
		expect(ratioTitle).toContain("not confidence");
		const rangeTitle = scoreTitle(-5, strengths(range)[1], 1, strengthScale(range[1]));
		expect(rangeTitle).toContain("Bar: position between lowest and highest result scores");
		expect(rangeTitle).toContain("Score -5");
		expect(rangeTitle).toContain("not confidence");
		expect(scoreTitle(-5, null, 1, "range")).toContain("Relative strength unavailable");
		expect(scoreTitle(2, 0.25, 1)).toContain("relative to highest result score");
	});
	test("keeps exact raw scores and diagnostic reasons available to tooltip consumers", () => {
		const score = 0.127403;
		expect(scoreTitle(score, 0.5, 4)).toContain(String(score));
		for (const state of ["bound", "changed-local", "range-mismatch", "unverified", "not-probed"]) {
			const reason = `specific-${state}-reason`;
			expect(verificationTitle({ state, reason })).toContain(reason);
			expect(verificationTitle({ state, reason: null })).not.toContain("null");
		}
		const titles = ["bound", "changed-local", "range-mismatch", "unverified", "not-probed"]
			.map(state => verificationTitle({ state, reason: null }));
		expect(new Set(titles).size).toBe(titles.length);
	});
});
