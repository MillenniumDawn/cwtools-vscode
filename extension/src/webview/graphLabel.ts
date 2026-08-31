export function deriveNodeLabel(
	entityType?: unknown,
	abbreviation?: unknown,
): string {
	if (typeof abbreviation === "string" && abbreviation.length > 0) {
		return abbreviation;
	}
	if (typeof entityType !== "string") {
		return "?";
	}

	const label = entityType
		.split("_")
		.filter((part) => part.length > 0)
		.map((part) => part.charAt(0).toUpperCase())
		.join("");
	return label || "?";
}
