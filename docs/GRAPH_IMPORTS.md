# Imported graph metadata

CWTools accepts generic Cytoscape JSON nodes without CWTools metadata. Nodes
without a file location can be hovered, but double-tapping them does not open
a file.

Optional `data.entityTypeDisplayName` must be a string. Optional `data.details` must be an array of entries with a string `key` and a
`values` array of strings. Optional `data.location` must contain a nonempty
string `filename`, a safe integer `line` of at least 1, and a safe integer
`column` of at least 0. Column 0 remains accepted for existing graph exports.

Malformed metadata is rejected before replacing the current graph or changing
its persisted source. The error names the node and the invalid field. The
navigation interaction also checks its coordinates before forwarding them,
and tooltip detail rendering omits malformed entries if data changes later.

`graphImportMetadata.test.ts` exercises the real dispatcher and headless
Cytoscape: invalid/null details and display names, reserved-field validation bypasses,
invalid coordinates, location-free hover and
navigation, and round-trip imports from a real Cytoscape export.
