const CI_FALSE_VALUES = new Set(["", "0", "false", "no", "off"]);

function isCi(env) {
	return ["CI", "GITHUB_ACTIONS"].some((key) =>
		!CI_FALSE_VALUES.has((env[key] ?? "").trim().toLowerCase()),
	);
}

export function hostTestLaunchArgs(env = process.env) {
	if (env.CWTOOLS_TEST_CONFIG_DISCOVERY === "1") return [];
	if (env.CWTOOLS_TEST_LAUNCHER !== "hosttest") {
		throw new Error(
			"Extension-host tests must be launched through scripts/build/hosttest.py so they cannot open a desktop window.",
		);
	}

	const display = env.CWTOOLS_TEST_DISPLAY;
	if (display === "xvfb" || display === "ozone") {
		if (process.platform !== "linux") {
			throw new Error(`The ${display} display backend is only supported on Linux.`);
		}
		return display === "xvfb"
			? ["--ozone-platform=x11"]
			: ["--ozone-platform=headless"];
	}
	if (display === "native") {
		if (env.CWTOOLS_TEST_NATIVE_DEBUG !== "1") {
			throw new Error(
				"The native display is available only through the explicit --native debugging option.",
			);
		}
		if (isCi(env)) {
			throw new Error("Native extension-host tests are disabled in CI.");
		}
		return [];
	}
	throw new Error(
		`Unknown or missing extension-host display backend: ${String(display)}`,
	);
}
