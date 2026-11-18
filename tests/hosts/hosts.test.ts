// Host integration tests: each scenario runs in every supported coding-agent
// host with the semctx plugin from this checkout and the real `semctl`.
//
// The tests are slow and need the host CLIs, Cargo, and Git, so they run only
// when SEMCTX_HOST_INTEGRATION=1. CI does not run them. SEMCTX_HOSTS selects
// hosts (for example `omp,claude`); a selected host that cannot run fails
// instead of skipping. See the README Development section.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { HostSuite, type HostSession, hostCommand } from "./harness";
import { HOST_ADAPTERS, REPO_ROOT, type SearchIntent } from "./hosts";

const ENABLED = process.env.SEMCTX_HOST_INTEGRATION === "1";
const SELECTED = process.env.SEMCTX_HOSTS?.split(",")
	.map(id => id.trim())
	.filter(id => id.length > 0);
const SESSION_TEST_TIMEOUT_MS = 120_000;
const SETUP_TIMEOUT_MS = 600_000;

const SERVER_MANUAL = path.join(REPO_ROOT, "src/mcp/docs/instructions/server.md");

/** The `semctl hook` outcome each intent must reach, or `null` for no reminder decision. */
const EXPECTED_OUTCOME: Record<SearchIntent, string | null> = {
	// Grace 0 makes a reminder due. Offline semctl reports it as unavailable.
	"broad-search": "unavailable",
	"single-file-search": "single_file",
	"subdirectory-shell-search": "single_file",
	"internal-url-search": null,
	"non-search": null,
};

function toolDecisions(session: HostSession): string[] {
	return session.trace
		.filter(record => record.event === "PreToolUse")
		.map(record => record.outcome ?? "")
		.filter(outcome => outcome !== "not_search");
}

for (const adapter of HOST_ADAPTERS) {
	const selected = SELECTED === undefined ? true : SELECTED.includes(adapter.id);
	const runnable = Bun.which(hostCommand(adapter)[0]) !== null;
	// Without SEMCTX_HOSTS, a host whose CLI is missing is skipped. With it, a
	// selected host always runs, so a missing CLI fails loudly.
	const skip = !ENABLED || !selected || (SELECTED === undefined && !runnable);

	describe.skipIf(skip)(`semctx plugin in ${adapter.id}`, () => {
		let suite: HostSuite;

		beforeAll(() => {
			suite = HostSuite.start(adapter);
		}, SETUP_TIMEOUT_MS);

		afterAll(() => {
			suite?.stop();
		});

		test(
			"session start delivers the semctl manual and a broad search reaches a reminder decision",
			async () => {
				const toolCall = adapter.toolCall("broad-search");
				if (toolCall === undefined) throw new Error(`${adapter.id} must map broad-search`);
				const session = await suite.run("list the rust files", [{ toolCall }, { text: "done" }]);
				expect(session.exitCode, session.output).toBe(0);

				const events = session.trace.map(record => record.event);
				expect(events).toContain("SessionStart");
				expect(events).toContain("UserPromptSubmit");
				for (const record of session.trace) expect(record.host).toBe(adapter.hookHost);
				// OMP can mount MCP after session_start. The prompt hook must
				// restore the manual before the first model request in that case.
				expect(
					session.trace.some(
						record => ["SessionStart", "UserPromptSubmit"].includes(record.event) && record.context,
					),
				).toBeTrue();
				expect(toolDecisions(session)).toEqual(["unavailable"]);

				const [manualLead] = fs.readFileSync(SERVER_MANUAL, "utf8").split("\n");
				expect(session.requests.length).toBeGreaterThanOrEqual(2);
				expect(session.requests[0]).toContain(JSON.stringify(manualLead).slice(1, -1));
			},
			SESSION_TEST_TIMEOUT_MS,
		);

		test.skipIf(adapter.id !== "omp")(
			"the local package exposes native MCP tools and returns a server error while logged out",
			async () => {
				const toolName = "mcp__semctx_current_context";
				const session = await suite.run("show the codebase context", [
					{
						toolCall: {
							name: "write",
							arguments: { path: `xd://${toolName}`, content: "{}", i: "Reading codebase context" },
						},
					},
					{ text: "done" },
				]);
				expect(session.exitCode, session.output).toBe(0);
				expect(session.requests.length, session.output).toBeGreaterThanOrEqual(2);
				const compliance = session.trace.filter(record => record.event === "PreToolUse" && record.tool === toolName);
				expect(compliance, session.output).toHaveLength(1);
				expect(compliance[0]).toMatchObject({ host: adapter.hookHost, outcome: "semctx_use" });

				// OMP mounts MCP tools as xd:// devices. The model must receive
				// those routes and the native write transport, not phantom functions.
				const firstRequest = JSON.parse(session.requests[0]);
				const names = firstRequest.tools.map((tool: { function: { name: string } }) => tool.function.name);
				expect(names, session.output).toContain("write");
				for (const name of ["current_context", "search_codebase", "find_definition", "replace_symbol_body"]) {
					expect(session.requests[0], session.output).toContain(`xd://mcp__semctx_${name}`);
				}

				const events = session.stdout
					.split("\n")
					.filter(line => line.trim().length > 0)
					.map(line => JSON.parse(line));
				const completed = events.filter(event => event.type === "tool_execution_end" && event.toolName === "write");
				expect(completed, session.output).toHaveLength(1);
				const [call] = completed;
				expect(call.isError).toBeTrue();
				const dispatch = call.result.details.xdev;
				expect(dispatch).toMatchObject({ tool: toolName, mode: "execute", args: {} });
				expect(dispatch.inner).toMatchObject({
					serverName: "semctx",
					mcpToolName: "current_context",
					isError: true,
				});

				// A missing tool or failed transport is a host error without raw
				// MCP content. Logged-out semctl instead returns isError content.
				// Compare that content with the next model request, not prose.
				const raw = dispatch.inner.rawContent;
				expect(Array.isArray(raw), session.output).toBeTrue();
				expect(raw.length).toBeGreaterThan(0);
				const nextRequest = JSON.parse(session.requests[1]);
				const results = nextRequest.messages.filter(
					(message: { role: string; tool_call_id?: string }) =>
						message.role === "tool" && message.tool_call_id === call.toolCallId,
				);
				expect(results).toHaveLength(1);
				for (const block of raw) {
					expect(block.type).toBe("text");
					expect(block.text.trim().length).toBeGreaterThan(0);
					expect(JSON.stringify(results[0].content)).toContain(JSON.stringify(block.text).slice(1, -1));
				}
			},
			SESSION_TEST_TIMEOUT_MS,
		);

		for (const intent of Object.keys(EXPECTED_OUTCOME) as SearchIntent[]) {
			if (intent === "broad-search") continue;
			const toolCall = adapter.toolCall(intent);
			test.skipIf(toolCall === undefined)(
				`${intent} reaches the expected hook decision`,
				async () => {
					if (toolCall === undefined) return;
					const session = await suite.run("search the code", [{ toolCall }, { text: "done" }]);
					expect(session.exitCode, session.output).toBe(0);
					// The SessionStart record proves the plugin hooks were active, and a
					// second model request proves the host ran the scripted call. Then
					// an absent decision cannot come from an unloaded plugin or a
					// rejected call.
					expect(session.trace.map(record => record.event), session.output).toContain("SessionStart");
					expect(session.requests.length, session.output).toBeGreaterThanOrEqual(2);
					const expected = EXPECTED_OUTCOME[intent];
					expect(toolDecisions(session)).toEqual(expected === null ? [] : [expected]);
				},
				SESSION_TEST_TIMEOUT_MS,
			);
		}
	});
}
