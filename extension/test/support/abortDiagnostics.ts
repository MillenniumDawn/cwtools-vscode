// Says where a run stopped when the extension host goes away before mocha
// reports (#216). Loaded as a spec file, not via mocha's `require`, so the tdd
// globals are bound and the hooks below register as root hooks. #216 has been
// seen in the gap between two spec files, so the last completed test is
// tracked as well as the in-flight one.
export {};

let inFlight: string | undefined;
let lastCompleted: string | undefined;
let mochaFinished = false;

setup(function () {
	inFlight = this.currentTest?.fullTitle();
});

teardown(function () {
	lastCompleted = inFlight ?? lastCompleted;
	inFlight = undefined;
});

suiteTeardown(function () {
	mochaFinished = true;
});

process.on("exit", (code) => {
	if (mochaFinished) {
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
