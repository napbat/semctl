import type { ReadableStream, ReadableStreamDefaultReader } from "node:stream/web";
import type { HookInvoker } from "./index";

const MAX_HOOK_OUTPUT_BYTES = 64 * 1024;

export interface HookChild {
	stdin: {
		write(data: string): number | Promise<number>;
		end(): number | Promise<number>;
	};
	stdout: ReadableStream<Uint8Array>;
	exited: Promise<number>;
	kill(): void;
}

export interface HookProcessRuntime {
	spawn(cwd: string): HookChild;
	scheduleTimeout(callback: () => void, timeoutMs: number): () => void;
}

const runtime: HookProcessRuntime = {
	spawn(cwd) {
		return Bun.spawn(["semctl", "hook"], {
			cwd,
			env: process.env,
			stdin: "pipe",
			stdout: "pipe",
			stderr: "ignore",
		});
	},
	scheduleTimeout(callback, timeoutMs) {
		// OMP can clear its managed timers before it calls session_shutdown.
		const timer = setTimeout(callback, timeoutMs);
		return () => clearTimeout(timer);
	},
};

async function readBounded(reader: ReadableStreamDefaultReader<Uint8Array>): Promise<string> {
	const chunks: Uint8Array[] = [];
	let total = 0;
	for (;;) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_HOOK_OUTPUT_BYTES) throw new Error("Hook output exceeds the limit");
		chunks.push(value);
	}

	const output = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		output.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return new TextDecoder().decode(output);
}

export function extractAdditionalContext(stdout: string): string | undefined {
	try {
		const parsed = JSON.parse(stdout) as {
			hookSpecificOutput?: { additionalContext?: unknown };
		};
		const context = parsed.hookSpecificOutput?.additionalContext;
		return typeof context === "string" && context.trim().length > 0 ? context : undefined;
	} catch {
		return undefined;
	}
}

export function createSemctlHookInvoker(processRuntime: HookProcessRuntime = runtime): HookInvoker {
	const cancellations = new Set<() => void>();

	return {
		async invoke(input, ctx, timeoutMs) {
			if (process.env.SEMCTX_HOOK_DISABLE !== undefined) return undefined;
			if (input.hook_event_name === "PreToolUse" && process.env.SEMCTX_NUDGE_DISABLE !== undefined) {
				return undefined;
			}

			let child: HookChild | undefined;
			let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
			let cancelTimeout: (() => void) | undefined;
			let cancelled = false;
			const { promise: cancellation, resolve: resolveCancellation } = Promise.withResolvers<undefined>();
			const cancel = () => {
				if (cancelled) return;
				cancelled = true;
				resolveCancellation(undefined);
				try {
					child?.kill();
				} catch {
					// Cleanup is best effort because the hook must not stop the OMP session.
				}
				try {
					// A stream can keep cancellation pending after the child is killed.
					const streamCancellation = reader === undefined ? child?.stdout.cancel() : reader.cancel();
					// Stream cleanup is best effort and must not delay the hook result.
					void streamCancellation?.catch(() => {});
				} catch {
					// A broken stream must not stop child cleanup or the OMP session.
				}
			};
			try {
				child = processRuntime.spawn(ctx.cwd);
				// Observe exit errors even if stdin fails before the result is awaited.
				void child.exited.catch(() => {});
				cancellations.add(cancel);
				cancelTimeout = processRuntime.scheduleTimeout(cancel, timeoutMs);
				const stdin = child.stdin;
				const written = Promise.resolve(stdin.write(JSON.stringify(input))).then(() => {
					if (!cancelled) return stdin.end();
				});
				reader = child.stdout.getReader();
				const result = await Promise.race([
					Promise.all([readBounded(reader), child.exited, written]),
					cancellation,
				]);
				if (cancelled || result === undefined) return undefined;
				const [stdout, exitCode] = result;
				return exitCode === 0 ? extractAdditionalContext(stdout) : undefined;
			} catch {
				cancel();
				return undefined;
			} finally {
				cancelTimeout?.();
				reader?.releaseLock();
				cancellations.delete(cancel);
			}
		},
		shutdown() {
			for (const cancel of cancellations) cancel();
			cancellations.clear();
		},
	};
}
