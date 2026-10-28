import { afterAll, afterEach, beforeAll, expect, test, vi } from "vitest";
import { installGraph, node, render, resetGraphState, send, state } from "./support/graphHeadless";

beforeAll(installGraph);
afterEach(() => { vi.restoreAllMocks(); });
afterAll(() => { state.cores.forEach((core) => core.destroy()); vi.unstubAllGlobals(); });

async function flushExport() {
	await new Promise<void>((resolve) => setImmediate(resolve));
}

test.each([
	["image merge", "merge failed", () => { state.mergeImages.mockRejectedValueOnce(new Error("merge failed")); }],
	["blob read", "read failed", () => { state.fileReaderFailure = new Error("read failed"); }],
])("reports %s export failures through the message dispatcher and allows retry", async (_kind, errorMessage, fail) => {
	resetGraphState();
	const cy = render([node("a")]);
	// base64uri returns a string. spyOn picks the blob-promise overload.
	vi.spyOn(cy, "png").mockReturnValue("data:image/png;base64,cG5n" as never);
	fail();

	const unhandled: unknown[] = [];
	const onUnhandled = (reason: unknown) => { unhandled.push(reason); };
	process.on("unhandledRejection", onUnhandled);
	try {
		send({ command: "exportImage" });
		await flushExport();

		expect(unhandled).toEqual([]);
		expect(state.postMessage).toHaveBeenCalledWith({
			command: "showError",
			message: `CWTools: couldn't export graph image: ${errorMessage}.`,
		});
		expect(state.postMessage).not.toHaveBeenCalledWith(expect.objectContaining({ command: "saveImage" }));

		state.fileReaderFailure = undefined;
		state.mergeImages.mockResolvedValueOnce("data:image/png;base64,QUJD");
		send({ command: "exportImage" });
		await flushExport();

		expect(unhandled).toEqual([]);
		expect(state.postMessage).toHaveBeenLastCalledWith({ command: "saveImage", image: "QUJD" });
	} finally {
		process.off("unhandledRejection", onUnhandled);
	}
});
