// A local model endpoint that replays a fixed script, so a real coding-agent
// host runs a deterministic session without a network model.

/** The model API that a host speaks. */
export type WireFormat = "openai-chat" | "anthropic-messages" | "openai-responses";

export interface ScriptedToolCall {
	name: string;
	arguments: Record<string, unknown>;
}

/** One assistant reply: a single tool call, or final text. */
export type ScriptedTurn = { toolCall: ScriptedToolCall } | { text: string };

/** The subset of a model request body that selects the next scripted turn. */
interface ModelRequest {
	stream?: boolean;
	tools?: unknown[];
	messages?: Array<{ role?: string }>;
	input?: Array<{ type?: string }>;
}

export interface ScriptedModel {
	/** The API base URL, including `/v1`. */
	readonly baseUrl: string;
	/** Raw bodies of the agent-loop requests since the last `script` call. */
	readonly requests: string[];
	/** Replace the script. Replies follow `turns` in order, then end with text. */
	script(turns: ScriptedTurn[]): void;
	stop(): void;
}

const CALL_ID = "call_semctx_0";

/** Encode server-sent events. OpenAI chat streams end with a `[DONE]` sentinel. */
function sse(events: Array<{ event?: string; data: unknown }>, terminator = ""): Response {
	const body = events
		.map(({ event, data }) => `${event === undefined ? "" : `event: ${event}\n`}data: ${JSON.stringify(data)}\n\n`)
		.join("");
	return new Response(body + terminator, { headers: { "content-type": "text/event-stream" } });
}

function openAiChat(turn: ScriptedTurn): Response {
	const chunk = (delta: Record<string, unknown>, finishReason: string | null = null) => ({
		data: {
			id: "scripted",
			object: "chat.completion.chunk",
			created: 0,
			model: "scripted",
			choices: [{ index: 0, delta, finish_reason: finishReason }],
		},
	});
	const events =
		"text" in turn
			? [chunk({ role: "assistant", content: turn.text }), chunk({}, "stop")]
			: [
					chunk({
						role: "assistant",
						tool_calls: [
							{
								index: 0,
								id: CALL_ID,
								type: "function",
								function: { name: turn.toolCall.name, arguments: JSON.stringify(turn.toolCall.arguments) },
							},
						],
					}),
					chunk({}, "tool_calls"),
				];
	return sse(events, "data: [DONE]\n\n");
}

function anthropicMessages(turn: ScriptedTurn): Response {
	const event = (type: string, data: Record<string, unknown>) => ({ event: type, data: { type, ...data } });
	const block =
		"text" in turn
			? [
					event("content_block_start", { index: 0, content_block: { type: "text", text: "" } }),
					event("content_block_delta", { index: 0, delta: { type: "text_delta", text: turn.text } }),
				]
			: [
					event("content_block_start", {
						index: 0,
						content_block: { type: "tool_use", id: CALL_ID, name: turn.toolCall.name, input: {} },
					}),
					event("content_block_delta", {
						index: 0,
						delta: { type: "input_json_delta", partial_json: JSON.stringify(turn.toolCall.arguments) },
					}),
				];
	return sse([
		event("message_start", {
			message: {
				id: "msg_scripted",
				type: "message",
				role: "assistant",
				model: "scripted",
				content: [],
				stop_reason: null,
				usage: { input_tokens: 1, output_tokens: 0 },
			},
		}),
		...block,
		event("content_block_stop", { index: 0 }),
		event("message_delta", {
			delta: { stop_reason: "text" in turn ? "end_turn" : "tool_use" },
			usage: { output_tokens: 1 },
		}),
		event("message_stop", {}),
	]);
}

function openAiResponses(turn: ScriptedTurn): Response {
	const item =
		"text" in turn
			? {
					type: "message",
					id: "msg_scripted",
					role: "assistant",
					status: "completed",
					content: [{ type: "output_text", text: turn.text, annotations: [] }],
				}
			: {
					type: "function_call",
					id: "fc_scripted",
					call_id: CALL_ID,
					name: turn.toolCall.name,
					arguments: JSON.stringify(turn.toolCall.arguments),
					status: "completed",
				};
	const response = {
		id: "resp_scripted",
		object: "response",
		status: "completed",
		output: [item],
		usage: {
			input_tokens: 1,
			output_tokens: 1,
			total_tokens: 2,
			input_tokens_details: { cached_tokens: 0 },
			output_tokens_details: { reasoning_tokens: 0 },
		},
	};
	const event = (type: string, data: Record<string, unknown>) => ({ event: type, data: { type, ...data } });
	return sse([
		event("response.created", { response: { ...response, status: "in_progress", output: [] } }),
		event("response.output_item.added", { output_index: 0, item }),
		event("response.output_item.done", { output_index: 0, item }),
		event("response.completed", { response }),
	]);
}

const ENCODERS: Record<WireFormat, (turn: ScriptedTurn) => Response> = {
	"openai-chat": openAiChat,
	"anthropic-messages": anthropicMessages,
	"openai-responses": openAiResponses,
};

/** The number of assistant tool rounds already in the request history. */
function completedRounds(wire: WireFormat, request: ModelRequest): number {
	if (wire === "openai-responses") {
		return (request.input ?? []).filter(item => item.type === "function_call").length;
	}
	return (request.messages ?? []).filter(message => message.role === "assistant").length;
}

export function startScriptedModel(wire: WireFormat): ScriptedModel {
	let turns: ScriptedTurn[] = [];
	const requests: string[] = [];
	const server = Bun.serve({
		hostname: "127.0.0.1",
		port: 0,
		async fetch(httpRequest) {
			if (httpRequest.method !== "POST") {
				return Response.json({ object: "list", data: [{ id: "scripted", object: "model" }], models: [] });
			}
			if (new URL(httpRequest.url).pathname.endsWith("/count_tokens")) {
				return Response.json({ input_tokens: 1 });
			}
			const raw = await httpRequest.text();
			const request: ModelRequest = raw.length === 0 ? {} : JSON.parse(raw);
			// Hosts also send side requests (titles, quota probes) without tools.
			// Those are not agent-loop turns, so they do not advance the script.
			if ((request.tools ?? []).length === 0) {
				if (wire === "anthropic-messages" && request.stream !== true) {
					return Response.json({
						id: "msg_side",
						type: "message",
						role: "assistant",
						model: "scripted",
						content: [{ type: "text", text: "ok" }],
						stop_reason: "end_turn",
						usage: { input_tokens: 1, output_tokens: 1 },
					});
				}
				return ENCODERS[wire]({ text: "ok" });
			}
			requests.push(raw);
			return ENCODERS[wire](turns[completedRounds(wire, request)] ?? { text: "done" });
		},
	});
	return {
		baseUrl: `http://127.0.0.1:${server.port}/v1`,
		requests,
		script(next) {
			turns = next;
			requests.length = 0;
		},
		stop() {
			server.stop(true);
		},
	};
}
