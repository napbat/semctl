import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { NativeChild } from "@oh-my-pi/pi-tui/native/node";
import type { TspCommonProps, TspKind } from "@oh-my-pi/pi-wire";

interface ElementProps extends TspCommonProps {
	tag: "div" | "span" | "section" | "button";
	class?: string;
	text?: string;
	attrs?: Partial<Record<`data-${string}` | `aria-${string}` | "role", string | number | boolean>>;
}
interface ElementDescription {
	k: "el";
	key?: string;
	p: ElementProps;
	c?: readonly NativeChild[];
}
export type NativeElement = NativeNode & { readonly p: ElementProps };

/** Tern advertises el independently of the older OMP SDK's closed kind union. */
export function supportsElements(cx: DescribeContext): boolean {
	return cx.supports("el" as TspKind);
}

/** The reconciler forwards advertised native kinds without interpreting their props. */
export function nativeElement(node: ElementDescription): NativeElement {
	return node as unknown as NativeElement;
}

export function asNativeElement(node: NativeNode): NativeElement | undefined {
	return (node.k as string) === "el" ? node as NativeElement : undefined;
}
