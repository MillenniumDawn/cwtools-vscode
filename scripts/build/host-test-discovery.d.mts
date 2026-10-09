export function findUnassignedHostTests(
	sourceNames: string[],
	testConfigs: { files: string[] }[],
): string[];

export function findStaleHostTests(
	sourceNames: string[],
	testConfigs: { files: string[] }[],
): string[];

export function checkHostTestDiscovery(root?: string): Promise<void>;
