import { describe, expect, test } from "vitest";
import { graphTruncationNotice } from "../../src/common/graphTypes";

describe("graph truncation notice", () => {
	test("shows a banner only when server truncation metadata is present", () => {
		expect(graphTruncationNotice([{ id: "a", references: [], isPrimary: true, entityType: "focus" }])).toBeUndefined();
		expect(graphTruncationNotice([{
			id: "a",
			references: [],
			isPrimary: true,
			entityType: "focus",
			details: [{ key: "truncated", values: ["showing 1 of 2 nodes"] }],
		}])).toBe("This graph was capped; some nodes or connections are omitted.");
	});
});
