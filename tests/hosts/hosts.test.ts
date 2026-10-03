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
				expect(session.trace.find(record => record.event === "SessionStart")?.context).toBeTrue();
				expect(toolDecisions(session)).toEqual(["unavailable"]);

				const [manualLead] = fs.readFileSync(SERVER_MANUAL, "utf8").split("\n");
				expect(session.requests.length).toBeGreaterThanOrEqual(2);
				expect(session.requests[0]).toContain(JSON.stringify(manualLead).slice(1, -1));
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
