import { vi } from "vitest";

// Pass the spies a test asserts on; everything else gets a fresh no-op.
export function mockLogger(overrides: Record<string, unknown> = {}) {
	return {
		initializeLogger: vi.fn(),
		logInfo: vi.fn(),
		logWarn: vi.fn(),
		logError: vi.fn(),
		errorMessage: (err: unknown) =>
			err instanceof Error ? err.message : String(err),
		outputChannel: { appendLine: () => {} },
		...overrides,
	};
}
