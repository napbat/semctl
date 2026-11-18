// Category colors encode evidence-backed communities, never rank or confidence.
const light = ["#526fbd", "#298174", "#925bb2", "#b6792b", "#22849d", "#b05277", "#6b8935", "#b36042", "#6961a9", "#7c704b"];
const dark = ["#8ca8ef", "#63bca5", "#bd8cda", "#d6ab62", "#68b9d1", "#dc8dab", "#a2bc6a", "#dc9b7a", "#aaa0df", "#bcb081"];
export const conceptSwatches = Object.freeze(light.map((color, index) => Object.freeze({ light: color, dark: dark[index] })));
export function conceptPalette(key) {
	let hash = 2166136261;
	for (let index = 0; index < key.length; index++) hash = Math.imul(hash ^ key.charCodeAt(index), 16777619);
	const index = (hash >>> 0) % light.length;
	return { light: light[index], dark: dark[index] };
}
export function normalizeConceptPath(path) {
	if (typeof path !== "string" || !path || path.length > 4096 || /[:\u0000-\u001f\u007f-\u009f]/.test(path)) return undefined;
	const normalized = path.replace(/\\/g, "/").replace(/^(\.\/)+/, "");
	return normalized && !normalized.split("/").some(part => !part || part === "." || part === "..") ? normalized : undefined;
}
