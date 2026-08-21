import { bench, describe } from "vitest";
import { fileListSignature } from "../../src/host/fileListSignature";
import { diagnosticsSignature } from "../../src/host/diagnosticsSignature";
import type { DiagnosticLike } from "../../src/host/diagnosticsSignature";
import { filesToTreeNodes } from "../../src/host/fileExplorer";
import type { FileListItem } from "../../src/host/fileExplorer";

// ~7,400 entries matches a Millennium-Dawn-scale updateFileList payload.
const FILE_COUNT = 7400;

const dirs = ["common/units", "common/decisions", "events", "history/countries", "gfx/interface", "localisation"];
const fileList: FileListItem[] = Array.from({ length: FILE_COUNT }, (_, i) => ({
	scope: i % 3 === 0 ? "vanilla" : "mod",
	uri: `file:///workspace/mod/${dirs[i % dirs.length]}/file_${i}.txt`,
	logicalpath: `${dirs[i % dirs.length]}/file_${i}.txt`,
}));

const diagnostics: DiagnosticLike[] = Array.from({ length: FILE_COUNT }, (_, i) => ({
	range: {
		start: { line: i % 500, character: i % 80 },
		end: { line: i % 500, character: (i % 80) + 10 },
	},
	severity: (i % 4) + 1,
	code: i % 2 === 0 ? `CW${i % 100}` : { value: i % 100 },
	message: `Expected localisation key event_${i}_desc to exist for language l_english`,
	source: "CWTools",
	relatedInformation: i % 10 === 0 ? [{}] : undefined,
}));

describe("client hot-path functions", () => {
	bench("fileListSignature (7,400 files)", () => {
		fileListSignature(fileList);
	});

	bench("diagnosticsSignature (7,400 diagnostics)", () => {
		diagnosticsSignature(diagnostics);
	});

	bench("filesToTreeNodes (7,400 files)", () => {
		filesToTreeNodes(fileList);
	});
});
