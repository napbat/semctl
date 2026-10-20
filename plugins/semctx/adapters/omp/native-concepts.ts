import type { DescribeContext } from "@oh-my-pi/pi-tui";
import type { NativeStylesheet } from "./native-surface";
import type { ConceptColor } from "./concept-colors";
import type { GraphColor } from "./graph-colors";
import type { DocConceptVia } from "./graph-data/doc-concepts.js";
import { conceptSwatches } from "./graph-data/palette.js";
import { nativeElement, supportsElements, type NativeElement } from "./native-elements";
import { compactTooltip } from "./tooltip-text";

const VIA_PHRASES: Record<DocConceptVia, string> = { doc: "the doc itself", mention: "mentioned in the doc", reference: "linked by references", folder: "same folder" };

/** A group color has no `via`, so its second line names only the doc. */
function conceptTitle(concept: ConceptColor): string {
	if (concept.kind === "reference") return compactTooltip(`Inferred concept: ${concept.label}`, "Reference/import grouping, not confidence or similarity");
	return compactTooltip(`Concept: ${concept.title ?? concept.label}`,
		[concept.source && `From ${concept.source}`, concept.via && VIA_PHRASES[concept.via]].filter(Boolean).join(" · "));
}

export function conceptClass(color: Pick<ConceptColor, "light">, dark: boolean): string {
	return `semctl-concept-${color.light.slice(1)}${dark ? " semctl-concept-dark" : ""}`;
}

export const CONCEPT_STYLES: readonly NativeStylesheet[] = [{ name: "semctl-concepts", css: [
	...conceptSwatches.map(color => `.semctl-concept-${color.light.slice(1)}{color:${color.light}}.semctl-concept-${color.light.slice(1)}.semctl-concept-dark{color:${color.dark}}`),
	`.semctl-concept-mark{display:inline-block;position:relative;width:8px;height:12px;flex-shrink:0}`,
	`.semctl-concept-mark::before{content:"";position:absolute;left:2px;top:1px;width:4px;height:10px;border-radius:2px;background:currentColor}`,
	`.semctl-concept-disclosure::before{left:2px;top:2px;width:0;height:0;border-radius:0;background:transparent;border-top:4px solid transparent;border-bottom:4px solid transparent;border-left:5px solid currentColor;transform-origin:2px 4px;transition:transform 140ms ease-out}`,
	`.semctl-concept-expanded::before{transform:rotate(90deg)}`,
	`.semctl-concept-still::before{transition:none}`,
	`@media (prefers-reduced-motion:reduce){.semctl-concept-mark::before{transition:none}}`,
].join("\n") }];

/** One renderer-owned category mark. The parent row owns selection and expansion. */
export function nativeConceptCue(concept: ConceptColor, cx: DescribeContext, expanded?: boolean, key = "concept-cue"): NativeElement | undefined {
	if (!supportsElements(cx) || !cx.feature("styles")) return undefined;
	const title = conceptTitle(concept);
	return nativeElement({ k: "el", key, p: { tag: "span",
		class: `semctl-concept-mark ${conceptClass(concept, cx.dark)}${expanded === undefined ? "" : " semctl-concept-disclosure"}${expanded ? " semctl-concept-expanded" : ""}${cx.reduceMotion ? " semctl-concept-still" : ""}`,
		title, aria: title, shrink: 0,
	} });
}

/** Shared graph categories distinguish verified concepts from truthful structural fallback. */
export function nativeGraphColorCue(color: GraphColor, cx: DescribeContext, expanded?: boolean, key = "graph-color-cue"): NativeElement | undefined {
	if (!supportsElements(cx) || !cx.feature("styles")) return undefined;
	const title = color.concept ? conceptTitle(color.concept) : compactTooltip(`Code structure: ${color.label}`, "Source-directory color");
	return nativeElement({ k: "el", key, p: { tag: "span",
		class: `semctl-concept-mark ${conceptClass(color, cx.dark)}${expanded === undefined ? "" : " semctl-concept-disclosure"}${expanded ? " semctl-concept-expanded" : ""}${cx.reduceMotion ? " semctl-concept-still" : ""}`,
		title, aria: title, shrink: 0,
	} });
}
