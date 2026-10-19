import { describe, expect, test } from "bun:test";
import { scoreTitle, strengths, verificationTitle } from "./matches.js";

describe("shared match indicators", () => {
	test("preserves relative strength and original rank", () => {
		const rows = [{ score: 8 }, { score: 2 }, { score: 0 }];
		const scales = strengths(rows);
		expect(scales).toEqual([1, 0.25, 0]);
		expect(scoreTitle(rows[1].score, scales[1], 1)).toContain("Rank 2");
		expect(scoreTitle(rows[1].score, scales[1], 1)).toContain("Score 2");
	});
	test("retains unknown strength for missing, negative or all-zero scores without inventing confidence", () => {
		for (const scores of [[0, 0], [2, null], [2, -1], [2, Infinity]]) {
			expect(strengths(scores.map(score => ({ score })))).toEqual(scores.map(() => null));
		}
		expect(strengths([])).toEqual([]);
		const title = scoreTitle(0, null, 0);
		expect(title).toContain("Score 0");
		expect(title).toContain("unavailable");
		expect(title).toContain("not confidence");
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
