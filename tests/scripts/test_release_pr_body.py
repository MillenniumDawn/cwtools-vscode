from __future__ import annotations

import json

import pytest

import release_pr_body
from release_pr_body import PullRequest

REPO = "MillenniumDawn/cwtools-vscode"


def _pull(
    number: int, title: str, login: str = "someone", ref: str = "fix/x"
) -> object:
    return {
        "number": number,
        "title": title,
        "user": {"login": login},
        "head": {"ref": ref},
    }


def _fake_run_capture(
    monkeypatch: pytest.MonkeyPatch,
    shas: list[str],
    pulls: dict[str, list[object] | None],
) -> list[list[str]]:
    calls: list[list[str]] = []

    def run_capture(cmd: str, args: list[str], **_: object) -> str:
        calls.append([cmd, *args])
        if cmd == "git" and args[0] == "describe":
            return "v3.4.7\n"
        if cmd == "git" and args[0] == "log":
            return "".join(f"{sha}\n" for sha in shas)
        sha = args[1].split("/")[-2]
        answer = pulls[sha]
        if answer is None:
            raise RuntimeError(f"command failed (1): gh {' '.join(args)}")
        return json.dumps(answer)

    monkeypatch.setattr(release_pr_body, "run_capture", run_capture)
    return calls


def test_pull_requests_come_oldest_first_and_once_each(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _fake_run_capture(
        monkeypatch,
        ["a1", "b2", "c3"],
        {
            "a1": [_pull(801, "ci: smoke-test the vsix")],
            "b2": [_pull(812, "ci:  publish\nOpen VSX"), _pull(801, "dup")],
            "c3": [],
        },
    )

    pulls = release_pr_body.merged_pull_requests(REPO, "v3.4.7", "origin/main")

    assert pulls == [
        PullRequest(801, "ci: smoke-test the vsix"),
        PullRequest(812, "ci: publish Open VSX"),
    ]
    log = next(call for call in calls if call[:2] == ["git", "log"])
    assert "v3.4.7..origin/main" in log
    assert "--first-parent" in log
    assert "--reverse" in log
    assert ["gh", "api", f"repos/{REPO}/commits/a1/pulls"] in calls


def test_dependabot_and_release_pull_requests_are_left_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _fake_run_capture(
        monkeypatch,
        ["a1", "b2", "c3"],
        {
            "a1": [_pull(810, "chore(deps): bump", login="dependabot[bot]")],
            "b2": [_pull(808, "Release 3.4.7", ref="release/version-bump")],
            "c3": [_pull(812, "ci: publish")],
        },
    )

    pulls = release_pr_body.merged_pull_requests(REPO, "v3.4.7", "origin/main")

    assert pulls == [PullRequest(812, "ci: publish")]


def test_a_failed_api_call_warns_and_keeps_the_rest(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _fake_run_capture(
        monkeypatch,
        ["a1", "b2"],
        {"a1": None, "b2": [_pull(812, "ci: publish")]},
    )

    pulls = release_pr_body.merged_pull_requests(REPO, "v3.4.7", "origin/main")

    assert pulls == [PullRequest(812, "ci: publish")]
    assert "::warning::no pull requests for a1" in capsys.readouterr().err


def test_the_last_release_tag_skips_pre_releases(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _fake_run_capture(monkeypatch, [], {})

    assert release_pr_body.last_release_tag("origin/main") == "v3.4.7"
    assert calls == [
        [
            "git",
            "describe",
            "--tags",
            "--abbrev=0",
            "--match",
            "v[0-9]*",
            "--exclude",
            "*-pre.*",
            "origin/main",
        ]
    ]


def test_the_body_lists_each_pull_request_under_its_heading() -> None:
    body = release_pr_body.render_body(
        "3.4.8",
        "patch",
        [PullRequest(801, "ci: smoke-test the vsix"), PullRequest(812, "ci: publish")],
    )

    assert body.endswith(
        "### Pull requests in this release\n\n"
        "- #801 ci: smoke-test the vsix\n"
        "- #812 ci: publish\n"
    )
    assert "`### 3.4.8`" in body
    assert "`v3.4.8` tag" in body
    assert "Bump: `patch`." in body


def test_the_body_says_so_when_no_pull_request_was_found() -> None:
    body = release_pr_body.render_body("3.4.8", "patch", [])

    assert body.endswith("### Pull requests in this release\n\n- None found.\n")


def test_main_prints_the_body(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _fake_run_capture(monkeypatch, ["a1"], {"a1": [_pull(812, "ci: publish")]})

    argv = ["--version", "3.4.8", "--bump", "minor", "--repo", REPO]

    assert release_pr_body.main(argv) == 0

    out = capsys.readouterr().out
    assert "- #812 ci: publish\n" in out
    assert "Bump: `minor`." in out
