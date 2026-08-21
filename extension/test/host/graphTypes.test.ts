import * as assert from "assert";
import sinon from "sinon";
import * as vscode from "vscode";
import type { GraphData, GraphNode } from "../../src/common/graphTypes";
import { getGraphData } from "../../src/common/graphTypes";

suite("graphTypes — getGraphData", () => {
	let executeCommandStub: sinon.SinonStub;

	setup(() => {
		executeCommandStub = sinon.stub(vscode.commands, "executeCommand");
	});

	teardown(() => {
		executeCommandStub.restore();
	});

	test("forwards entityType and depth to the getGraphData command", async () => {
		executeCommandStub.resolves([]);
		await getGraphData("technology", 3);
		assert.ok(executeCommandStub.calledOnce);
		const [command, entityType, depth] = executeCommandStub.firstCall.args as [
			string,
			string,
			number,
		];
		assert.strictEqual(command, "getGraphData");
		assert.strictEqual(entityType, "technology");
		assert.strictEqual(depth, 3);
	});

	test("returns the command result typed as GraphData", async () => {
		const node: GraphNode = {
			id: "tech_lasers",
			name: "Lasers",
			references: [{ key: "tech_lasers_2", isOutgoing: true, label: "prereq" }],
			isPrimary: true,
			entityType: "technology",
		};
		executeCommandStub.resolves([node]);
		const data = await getGraphData("technology", 1);
		assert.strictEqual(data.length, 1);
		assert.strictEqual(data[0].id, "tech_lasers");
		assert.strictEqual(data[0].references[0].key, "tech_lasers_2");
	});

	test("returns an empty array when the server has no data", async () => {
		executeCommandStub.resolves([]);
		const data: GraphData = await getGraphData("unknown", 2);
		assert.deepStrictEqual(data, []);
	});

	test("propagates errors from the command", async () => {
		executeCommandStub.rejects(new Error("server offline"));
		await assert.rejects(() => getGraphData("technology", 1), /server offline/);
	});
});
