#!/usr/bin/env python3
"""A push skips CI's Rust jobs only when it changed nothing but release notes and CI passed on the commit
before it; everything unclear runs them."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("skip_rust_jobs", Path(__file__).with_name("skip-rust-jobs.py"))
skip = importlib.util.module_from_spec(spec)
spec.loader.exec_module(skip)

BEFORE, SHA = "a" * 40, "b" * 40
NOTES = [{"filename": "docs/releases/0.2.97.md", "status": "added"},
         {"filename": "docs/releases/unreleased/ST-1.md", "status": "removed"}]


def compare(files, status="ahead", behind=0):
    return {"status": status, "behind_by": behind, "files": files}


def run(status="completed", conclusion="success", number=7, sha=BEFORE, event="push"):
    return {"id": number, "run_number": number, "head_sha": sha, "event": event, "status": status,
            "conclusion": conclusion, "html_url": f"https://example.invalid/runs/{number}"}


class Hub:
    """The two API reads: the compare answer, then one answer per poll (the last one repeats)."""

    def __init__(self, files, *polls):
        self.files, self.polls, self.asked, self.now = files, list(polls), [], 0

    def get(self, path):
        self.asked.append(path)
        if path.startswith("compare/"):
            return self.files
        answer = self.polls.pop(0) if len(self.polls) > 1 else self.polls[0]
        if isinstance(answer, Exception):
            raise answer
        return {"workflow_runs": answer}

    def sleep(self, seconds):
        self.now += seconds

    def decide(self, before=BEFORE, **kw):
        return skip.decide(self.get, before, SHA, "ci.yml", "main", clock=lambda: self.now, sleep=self.sleep, **kw)


class DecideTests(unittest.TestCase):
    def setUp(self):
        self.quiet = contextlib.redirect_stdout(io.StringIO())
        self.quiet.__enter__()
        self.addCleanup(self.quiet.__exit__, None, None, None)

    def refused(self, hub, **kw):
        with self.assertRaises(skip.Undecided) as caught:
            hub.decide(**kw)
        return str(caught.exception)

    def test_release_notes_on_a_passed_commit_skip(self):
        hub = Hub(compare(NOTES), [run()])
        self.assertIn("CI passed on aaaaaaa", hub.decide())
        self.assertEqual(hub.asked, [
            f"compare/{BEFORE}...{SHA}",
            f"actions/workflows/ci.yml/runs?head_sha={BEFORE}&event=push&branch=main&per_page=20"])

    def test_any_other_file_runs_them(self):
        hub = Hub(compare(NOTES + [{"filename": "hub-rs/Cargo.lock"}, {"filename": "README.md"}]), [run()])
        self.assertEqual(self.refused(hub), "it changes hub-rs/Cargo.lock and 1 more")
        self.assertEqual(len(hub.asked), 1)  # the run on the commit before is not even read

    def test_a_file_moved_into_the_notes_counts_under_its_old_name(self):
        moved = {"filename": "docs/releases/x.md", "previous_filename": "hub-rs/x.md", "status": "renamed"}
        self.assertEqual(self.refused(Hub(compare([moved]), [run()])), "it changes hub-rs/x.md")

    def test_a_similar_directory_is_not_the_notes(self):
        self.assertIn("docs/releases-old/a.md", self.refused(Hub(compare([{"filename": "docs/releases-old/a.md"}]), [run()])))

    def test_a_rewritten_or_first_push_runs_them(self):
        self.assertIn("not a plain step forward", self.refused(Hub(compare(NOTES, "diverged", 2), [run()])))
        self.assertIn("no commit before", self.refused(Hub(compare(NOTES), [run()]), before="0" * 40))
        self.assertIn("no commit before", self.refused(Hub(compare(NOTES), [run()]), before=None))

    def test_a_change_too_long_to_list_or_empty_runs_them(self):
        many = [{"filename": f"docs/releases/{i}.md"} for i in range(skip.MAX_FILES)]
        self.assertIn("300 or more files", self.refused(Hub(compare(many), [run()])))
        self.assertIn("no changed files", self.refused(Hub(compare([]), [run()])))

    def test_a_failed_or_cancelled_run_before_runs_them(self):
        for conclusion in ("failure", "cancelled", "timed_out", None):
            self.assertIn(f"ended {conclusion}", self.refused(Hub(compare(NOTES), [run(conclusion=conclusion)])))

    def test_it_waits_for_the_run_before_and_takes_its_verdict(self):
        hub = Hub(compare(NOTES), [run("queued", None)], [run("in_progress", None)], [run()])
        self.assertIn("CI passed", hub.decide())
        self.assertEqual(hub.now, 2 * skip.POLL)
        hub = Hub(compare(NOTES), [run("in_progress", None)], [run(conclusion="failure")])
        self.assertIn("ended failure", self.refused(hub))

    def test_the_newest_run_on_that_commit_decides(self):
        hub = Hub(compare(NOTES), [run(number=3), run(conclusion="failure", number=9)])
        self.assertIn("ended failure", self.refused(hub))
        other = [run(number=9, sha=SHA), run(number=8, event="pull_request")]  # not push runs on that commit
        self.assertIn("no run yet", self.refused(Hub(compare(NOTES), other)))

    def test_the_wait_ends(self):
        hub = Hub(compare(NOTES), [run("in_progress", None)])
        self.assertIn("has not passed after 90s (in_progress)", self.refused(hub, wait=90))
        hub = Hub(compare(NOTES), [])
        self.assertIn(f"has not passed after {skip.APPEAR}s (no run yet)", self.refused(hub))

    def test_one_unanswered_poll_is_asked_again(self):
        hub = Hub(compare(NOTES), OSError("reset"), [run()])
        self.assertIn("CI passed", hub.decide())
        self.assertIn("did not answer", self.refused(Hub(compare(NOTES), OSError("down"))))


class ApiTests(unittest.TestCase):
    def get(self, *answers):
        answers, self.asked = list(answers), []

        def opener(request, timeout):
            self.asked.append((request.full_url, request.get_header("Authorization")))
            answer = answers.pop(0)
            if isinstance(answer, Exception):
                raise answer
            return contextlib.closing(io.BytesIO(json.dumps(answer).encode()))
        return skip.api("o/r", "t0ken", "https://api.example.invalid", sleep=lambda s: None, opener=opener)

    def error(self, code):
        return skip.urllib.error.HTTPError("https://api.example.invalid", code, "x", {}, None)

    def test_a_server_error_is_asked_again_and_a_refusal_is_not(self):
        self.assertEqual(self.get(self.error(500), OSError("reset"), {"ok": 1})("compare/a...b"), {"ok": 1})
        self.assertEqual(self.asked[0], ("https://api.example.invalid/repos/o/r/compare/a...b", "Bearer t0ken"))
        self.assertEqual(len(self.asked), 3)
        with self.assertRaises(OSError):
            self.get(self.error(502), self.error(502), self.error(502), {"ok": 1})("x")
        with self.assertRaises(OSError):
            self.get(self.error(403), {"ok": 1})("x")
        self.assertEqual(len(self.asked), 1)

    def test_an_answer_that_is_not_an_object_is_refused(self):
        with self.assertRaises(ValueError):
            self.get([1, 2])("x")


class MainTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.out = self.dir / "output"
        event = self.dir / "event.json"
        event.write_text(json.dumps({"before": BEFORE}))
        self.env = {"GITHUB_EVENT_PATH": str(event), "GITHUB_OUTPUT": str(self.out), "GITHUB_SHA": SHA,
                    "GITHUB_REPOSITORY": "o/r", "GITHUB_REF_NAME": "main",
                    "GITHUB_WORKFLOW_REF": "o/r/.github/workflows/ci.yml@refs/heads/main"}

    def main(self, hub, env=None):
        said = io.StringIO()
        with contextlib.redirect_stdout(said):
            code = skip.main([], env or self.env, hub.get, clock=lambda: hub.now, sleep=hub.sleep)
        return code, said.getvalue()

    def test_the_decision_is_the_output_and_the_exit_code_is_always_zero(self):
        code, said = self.main(Hub(compare(NOTES), [run()]))
        self.assertEqual((code, self.out.read_text()), (0, "skip=true\n"))
        self.assertIn("Rust jobs skipped: only docs/releases/ changed", said)
        self.out.unlink()
        code, said = self.main(Hub(compare([{"filename": "web/src/a.ts"}]), [run()]))
        self.assertEqual((code, self.out.read_text()), (0, "skip=false\n"))
        self.assertIn("Rust jobs run: it changes web/src/a.ts", said)

    def test_the_workflow_asked_about_is_the_one_running(self):
        hub = Hub(compare(NOTES), [run()])
        self.main(hub, dict(self.env, GITHUB_WORKFLOW_REF="o/r/.github/workflows/checks.yml@refs/heads/main"))
        self.assertTrue(hub.asked[1].startswith("actions/workflows/checks.yml/runs?"))

    def test_a_broken_environment_or_api_runs_them(self):
        class Down(Hub):
            def get(self, path):
                raise OSError("no route")
        code, said = self.main(Down(None))
        self.assertEqual((code, self.out.read_text()), (0, "skip=false\n"))
        self.assertIn("could not tell (OSError: no route)", said)
        self.out.unlink()
        env = dict(self.env)
        del env["GITHUB_SHA"]
        code, _ = self.main(Hub(compare(NOTES), [run()]), env)
        self.assertEqual((code, self.out.read_text()), (0, "skip=false\n"))


if __name__ == "__main__":
    unittest.main()
