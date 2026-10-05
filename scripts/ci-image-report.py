"""Frozen CI image observations, fail-closed first-pair gate and sample report."""
import argparse
import datetime as dt
import hashlib
import json
import math
import os
from pathlib import Path
import re
import statistics

LANES = ("check", "msrv", "coverage")
ARMS = ("apt", "image")
COMMANDS = {
    "check": ("npm-ci", "frontend-check", "frontend-coverage", "updater-manifest", "frontend-build", "rust-format", "rust-clippy", "rust-tests"),
    "msrv": ("msrv-check",),
    "coverage": ("rust-coverage", "coverage-summary"),
}
OUT = Path("output/ci-image")
REVIEWED_CI_SHA256 = "e9f88a9a2eb28a8dd1c0ed835efaf1c6d6d3273583efb079c4bfd62dbc890e1c"


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def read(path):
    return json.loads(path.read_text(encoding="utf-8"))


def owner():
    return {"run": os.environ["GITHUB_RUN_ID"], "attempt": os.environ["GITHUB_RUN_ATTEMPT"], "source": os.environ["GITHUB_SHA"],
            "lane": os.environ["CI_IMAGE_LANE"], "snapshot": os.environ["CI_IMAGE_SNAPSHOT"]}


def observation(seed):
    value = owner()
    value.update(arm=os.environ["CI_IMAGE_ARM"], repetition=int(os.environ["CI_IMAGE_REPETITION"]),
                 seed=seed == "true", snapshot_restored=os.environ.get("CI_IMAGE_SNAPSHOT_RESTORED") == "true",
                 snapshot_sha256=os.environ.get("CI_IMAGE_SNAPSHOT_SHA", ""),
                 snapshot_bytes=int(os.environ.get("CI_IMAGE_SNAPSHOT_BYTES", "0")),
                 image=os.environ["CI_IMAGE_DIGEST"],
                 recipe=hashlib.sha256(Path("scripts/ci-image-lane.sh").read_bytes()).hexdigest())
    if not value["seed"]:
        if not value["snapshot_restored"] or not re.fullmatch(r"[a-f0-9]{64}", value["snapshot_sha256"]) or read(OUT / "snapshot.json") != owner():
            raise ValueError("Exact immutable seed snapshot unavailable or ownership changed")
    write(OUT / "observation/metadata.json", value)


def finish():
    path = OUT / "observation/metadata.json"
    if path.exists():
        value = read(path)
        value["status"] = os.environ["OBSERVATION_STATUS"]
        if value["seed"]:
            value["snapshot_sha256"] = os.environ.get("CI_IMAGE_SNAPSHOT_SHA", "")
            value["snapshot_bytes"] = int(os.environ.get("CI_IMAGE_SNAPSHOT_BYTES", "0"))
        write(path, value)


def load_observations(root):
    return [(read(path), path.parent) for path in Path(root).rglob("metadata.json")]


def pull_proof(value, path):
    digest = value["image"]
    if not re.fullmatch(r"ghcr\.io/ekalb81/agent-odometer-ci-probe@sha256:[a-f0-9]{64}", digest):
        raise ValueError("Requested digest outside fixed namespace")
    image = read(path / "image.json")
    if not isinstance(image, list) or len(image) != 1 or not isinstance(image[0], dict) or not isinstance(image[0].get("RepoDigests"), list) or digest not in image[0]["RepoDigests"]:
        raise ValueError("Pulled image does not prove the exact requested RepoDigest")
    timings = []
    for prefix in ("pull", "cached-pull"):
        text = (path / f"{prefix}-ms.txt").read_text(encoding="utf-8").strip()
        if not re.fullmatch(r"[0-9]+", text):
            raise ValueError("Missing, negative or corrupt pull timing")
        evidence = (path / f"{prefix}.log").read_text(encoding="utf-8").strip()
        if not evidence or f"Digest: {digest.split('@')[1]}" not in evidence:
            raise ValueError("Nonempty exact-digest pull evidence required")
        timings.append(int(text) / 1000)
    return timings


def validate_pairs(observations, repetitions):
    rows = {(v["lane"], v["arm"], v["repetition"]): (v, p)
            for v, p in observations if not v["seed"]}
    if len(rows) != len([v for v, _ in observations if not v["seed"]]):
        raise ValueError("Duplicate observations")
    for lane in LANES:
        reference = None
        for repetition in repetitions:
            pair = [rows[(lane, arm, repetition)] for arm in ARMS]
            for value, path in pair:
                if value.get("status") != "success" or not value["snapshot_restored"] or not re.fullmatch(r"[a-f0-9]{64}", value["snapshot_sha256"]):
                    raise ValueError("Failed lane or verified snapshot unavailable")
                identity = (path / "identity.txt").read_bytes()
                signature = (value["run"], value["source"], value["snapshot"], value["snapshot_sha256"], value["recipe"], value["image"], identity)
                if reference is None:
                    reference = signature
                if signature != reference:
                    raise ValueError("Tool, required dependency, UID/HOME/path, cache or source parity differs")
                lines = (path / "commands.tsv").read_text(encoding="utf-8").splitlines()
                if tuple(line.split("\t", 1)[0] for line in lines) != COMMANDS[lane] or any(line.rsplit("\t", 1)[-1] != "0" for line in lines):
                    raise ValueError("Incomplete or failing command list")
                if value["arm"] == "image":
                    pull_proof(value, path)
    return rows


def seconds(job):
    if not job.get("started_at") or not job.get("completed_at"):
        return None
    return (dt.datetime.fromisoformat(job["completed_at"].replace("Z", "+00:00")) -
            dt.datetime.fromisoformat(job["started_at"].replace("Z", "+00:00"))).total_seconds()


def percentile95(values):
    return sorted(values)[math.ceil(.95 * len(values)) - 1]


def validate_recipes():
    """Any full production workflow drift requires an explicit recipe review."""
    actual = hashlib.sha256(Path(".github/workflows/ci.yml").read_bytes()).hexdigest()
    if actual != REVIEWED_CI_SHA256:
        raise ValueError("Production ci.yml changed; review recipes and update the pinned full-file hash")
    print("Production workflow exactly matches the reviewed full-file hash")


def summarize(root, job_file):
    observations = load_observations(root)
    jobs = [json.loads(line) for line in Path(job_file).read_text(encoding="utf-8").splitlines()]
    by_name = {job["name"]: job for job in jobs}
    samples = {(lane, arm): [] for lane in LANES for arm in ARMS}
    failures = []
    pulls = []
    try:
        rows = validate_pairs(observations, range(1, 6))
        parity = True
    except (KeyError, ValueError, OSError, TypeError) as error:
        rows = {(v["lane"], v["arm"], v["repetition"]): (v, p) for v, p in observations if not v["seed"]}
        parity = False
        failures.append(f"Parity/completeness: {error}")
    for lane in LANES:
        for arm in ARMS:
            for repetition in range(1, 6):
                name = f"Measure first {lane} {arm}" if repetition == 1 else f"Measure repeated {lane} {arm} {repetition}"
                job = by_name.get(name)
                elapsed = seconds(job) if job else None
                if job and job["conclusion"] == "success" and elapsed is not None:
                    samples[lane, arm].append(elapsed)
                else:
                    failures.append(f"{name}: {job.get('conclusion', 'unfinished') if job else 'missing/skipped'}")
                row = rows.get((lane, arm, repetition))
                if arm == "image" and row:
                    value, path = row
                    try:
                        cold, warm = pull_proof(value, path)
                        pulls.append((lane, repetition, cold, warm))
                    except (OSError, ValueError, KeyError, TypeError) as error:
                        failures.append(f"{name}: invalid pull proof: {error}")
    print("# CI image sample comparison\n")
    print("Production lanes are unchanged. Five observations per arm/lane were planned. This measures warm Cargo/npm snapshots and fresh-runner registry pulls; it does not establish cold Cargo performance.\n")
    print("Full job start→completion includes checkout, tool setup, verified snapshot artifact transfer/restore, apt or digest pull, commands, artifact save and cleanup. The image cost conservatively also includes its extra cached-pull/identity probes. Queue delay is excluded and raw timestamps are retained.\n")
    print("| Lane | Arm | Successful / planned | p50 seconds | Sample p95 seconds | Observed successful runner-min |\n|---|---|---:|---:|---:|---:|")
    for (lane, arm), values in samples.items():
        values_text = f"{statistics.median(values):.1f} | {percentile95(values):.1f} | {sum(values)/60:.2f}" if values else "unavailable | unavailable | 0"
        print(f"| {lane} | {arm} | {len(values)}/5 | {values_text} |")
    print("\nNearest-rank p95 with n=5 is the sample maximum. It is not a stable population-tail estimate. Failed/time-out/skipped cases remain in the planned denominator; successful-only timing cannot justify adoption when any case is missing.\n")
    completed = [seconds(job) for job in jobs if seconds(job) is not None]
    print(f"All completed experiment jobs consumed {sum(completed)/60:.2f} observed runner-minutes, including failed jobs, seeds and publisher. Sum of per-job rounded-up minutes is {sum(math.ceil(value/60) for value in completed)} (an estimate, not billing data). The reporting job is still running and adds at most 5 minutes. Hard cap: 540 runner-minutes, six measured jobs concurrently.\n")
    print("| Lane | Repetition | First registry pull seconds | Immediate cached pull seconds |\n|---|---:|---:|---:|")
    for lane, repetition, cold, warm in pulls:
        print(f"| {lane} | {repetition} | {cold:.3f} | {warm:.3f} |")
    print("\nPull logs retain actual per-layer 'Already exists'/'Pull complete' behavior and pre-pull Docker inventory; fresh runner does not imply every base layer was absent. Every successful measurement required the same checksum-verified immutable artifact snapshot; snapshot names/hashes and source/recipe identity are in metadata. This is controlled warm-snapshot transfer, not production Actions cache-hit behavior. Production cache storage is untouched.\n")
    for lane in LANES:
        sizes = [value.get("snapshot_bytes", 0) for value, _ in observations if value["lane"] == lane and not value["seed"]]
        print(f"- {lane} compressed snapshot bytes: {max(sizes) if sizes else 'unavailable'}; stored once per attempt/lane, retained 14 days")
    eligible = parity and not failures and all(len(values) == 5 for values in samples.values())
    publication = next((seconds(job) for job in jobs if job["name"] == "publish"), None)
    gains = []
    if eligible and publication is not None:
        for lane in LANES:
            apt, image = samples[lane, "apt"], samples[lane, "image"]
            median_gain = statistics.median(apt) - statistics.median(image)
            required_gain = max(15, .1 * statistics.median(apt)) if lane in ("check", "coverage") else -.05 * statistics.median(apt)
            gains.append(median_gain >= required_gain and max(image) <= 1.1 * max(apt))
        apt_total = sum(sum(samples[lane, "apt"]) for lane in LANES)
        image_total = sum(sum(samples[lane, "image"]) for lane in LANES) + publication
        gains.append(image_total < apt_total)
        decision = "meets the sample adoption criteria; separate security/reproducibility review still required" if all(gains) else "non-adoption for this sample: improvement criteria not met"
    else:
        decision = "inconclusive: missing/failed measurements or identity/snapshot parity not proven"
    print(f"Decision: **{decision}**. No automatic production switch occurs.\n")
    if failures:
        print("Failures and unavailable observations:\n")
        for failure in failures:
            print(f"- {failure}")


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    cmd = sub.add_parser("observation"); cmd.add_argument("--seed")
    sub.add_parser("snapshot"); sub.add_parser("finish"); sub.add_parser("recipes")
    cmd = sub.add_parser("parity"); cmd.add_argument("root")
    cmd = sub.add_parser("report"); cmd.add_argument("root"); cmd.add_argument("jobs")
    args = parser.parse_args()
    if args.command == "observation": observation(args.seed)
    elif args.command == "snapshot": write(OUT / "snapshot.json", owner())
    elif args.command == "finish": finish()
    elif args.command == "recipes": validate_recipes()
    elif args.command == "parity": validate_pairs(load_observations(args.root), (1,)); print("All three first pairs passed identity/snapshot/correctness gates")
    elif args.command == "report": summarize(args.root, args.jobs)


if __name__ == "__main__":
    main()
