"""Supplementary Linux CSV check; build cwtools_cli before running with pytest."""

import csv
import io
import json
import subprocess
import tempfile
from pathlib import Path


def test_cli_csv_rows_round_trip_for_special_filenames() -> None:
    repository = Path(__file__).resolve().parents[4]
    binary = repository / "engine" / "target" / "debug" / "cwtools"
    rules = repository / "engine" / "crates" / "cli" / "tests" / "fixtures" / "rules"
    names = [
        "plain",
        "carriage\rreturn",
        "line\nfeed",
        "line\r\nending",
        'quote"name',
        "comma,name",
    ]

    with tempfile.TemporaryDirectory() as temporary_directory:
        mod = Path(temporary_directory)
        common = mod / "common"
        common.mkdir()
        for name in names:
            (common / f"{name}.txt").write_text("broken = {", encoding="utf-8")

        reports = {}
        for report_type in ["csv", "json"]:
            result = subprocess.run(
                [
                    str(binary),
                    "validate",
                    "--game",
                    "stellaris",
                    "--directory",
                    str(mod),
                    "--rules",
                    str(rules),
                    "--report-type",
                    report_type,
                    "--fail-on",
                    "none",
                ],
                check=True,
                capture_output=True,
            )
            reports[report_type] = result.stdout.decode("utf-8")

    csv_rows = list(csv.DictReader(io.StringIO(reports["csv"], newline="")))
    json_rows = json.loads(reports["json"])
    actual_csv = [
        {
            field: row[field]
            for field in ("file", "line", "severity", "code", "message", "hash")
        }
        for row in csv_rows
    ]
    expected_csv = [
        {
            field: str(row[field])
            for field in ("file", "line", "severity", "code", "message", "hash")
        }
        for row in json_rows
    ]
    assert actual_csv == expected_csv
    expected_names = {str(mod / "common" / f"{name}.txt") for name in names}
    assert {row["file"] for row in json_rows} == expected_names
