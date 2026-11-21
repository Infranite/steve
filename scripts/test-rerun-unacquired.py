#!/usr/bin/env python3
"""A failed CI push run is rerun only when every failure is a job GitHub never gave a runner, at most three
times; any job that ran a step and failed keeps the run failed."""
import contextlib
import importlib.util
import io
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("rerun_unacquired", Path(__file__).with_name("rerun-unacquired.py"))
rerun = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rerun)

NOT_ACQUIRED = "The job was not acquired by Runner of type hosted even after multiple attempts"
CAPACITY = "Due to capacity constraints, jobs targeting macOS arm64 runners may experience longer queue times."


def job(id, name, conclusion="success", steps=3):
    return {"id": id, "name": name, "conclusion": conclusion, "steps": [{"number": n} for n in range(steps)]}


def unacquired(id, name):
    return job(id, name, "cancelled", steps=0)


NOTES = {}


def annotations(j):
    return NOTES.get(j["id"], [])


def run_44562771299():
    """The run that showed it: three macOS jobs unacquired, the macOS `rust` job and the gate failing after."""
    NOTES.clear()
    NOTES.update({4: [NOT_ACQUIRED, CAPACITY], 5: [NOT_ACQUIRED, CAPACITY], 6: [NOT_ACQUIRED]})
    return [job(1, "scripts"), job(2, "web"), job(3, "rust-test (ubuntu-22.04, 1/4)"),
            unacquired(4, "apple"), unacquired(5, "rust-test (macos-15, 1/2)"),
            unacquired(6, "rust-test (macos-15, 2/2)"), job(7, "rust (ubuntu-22.04)"),
            job(8, "rust (macos-15)", "failure", steps=6), job(9, "CI gate", "failure", steps=3)]


class Decide(unittest.TestCase):
    def test_only_unacquired_jobs_and_their_reporters_failed(self):
        why, refusal = rerun.decide(run_44562771299(), annotations, attempt=1)
        self.assertIsNone(refusal)
        self.assertIn("apple", why)

    def test_three_reruns_at_most(self):
        for attempt in (2, 3):
            self.assertIsNotNone(rerun.decide(run_44562771299(), annotations, attempt)[0])
        why, refusal = rerun.decide(run_44562771299(), annotations, attempt=4)
        self.assertIsNone(why)
        self.assertIn("3 times", refusal)

    def test_a_job_that_ran_and_failed_is_never_rerun(self):
        jobs = run_44562771299()
        jobs[2] = job(3, "rust-test (ubuntu-22.04, 1/4)", "failure", steps=20)
        why, refusal = rerun.decide(jobs, annotations, attempt=1)
        self.assertIsNone(why)
        self.assertIn("rust-test (ubuntu-22.04, 1/4) ran", refusal)

    def test_a_cancelled_job_without_the_annotation_is_not_a_missing_runner(self):
        jobs = run_44562771299()
        NOTES[4] = [CAPACITY]  # queued long, but GitHub did not say it gave up
        why, refusal = rerun.decide(jobs, annotations, attempt=1)
        self.assertIsNone(why)
        self.assertIn("apple", refusal)

    def test_a_job_with_steps_is_not_unacquired_even_with_the_annotation(self):
        jobs = run_44562771299()
        jobs[3] = job(4, "apple", "cancelled", steps=2)
        why, refusal = rerun.decide(jobs, annotations, attempt=1)
        self.assertIsNone(why)
        self.assertIn("apple ran", refusal)

    def test_a_platforms_rust_job_counts_as_a_reporter_only_when_its_shards_went_unacquired(self):
        NOTES.clear()
        NOTES[4] = [NOT_ACQUIRED]
        jobs = [unacquired(4, "apple"), job(8, "rust (macos-15)", "failure", steps=6),
                job(9, "CI gate", "failure")]
        why, refusal = rerun.decide(jobs, annotations, attempt=1)
        self.assertIsNone(why)
        self.assertIn("rust (macos-15) ran", refusal)

    def test_nothing_failed_or_nothing_unacquired(self):
        NOTES.clear()
        self.assertEqual(rerun.decide([job(1, "web")], annotations, 1), (None, "no failed job"))
        why, refusal = rerun.decide([job(9, "CI gate", "failure")], annotations, 1)
        self.assertIsNone(why)
        self.assertIn("want of a runner", refusal)


class Main(unittest.TestCase):
    def calls(self, jobs, notes, numbers=(7,)):
        asked = []

        def call(path, method="GET"):
            asked.append((method, path))
            if path.startswith("actions/runs/") and path.count("/") == 2:
                return {"id": int(path.rsplit("/", 1)[1]), "workflow_id": 99, "run_number": 7}
            if path.startswith("actions/workflows/99/runs"):
                return {"workflow_runs": [{"id": n, "run_number": n, "html_url": f"run/{n}"} for n in numbers]}
            if "/jobs?" in path:
                return {"total_count": len(jobs), "jobs": jobs}
            if path.startswith("check-runs/"):
                return [{"message": m} for m in notes.get(int(path.split("/")[1]), [])]
            return None
        return asked, call

    def test_requests_the_rerun_of_failed_jobs(self):
        jobs = run_44562771299()
        asked, call = self.calls(jobs, dict(NOTES))
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(rerun.main(["44562771299", "1"], env={}, call=call), 0)
        self.assertIn(("POST", "actions/runs/44562771299/rerun-failed-jobs"), asked)
        self.assertIn("Rerun of the failed jobs requested", out.getvalue())

    def test_does_not_rerun_a_real_failure(self):
        jobs = run_44562771299()
        jobs[1] = job(2, "web", "failure", steps=9)
        asked, call = self.calls(jobs, dict(NOTES))
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(rerun.main(["44562771299", "1"], env={}, call=call), 0)
        self.assertNotIn("POST", [m for m, _ in asked])
        self.assertIn("Not rerun: web ran and ended failure", out.getvalue())

    def test_a_newer_push_run_is_not_displaced(self):
        # a rerun re-enters the ci-main group and would cancel the newer push's pending run
        asked, call = self.calls(run_44562771299(), dict(NOTES), numbers=(7, 8))
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(rerun.main(["44562771299", "1"], env={}, call=call), 0)
        self.assertNotIn("POST", [m for m, _ in asked])
        self.assertIn("a newer push run exists (run/8)", out.getvalue())

    def test_an_unanswered_question_is_not_a_rerun(self):
        def call(path, method="GET"):
            raise OSError("GitHub did not answer")
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(rerun.main(["1", "1"], env={}, call=call), 0)
        self.assertIn("could not tell", out.getvalue())


if __name__ == "__main__":
    unittest.main()
