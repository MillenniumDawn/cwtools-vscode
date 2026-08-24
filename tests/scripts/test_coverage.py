from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

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
            title="Extension-host client",
            path="coverage/coverage-summary.json",
            html_artifact="coverage-html",
            labels=("unit",),
            scopes=("extension/src/host", "extension/src/common"),
            scope_note="Modules measured only by Vitest are excluded.",
        )
        section = coverage_summary.render_coverage_section(
            source,
            {
                "total": file_coverage(),
                "/repo/extension/src/host/fileExplorer.ts": file_coverage(),
                "/repo/node_modules/example/index.js": file_coverage(),
            },
            "/repo",
        )
        rendered = "\n".join(section.details)
        self.assertIn("Measured labels: `unit`", rendered)
        self.assertIn(
            "Source scope: `extension/src/host`, `extension/src/common`",
            rendered,
        )
        self.assertIn("extension/src/host/fileExplorer.ts", rendered)
        self.assertNotIn("node_modules", rendered)

    def test_overview_uses_weighted_filtered_totals(self) -> None:
        def coverage(total: int, covered: int) -> dict[str, dict[str, float | int]]:
            pct = 100 * covered / total
            return {
                "lines": {"total": total, "covered": covered, "pct": pct},
                "statements": {"total": total, "covered": covered, "pct": pct},
                "branches": {"total": 1, "covered": 1, "pct": 100},
                "functions": {"total": 1, "covered": 1, "pct": 100},
            }

        section = coverage_summary.render_coverage_section(
            coverage_summary.CoverageSource(
                title="Extension-host client",
                path="host.json",
                html_artifact="coverage-html",
                scopes=("extension/src/host",),
            ),
            {
                "/repo/extension/src/host/small.ts": coverage(1, 1),
                "/repo/extension/src/host/large.ts": coverage(9, 0),
                "/repo/node_modules/example/index.js": coverage(90, 90),
            },
            "/repo",
        )

        self.assertEqual(
            section.overview,
            "| **Extension-host client** | 🔴 10.0 | 🔴 10.0 "
            "| 🟢 100.0 | 🟢 100.0 |",
        )

    def test_renders_one_overview_with_collapsed_file_sections(self) -> None:
        rust = coverage_summary.render_coverage_section(
            coverage_summary.CoverageSource(
                title="Rust engine",
                path="rust.json",
                html_artifact="rust-coverage",
            ),
            {"engine/crates/parser.rs": file_coverage()},
            "/repo",
        )
        node = coverage_summary.render_coverage_section(
            coverage_summary.CoverageSource(
                title="Node unit",
                path="node.json",
                html_artifact="coverage-node",
            ),
            {"extension/src/host/engine.ts": file_coverage()},
            "/repo",
        )

        rendered = coverage_summary.render_coverage_report((rust, node))
        overview, details = rendered.split("<details>", 1)

        self.assertEqual(rendered.count("## Coverage"), 1)
        self.assertIn("| **Rust engine** |", overview)
        self.assertIn("| **Node unit** |", overview)
        self.assertNotIn("| File |", overview)
        self.assertIn("engine/crates/parser.rs", details)
        self.assertIn("extension/src/host/engine.ts", details)
        self.assertEqual(rendered.count("<details>"), 2)
        self.assertEqual(rendered.count("</details>"), 2)
        self.assertNotIn("**All files**", rendered)

    def test_summary_skips_missing_sources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "coverage-summary.json"
            summary.write_text(
                json.dumps({"extension/src/host/engine.ts": file_coverage()}),
                encoding="utf-8",
            )
            rendered = coverage_summary.render_coverage_summary(
                (
                    coverage_summary.CoverageSource(
                        title="Missing suite",
                        path=str(Path(directory) / "missing.json"),
                        html_artifact="missing",
                    ),
                    coverage_summary.CoverageSource(
                        title="Node unit",
                        path=str(summary),
                        html_artifact="coverage-node",
                    ),
                )
            )

        self.assertIn("| **Node unit** |", rendered)
        self.assertNotIn("Missing suite", rendered)
        self.assertEqual(rendered.count("<details>"), 1)

    def test_rejects_an_empty_filtered_report(self) -> None:
        source = coverage_summary.CoverageSource(
            title="Extension-host client",
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
            title="Rust engine",
            path="engine/target/coverage/coverage-summary.json",
            html_artifact="rust-coverage",
            max_files=2,
            required_metrics=("lines", "statements", "functions"),
        )

        def file(pct: int) -> dict[str, dict[str, float | int | None]]:
            return {
                "lines": {"total": 10, "covered": pct // 10, "pct": pct},
                "statements": {"total": 10, "covered": pct // 10, "pct": pct},
                "branches": {"total": 0, "covered": 0, "pct": None},
                "functions": {"total": 1, "covered": 1, "pct": 100},
            }

        section = coverage_summary.render_coverage_section(
            rust,
            {
                "total": file_coverage(),
                "engine/crates/a.rs": file(10),
                "engine/crates/b.rs": file(20),
                "engine/crates/c.rs": file(90),
            },
            "/repo",
        )
        rendered = "\n".join(section.details)
        self.assertIn("engine/crates/a.rs", rendered)
        self.assertIn("engine/crates/b.rs", rendered)
        self.assertNotIn("engine/crates/c.rs", rendered)
        self.assertIn("2 worst-covered files of 3", rendered)
        self.assertIn("Full report is in the `rust-coverage` artifact", rendered)


if __name__ == "__main__":
    unittest.main()
