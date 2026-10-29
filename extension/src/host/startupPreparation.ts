import * as os from "os";
import * as fsPromises from "fs/promises";
import { logError } from "./logger";
import type { RulesSetup } from "./rulesSetup";

// Never rejects: Windows has no exec bit, and stat/chmod failures are logged, not fatal. (#1155)
async function prepareServerBinary(serverExe: string): Promise<void> {
	if (os.platform() === "win32") {
		return;
	}
	try {
		const stat = await fsPromises.stat(serverExe);
		if ((stat.mode & 0o111) === 0) {
			await fsPromises.chmod(serverExe, 0o755);
		}
	} catch (e) {
		logError("stat/chmod error on server binary", e);
	}
}

// Both preparations share one combined wait, so neither rejection is left unhandled. (#1155)
export async function prepareClientStart(
	serverExe: string,
	resolveRules: () => Promise<RulesSetup>,
): Promise<RulesSetup> {
	const rulesSetup = resolveRules();
	const binaryPrepared = prepareServerBinary(serverExe);
	const [setup] = await Promise.all([rulesSetup, binaryPrepared]);
	return setup;
}