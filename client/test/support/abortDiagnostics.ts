/**
 * Says where a run stopped when the extension host goes away before mocha
 * reports (#216: no summary, host `exited with code: 0`, runner `Exit code: 1`).
 * Mocha's output ends at the last completed test and says nothing about whether
 * the shutdown came from inside the host, so the position has to be recorded as
 * the run goes.
 *
 * Loaded as a spec file so mocha's tdd globals are bound: a top-level setup /
 * teardown pair here is a root hook and runs around every test in the label.
 * The gap between two files is exactly where #216 has been seen, so the last
 * completed test is tracked as well as the in-flight one.
 */
export {};

let inFlight: string | undefined;
let lastCompleted: string | undefined;
let reported = false;

setup(function () {
	inFlight = this.currentTest?.fullTitle();
});

teardown(function () {
	lastCompleted = inFlight ?? lastCompleted;
	inFlight = undefined;
});

suiteTeardown(function () {
	reported = true;
});

process.on("exit", (code) => {
	if (reported) {
		return;
	}
	const where = inFlight
		? `during "${inFlight}"`
		: lastCompleted
			? `after "${lastCompleted}"`
			: "before the first test";
	console.error(
		`[abort] extension host exited with code ${code} ${where}; mocha never reported (see #216)`,
	);
});
