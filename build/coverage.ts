export const HOST_COVERAGE_LABELS = ["unit"] as const;
export const HOST_COVERAGE_SCOPES = [
	"extension/src/host",
	"extension/src/common",
] as const;
export const HOST_COVERAGE_DROPS = [
	"extension/src/host/engine.ts",
	"extension/src/host/executable.ts",
	"extension/src/host/games.ts",
	"extension/src/host/rulesManifest.ts",
	"extension/src/host/rulesSetup.ts",
] as const;

export const COVERAGE_METRICS = [
	"lines",
	"statements",
	"branches",
	"functions",
] as const;

export type CoverageMetricName = (typeof COVERAGE_METRICS)[number];

export interface CoverageMetric {
	pct?: number;
	covered?: number;
	total?: number;
}

export interface FileCoverage {
	statements: CoverageMetric;
	branches: CoverageMetric;
	functions: CoverageMetric;
	lines: CoverageMetric;
}

export type CoverageSummary = { total: FileCoverage } & Record<
	string,
	FileCoverage
>;

const isRecord = (value: unknown): value is Record<string, unknown> =>
	typeof value === "object" && value !== null;

export const isHostSource = (path: string): boolean => {
	const normalized = path.split("\\").join("/");
	return HOST_COVERAGE_SCOPES.some((scope) =>
		normalized.includes(`/${scope}/`),
	);
};

function metricTotal(
	file: string,
	coverage: unknown,
	metric: CoverageMetricName,
): number {
	if (!isRecord(coverage) || !isRecord(coverage[metric])) {
		throw new Error(`coverage for ${file} has no ${metric} metric`);
	}
	const total = coverage[metric].total;
	if (typeof total !== "number" || !Number.isFinite(total) || total < 0) {
		throw new Error(`coverage for ${file} has an invalid ${metric} total`);
	}
	return total;
}

export function validateHostCoverageSummary(value: unknown): void {
	if (!isRecord(value)) {
		throw new Error("host coverage summary is not an object");
	}

	const files = Object.entries(value).filter(([file]) => isHostSource(file));
	if (files.length === 0) {
		throw new Error(
			`host coverage contains no files under ${HOST_COVERAGE_SCOPES.join(" or ")}`,
		);
	}

	for (const metric of COVERAGE_METRICS) {
		const total = files.reduce(
			(sum, [file, coverage]) => sum + metricTotal(file, coverage, metric),
			0,
		);
		if (total === 0) {
			throw new Error(`host coverage has a zero ${metric} total`);
		}
	}
}
