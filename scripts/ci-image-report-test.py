"""Meaningful reducer regressions: no adoption from incomplete or mismatched data."""
import contextlib
import importlib.util
import io
import json
import hashlib
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("report", Path(__file__).with_name("ci-image-report.py"))
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class MeasurementProof(unittest.TestCase):
    def fixture(self, root, repetitions=range(1, 6)):
        jobs = []
        for lane in report.LANES:
            for arm in report.ARMS:
                for repetition in repetitions:
                    path = root / f"{lane}-{arm}-{repetition}"
                    report.write(path / "metadata.json", {
                        "lane": lane, "arm": arm, "repetition": repetition,
                        "seed": False, "snapshot_restored": True, "snapshot_sha256": "d" * 64, "status": "success",
                        "run": "123", "source": "a" * 40, "recipe": "b" * 64,
                        "image": "ghcr.io/ekalb81/agent-odometer-ci-probe@sha256:" + "c" * 64, "snapshot": lane,
                    })
                    (path / "identity.txt").write_text("1001\n/home/runner\nmatching tools/packages", encoding="utf-8")
                    (path / "commands.tsv").write_text("".join(f"{label}\t1\t0\n" for label in report.COMMANDS[lane]), encoding="utf-8")
                    if arm == "image":
                        (path / "pull.log").write_text("Pull complete\nDigest: sha256:" + "c" * 64, encoding="utf-8")
                        (path / "cached-pull.log").write_text("Already exists\nDigest: sha256:" + "c" * 64, encoding="utf-8")
                        report.write(path / "image.json", [{"RepoDigests": ["ghcr.io/ekalb81/agent-odometer-ci-probe@sha256:" + "c" * 64]}])
                        (path / "pull-ms.txt").write_text("1500", encoding="utf-8")
                        (path / "cached-pull-ms.txt").write_text("100", encoding="utf-8")
                    name = f"Measure first {lane} {arm}" if repetition == 1 else f"Measure repeated {lane} {arm} {repetition}"
                    seconds = 100 if arm == "apt" else 70
                    jobs.append({"name": name, "conclusion": "success", "started_at": "2026-10-04T00:00:00Z", "completed_at": f"2026-10-04T00:01:{seconds-60:02}Z"})
        jobs.append({"name": "publish", "conclusion": "success", "started_at": "2026-10-04T00:00:00Z", "completed_at": "2026-10-04T00:00:10Z"})
        path = root / "jobs.jsonl"
        path.write_text("".join(json.dumps(job) + "\n" for job in jobs), encoding="utf-8")
        return path, jobs

    def render(self, root, jobs):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            report.summarize(root, jobs)
        return output.getvalue()

    def test_first_pair_refuses_dependency_difference_snapshot_miss_and_partial_lane(self):
        for change in ("dependency", "snapshot", "command"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.fixture(root, (1,))
                path = root / "check-image-1"
                if change == "dependency":
                    (path / "identity.txt").write_text("different libssl patch", encoding="utf-8")
                elif change == "snapshot":
                    value = report.read(path / "metadata.json"); value["snapshot_restored"] = False
                    report.write(path / "metadata.json", value)
                else:
                    (path / "commands.tsv").write_text("npm-ci\t1\t0\n", encoding="utf-8")
                with self.assertRaises(ValueError):
                    report.validate_pairs(report.load_observations(root), (1,))

    def test_full_sample_can_meet_gate_but_never_claims_stable_tail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); jobs, _ = self.fixture(root)
            output = self.render(root, jobs)
            self.assertIn("meets the sample adoption criteria", output)
            self.assertIn("not a stable population-tail estimate", output)
            self.assertIn("5/5", output)

    def test_failed_job_time_stays_in_total_and_prevents_adoption(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); jobs_path, jobs = self.fixture(root)
            jobs[0]["conclusion"] = "failure"
            jobs_path.write_text("".join(json.dumps(job) + "\n" for job in jobs), encoding="utf-8")
            output = self.render(root, jobs_path)
            self.assertIn("inconclusive", output)
            self.assertIn("4/5", output)
            self.assertIn("42.67 observed runner-minutes", output)  # includes failed 100s job
            self.assertIn("Measure first check apt: failure", output)

    def test_skipped_remaining_repetitions_are_not_fabricated_measurements(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); jobs, _ = self.fixture(root, (1,))
            output = self.render(root, jobs)
            self.assertIn("1/5", output)
            self.assertIn("missing/skipped", output)
            self.assertIn("inconclusive", output)

    def test_n5_nearest_rank_p95_is_maximum(self):
        self.assertEqual(report.percentile95([1, 2, 3, 4, 50]), 50)

    def test_recipe_guard_rejects_changed_flags_and_extra_commands(self):
        source = Path(".github/workflows/ci.yml").read_bytes()
        for changed in (source.replace(b"cargo test --locked", b"cargo test --locked --release"),
                        source.replace(b"run: npm run check", b"run: npm run check && echo extra")):
            with patch.object(Path, "read_bytes", return_value=changed), self.assertRaises(ValueError):
                report.validate_recipes()

    def test_empty_wrong_missing_and_corrupt_registry_proofs_never_qualify(self):
        for change in ("empty-image", "wrong-digest", "missing-time", "negative-time", "corrupt-time", "empty-log", "wrong-log"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory); jobs, _ = self.fixture(root)
                path = root / "check-image-1"
                if change == "empty-image": report.write(path / "image.json", [])
                elif change == "wrong-digest": report.write(path / "image.json", [{"RepoDigests": ["sha256:" + "e" * 64]}])
                elif change == "missing-time": (path / "cached-pull-ms.txt").unlink()
                elif change == "negative-time": (path / "pull-ms.txt").write_text("-1", encoding="utf-8")
                elif change == "corrupt-time": (path / "pull-ms.txt").write_text("unknown", encoding="utf-8")
                elif change == "empty-log": (path / "pull.log").write_text("", encoding="utf-8")
                else: (path / "cached-pull.log").write_text("Digest: sha256:" + "e" * 64, encoding="utf-8")
                output = self.render(root, jobs)
                self.assertIn("inconclusive", output)
                self.assertNotIn("meets the sample adoption criteria", output)

    def test_same_named_snapshot_with_different_hash_fails_pair_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); self.fixture(root, (1,))
            path = root / "check-image-1/metadata.json"
            value = report.read(path); value["snapshot_sha256"] = "e" * 64; report.write(path, value)
            with self.assertRaises(ValueError):
                report.validate_pairs(report.load_observations(root), (1,))

    @unittest.skipIf(os.name == "nt", "GNU tar integration runs on Linux; local container proof is recorded separately")
    def test_tar_snapshot_preserves_modes_links_and_refuses_corruption_or_escape(self):
        script = Path(__file__).with_name("ci-image-snapshot.sh").resolve()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); profile = root / "profile"; workspace = profile / "work/repo"
            target = workspace / "src-tauri/target"; target.mkdir(parents=True)
            registry = profile / ".cargo/registry"; registry.mkdir(parents=True)
            source = registry / "source"; source.write_bytes(b"synthetic crate bytes")
            link = registry / "link"; link.symlink_to("source")
            binary = target / "binary"; binary.write_bytes(b"synthetic executable"); binary.chmod(0o751)
            report.write(workspace / "output/ci-image/snapshot.json", {"synthetic": True})
            environment = dict(os.environ, HOME=str(profile), GITHUB_WORKSPACE=str(workspace),
                               RUNNER_TEMP=str(root / "temporary"), GITHUB_ENV=str(root / "env"))
            def run(mode):
                return subprocess.run(["bash", str(script), mode], env=environment, capture_output=True, text=True)
            self.assertEqual(run("pack").returncode, 0)
            archive = root / "temporary/ci-image-snapshot/snapshot.tar.gz"
            checksum = archive.with_name("snapshot.sha256")
            original = archive.read_bytes()
            binary.write_bytes(b"changed"); binary.chmod(0o600); link.unlink()
            self.assertEqual(run("restore").returncode, 0)
            self.assertEqual(binary.read_bytes(), b"synthetic executable")
            self.assertEqual(binary.stat().st_mode & 0o777, 0o751)
            self.assertEqual(link.readlink(), Path("source"))
            archive.write_bytes(original[:-5])
            self.assertNotEqual(run("restore").returncode, 0)
            for escape in ("path", "link"):
                with tarfile.open(archive, "w:gz") as tar:
                    member = tarfile.TarInfo("../escaped" if escape == "path" else ".cargo/registry/escape")
                    if escape == "link": member.type = tarfile.SYMTYPE; member.linkname = "../../../escaped"
                    tar.addfile(member)
                checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "  snapshot.tar.gz\n", encoding="utf-8")
                self.assertNotEqual(run("restore").returncode, 0)
                self.assertFalse((root / "escaped").exists())


if __name__ == "__main__":
    unittest.main()
