from __future__ import annotations

import unittest

from load import load_build

coverage_metrics = load_build("coverage_metrics")
coverage_summary = load_build("coverage_summary")


def metric(total: int) -> dict[str, int]:
    return {"total": total, "covered": total, "pct": 100}


def file_coverage(
    totals: dict[str, int] | None = None,
) -> dict[str, dict[str, int]]:
    values = totals or {
        "lines": 1,
        "statements": 1,
        "branches": 1,
        "functions": 1,
    }
    return {
        "lines": metric(values["lines"]),
        "statements": metric(values["statements"]),
        "branches": metric(values["branches"]),
        "functions": metric(values["functions"]),
    }


class ValidateHostCoverageSummaryTests(unittest.TestCase):
    def test_accepts_non_empty_host_and_common_source_coverage(self) -> None:
        coverage_metrics.validate_host_coverage_summary(
            {
                "total": file_coverage(),
                "/repo/extension/src/host/fileExplorer.ts": file_coverage(),
                r"C:\repo\extension\src\common\graphTypes.ts": file_coverage(),
            }
        )

    def test_rejects_a_report_containing_only_dependencies(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError,
            "no files under extension/src/host or extension/src/common",
        ):
            coverage_metrics.validate_host_coverage_summary(
                {
                    "total": file_coverage(),
                    "/repo/node_modules/example/index.js": file_coverage(),
                }
            )

    def test_rejects_a_zero_metric_total(self) -> None:
        for metric_name in ("lines", "statements", "branches", "functions"):
            with self.subTest(metric_name):
                totals = {
                    "lines": 1,
                    "statements": 1,
                    "branches": 1,
                    "functions": 1,
                    metric_name: 0,
                }
                with self.assertRaisesRegex(
                    RuntimeError, f"host coverage has a zero {metric_name} total"
                ):
                    coverage_metrics.validate_host_coverage_summary(
                        {
                            "total": file_coverage(),
                            "/repo/extension/src/host/fileExplorer.ts": file_coverage(
                                totals
                            ),
                        }
                    )


class RenderCoverageSectionTests(unittest.TestCase):
    def test_renders_the_measured_label_and_source_scope(self) -> None:
        source = coverage_summary.CoverageSource(
            title="extension-host client",
            path="coverage/coverage-summary.json",
            html_artifact="coverage-html",
            labels=("unit",),
            scopes=("extension/src/host", "extension/src/common"),
            scope_note="Modules measured only by Vitest are excluded.",
        )
        rendered = "\n".join(
            coverage_summary.render_coverage_section(
                source,
                {
                    "total": file_coverage(),
                    "/repo/extension/src/host/fileExplorer.ts": file_coverage(),
                    "/repo/node_modules/example/index.js": file_coverage(),
                },
                "/repo",
            )
        )
        self.assertIn("Measured labels: `unit`", rendered)
        self.assertIn(
            "Source scope: `extension/src/host`, `extension/src/common`",
            rendered,
        )
        self.assertIn("extension/src/host/fileExplorer.ts", rendered)
        self.assertNotIn("node_modules", rendered)

    def test_rejects_an_empty_filtered_report(self) -> None:
        source = coverage_summary.CoverageSource(
            title="extension-host client",
            path="coverage/coverage-summary.json",
            html_artifact="coverage-html",
            labels=("unit",),
            scopes=("extension/src/host", "extension/src/common"),
        )
        with self.assertRaisesRegex(RuntimeError, "no source files"):
            coverage_summary.render_coverage_section(
                source,
                {
                    "total": file_coverage(),
                    "/repo/node_modules/example/index.js": file_coverage(),
                },
            )

    def test_lists_the_worst_covered_rust_files(self) -> None:
        rust = coverage_summary.CoverageSource(
            title="rust engine",
            path="engine/target/coverage/coverage-summary.json",
            html_artifact="rust-coverage",
            max_files=2,
            required_metrics=("lines", "statements", "functions"),
        )

        def file(pct: int) -> dict[str, dict[str, float | int]]:
            return {
                "lines": {"total": 10, "covered": pct / 10, "pct": pct},
                "statements": {"total": 10, "covered": pct / 10, "pct": pct},
                "branches": {"total": 0, "covered": 0, "pct": 0},
                "functions": {"total": 1, "covered": 1, "pct": 100},
            }

        rendered = "\n".join(
            coverage_summary.render_coverage_section(
                rust,
                {
                    "total": file_coverage(),
                    "engine/crates/a.rs": file(10),
                    "engine/crates/b.rs": file(20),
                    "engine/crates/c.rs": file(90),
                },
                "/repo",
            )
        )
        self.assertIn("engine/crates/a.rs", rendered)
        self.assertIn("engine/crates/b.rs", rendered)
        self.assertNotIn("engine/crates/c.rs", rendered)
        self.assertIn("Showing the 2 worst-covered files of 3", rendered)
        self.assertIn("Full report is in the `rust-coverage` artifact", rendered)


if __name__ == "__main__":
    unittest.main()
