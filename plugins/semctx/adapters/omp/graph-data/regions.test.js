import { describe, expect, test } from "bun:test";
import { homeFolder, mapRegions } from "./regions.js";

const vector = (...terms) => new Map(terms.map(term => [term, 1 / Math.sqrt(terms.length)]));

describe("map regions", () => {
	test("a home folder holds at least 60% of the member folders and prefers the deepest such folder", () => {
		expect(homeFolder(["hv/src/vtx", "hv/src/vtx", "hv/src/svm", "hv/include"])).toBe("hv/src");
		expect(homeFolder(["hv/src/vtx", "hv/src/vtx", "hv/src/vtx", "tools"])).toBe("hv/src/vtx");
		expect(homeFolder(["hv/src", "tools/lib", "docs"])).toBe("");
		expect(homeFolder([])).toBe("");
		expect(homeFolder(["hv/src", undefined, "hv/src"])).toBe("hv/src");
	});
	test("documentation families take precedence over folders, and folders over linked terms", () => {
		const architecture = { key: "family:docs/architecture", label: "Architecture" };
		const { regions, regionOf } = mapRegions([
			{ id: "dispatch", home: "hv/src", family: architecture, weight: 9 },
			{ id: "runtime", home: "uefi", family: architecture, weight: 8 },
			{ id: "vtx", home: "hv/src/vtx", weight: 5 },
			{ id: "svm", home: "hv/include/svm", weight: 4 },
			{ id: "lib", home: "tools/lib", weight: 3 },
			{ id: "rust", home: "tools/rust", weight: 3 },
			{ id: "probe", home: "", vector: vector("hwid", "probe"), weight: 2 },
			{ id: "posture", home: "", vector: vector("hwid", "posture"), weight: 2 },
			{ id: "alone", home: "cmake", weight: 1 },
		], [{ a: "probe", b: "posture", weight: 6 }]);
		expect(regions.map(region => [region.label, region.kind, region.members])).toEqual([
			["Architecture", "doc", ["dispatch", "runtime"]],
			["hv", "path", ["svm", "vtx"]],
			["tools", "path", ["lib", "rust"]],
			["hwid", "terms", ["posture", "probe"]],
		]);
		expect(regions.find(region => region.kind === "path")?.folder).toBe("hv");
		expect(regionOf.has("alone")).toBe(false);
	});
	test("a folder that would hold most groups is refined one level deeper", () => {
		const groups = ["src/query/a", "src/query/b", "src/engine/c", "src/engine/d", "src/sync/e"].map((home, index) => ({ id: `g${index}`, home }));
		expect(mapRegions(groups).regions.map(region => [region.label, region.members])).toEqual([
			["src/engine", ["g2", "g3"]],
			["src/query", ["g0", "g1"]],
		]);
	});
	test("linked groups without one shared term form no region", () => {
		const { regions } = mapRegions([{ id: "a", home: "", vector: vector("alpha") }, { id: "b", home: "", vector: vector("beta") }], [{ a: "a", b: "b", weight: 20 }]);
		expect(regions).toEqual([]);
	});
	test("the result is independent of input order and keeps at most the limit, largest first", () => {
		const groups = Array.from({ length: 8 }, (_, index) => [{ id: `${index}a`, home: `m${index}/x`, weight: index + 1 }, { id: `${index}b`, home: `m${index}/y`, weight: 1 }]).flat();
		const forward = mapRegions(groups, [], { limit: 3 }), reverse = mapRegions([...groups].reverse(), [], { limit: 3 });
		expect(forward.regions).toEqual(reverse.regions);
		expect(forward.regions.map(region => region.label)).toEqual(["m7", "m6", "m5"]);
		expect(forward.regionOf.has("0a")).toBe(false);
	});
});
