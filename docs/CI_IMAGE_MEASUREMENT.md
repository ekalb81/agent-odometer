# CI image measurement (#78)

The original `run_ci_image_probe` remains a local tar-load lower-bound probe.
It cannot establish registry transfer cost or full-lane savings. The separate
`Opt-in full CI image measurement` workflow is a bounded experiment. It changes
no production lane, cache entry/key, capability or application setting.

## Reviewed execution boundary

Drafting and local validation do not publish or dispatch anything. Review the
concrete workflow before its first registry publication and dispatch. Once
reviewed and present on trusted `main`, dispatch with `confirm_probe=true`.
Other repositories/refs and the default false input cannot run it. There is no
arbitrary source SHA, image name, command, repetition or concurrency input.

Only the publisher has `packages: write`; it can publish the fixed
`ghcr.io/ekalb81/agent-odometer-ci-probe` package with a unique run/attempt/source
tag. It records and distributes its immutable digest. Consumers have only
`packages: read`. Tokens go to `docker login --password-stdin`, never command
arguments, container environments or artifacts. Checkout persists no credential.
The report has `actions: read` solely to read job timestamps and conclusions.
No Codecov upload or OIDC permission is added to this experiment.

## Design and budget

| Stage | Jobs | Timeout/job | Maximum runner-minutes |
|---|---:|---:|---:|
| Build/publish/freeze tools | 1 | 20 min | 20 |
| Warm independent lane snapshots | 3 | 20 min | 60 |
| First apt/image pair in each lane | 6 | 15 min | 90 |
| Identity/correctness gate | 1 | 5 min | 5 |
| Repetitions 2–5, all lanes and arms | 24 | 15 min | 360 |
| Full report | 1 | 5 min | 5 |
| Total | 36 | | **540** |

At most six measured jobs run concurrently; seed concurrency is three. A first
pair failure, differing tool/dependency identity or missing exact verified snapshot
prevents repetitions 2–5. Other first pairs finish to preserve their evidence.
Registry pulls separately time out after 300 seconds; the immediate cached pull
after 120 seconds. No automatic reruns hide failures. Cancel manually if the
whole run needs to stop; the global concurrency group prevents simultaneous
experiments. This cap measures runner time, not queue delay or wall-clock time.

The planning estimate is roughly 100–140 runner-minutes, based on the historical
full-lane samples documented in `ci.yml` plus publication and seed overhead.
This is an estimate, not a promised current result. The 540-minute limit is the
explicit worst case. A complete, valid run is required for a timing comparison.

The publisher freezes exact source SHA, Node 22 patch, stable Rust patch, declared
MSRV (normalized to its zero patch where omitted), cargo-llvm-cov version,
Dockerfile/base-image digest, installed image packages and recipe hashes. Each
lane uses its own matching toolchain; coverage includes llvm-tools-preview.

Seeds execute the complete apt-based lane once, pack Cargo registry/git/target
and npm `_cacache` into an attempt-isolated immutable tar artifact with explicit
gzip level 1 compression, and record its
SHA-256 plus ownership. Tar preserves permissions and symlinks; extraction rejects
escaping paths/links. Both arms download the same exact per-lane artifact and
verify its checksum before restoring. There is no Actions cache read/write, so
experimental snapshots cannot evict production cache entries. Compressed artifact
size is recorded; the three snapshots have 14-day artifact retention.
The archive excludes npm logs, npmrc, Cargo credentials/configuration, tool
binaries, Rustup and all other HOME paths. The report downloads only small
observation/publisher artifacts, never the three seed tarballs again.
These snapshots are a controlled warm-snapshot artifact-transfer sample, not the production Actions cache
hit distribution and not a cold-Cargo apt/image comparison.

The apt arm uses the existing retrying `linux-deps` action on Ubuntu 22.04.
The image arm times an actual digest pull on each fresh hosted runner, then runs
the same complete command list inside that image. Workspace/target paths,
UID/HOME, Node toolcache and Rust/Cargo/npm homes are identical and mounted at
their original paths. The container drops capabilities and receives no token.
Required development-package versions, tool versions, pkg-config availability
and path/UID identity must match in every first pair and in the final report.
Unrelated host packages are not a parity requirement. Full compilation and
tests additionally exercise library resolution; metadata presence alone is
insufficient. A pinned SHA-256 of the full production ci.yml rejects any workflow drift,
including extra commands or changed flags. Review the complete production recipes,
update the copied command lists if needed, and deliberately update
REVIEWED_CI_SHA256 before another experiment; never refresh that hash automatically.
Recipe checks, reducer regressions and frozen-version validation run before push.

The second immediate pull measures Docker's local layer reuse separately. The
pre-pull Docker inventory and pull logs expose any already-cached base layers;
“fresh hosted runner” does not mean every layer was cold. The full first-pull
cost remains inside end-to-end image-job duration. No local image tar transfer is substituted for a registry pull. Exact
RepoDigests, nonempty digest-bearing first/cached logs and nonnegative timings
are required; absent or corrupt evidence makes the comparison inconclusive.

## Evidence and decision

The first experiment, [run 37256139929](https://github.com/ekalb81/agent-odometer/actions/runs/37256139929),
used source `9a35c2b3d84910cead9ab4c5fe0ae84ec6967ce6` and was inconclusive.
Publication and the MSRV/Coverage seeds passed. All eight Check seed commands
passed, but its default-compression snapshot took 7m36s to package 3,247,165,123
bytes; the job reached its 15-minute cap during upload. All 30 measured cases
were skipped. Total observed job time, including the report, was 1,734 seconds
(28.90 runner-minutes). This establishes preparation overhead, not a relative
image speed result.

The second reviewed attempt used faster compression and a 20-minute seed-only
cap. Measured jobs retained their 15-minute cap, five planned samples per arm/lane,
identical snapshot requirements, and adoption criteria. The second attempt was
dispatched separately after source and recipe review.

Artifacts retain source/digest/tool/snapshot names/checksums, package versions, raw pull
logs, per-command timings, complete job timestamps/conclusions and failures.
End-to-end job start→completion includes checkout, setup, verified snapshot artifact transfer,
dependencies or pull, execution and cleanup. Queue delay is excluded. Image
jobs conservatively also include their extra cached-pull and identity probes.
Completed failed-job time and publication/seed overhead remain in total runner
minutes. Rounded per-job minutes are an estimate; they are not billing data.
The report job is still running when it reads timestamps, so its remaining
maximum is stated separately.

Each arm/lane has five planned observations. Report p50 and nearest-rank sample
p95; **at n=5, p95 is the maximum**. It does not estimate a stable population
tail. Missing, failed, cancelled and timed-out observations remain in the planned
denominator. Successful-only times cannot qualify an incomplete run for adoption.

The sample adoption criteria require all observations and identity checks to
pass, at least 10% **and** 15 seconds lower Check/Coverage median, no lane with
over 5% median regression, no sample maximum over 10% worse, and lower aggregate
observed image runner time after adding one publication job across this sample.
The seed/gate/report overhead is separately charged to the experiment, not
silently assigned to only one candidate. A passing sample is a candidate for
review, never an automatic switch. Failure to improve supports non-adoption for
this sample; incomplete or mismatched evidence is inconclusive.

If adoption is later approved, publish a separately reviewed immutable version,
pin its digest in consuming lanes, keep the current apt action as rollback,
record its source/tool/package manifest, and repeat comparison after dependency
or base-image updates. Reverting the consuming workflow to the existing apt
action is the rollback; a moving tag is never a recovery strategy. Experimental
package retention/deletion remains a separate repository-owner operation.

## Second experiment: completed measurement and decision

[Run 37262773383](https://github.com/ekalb81/agent-odometer/actions/runs/37262773383) completed on 2026-10-05 at source `e74cd3077e7383465c2ce81668130f704dd8b6a4`. The publisher, all three seeds, the three first apt/image pairs, and the first-pair parity gate passed. Of 30 planned measurements, 29 jobs passed. Check image repetition 3 failed its Rust tests: `quota_live::tests::synthetic_stdio_driver_checks_local_identity_before_requesting_quota` returned `LaunchFailed` where the test expected `Unsupported` (695 passed, 1 failed in that test binary). Preserve that failure; no retry or replacement observation was made. The failure's observation metadata says `status: success`, but `commands.tsv` records `rust-tests` exit 101 and the GitHub job conclusion is failure. The reducer correctly rejects the command list and reports the full comparison **inconclusive**. The separate repairs in PR #318 and PR #319 address a shared test-budget isolation risk and the composite action's misleading outer status respectively; the exact cause of this run's test failure is unproven.

| Lane | Apt passed/planned | Apt median / sample max | Image passed/planned | Image median / sample max |
|---|---:|---:|---:|---:|
| Check | 5/5 | 441 / 522 s | 4/5 | 488 / 546 s among successes |
| MSRV | 5/5 | 90 / 113 s | 5/5 | 87 / 97 s |
| Coverage | 5/5 | 303 / 353 s | 5/5 | 351 / 367 s |

These are full job start-to-completion times, excluding queue delay. The nearest-rank p95 with four or five samples is the observed maximum, not a reliable population-tail estimate. Check image's successful-only timing has a missing case and cannot qualify that lane. All ten Coverage observations passed the reducer's exact snapshot, source, recipe, image, identity, command and pull checks. Its image median was **48 seconds (15.8%) slower** than apt, whereas the adoption rule requires at least a 10% and 15-second improvement. Thus this measured image does **not meet the necessary Coverage benefit**, independently of the incomplete Check lane. Do not adopt it or run again solely to fill that failed Check case; the full 30-case comparison remains formally inconclusive.

All 36 completed jobs, including publication, seeds, gate, failed measurement and report, consumed 10,213 observed runner-seconds, **170.22 runner-minutes** against the 540-minute cap. Summing each job rounded up gives 191 minutes, an estimate rather than billing data. The published in-job report says 170.08 minutes because its own eight-second job had not yet completed when it fetched timestamps. The first experiment ([run 37256139929](https://github.com/ekalb81/agent-odometer/actions/runs/37256139929)) remains separate: zero of 30 measurements and 1,734 seconds (28.90 runner-minutes) after Check seed upload timed out.

The image was pulled by immutable digest `ghcr.io/ekalb81/agent-odometer-ci-probe@sha256:4b6b9a4350f47f3fd96c19a2eeb30a7e9ead4a926da737819300350c3efec2a6`. The publisher froze Node 22.23.3, Rust stable 1.99.0, MSRV 1.95.0 and cargo-llvm-cov 0.9.1; Dockerfile, production CI, apt action and lane recipe hashes are in `publisher/recipe-hashes.txt`. The Coverage snapshot SHA-256 is `2c547fd1bb72ba5dfffc197626e37ea539158f0d78321b3377ba758d702cff71`; all ten Coverage observations used it. Each image job measured a first registry pull and immediate cached pull. First pulls ranged 14.219–23.835 seconds and cached pulls 0.060–0.272 seconds; pre-pull inventory and logs show actual layer behavior, so fresh runners do not establish wholly cold layers. The same verified warm Cargo/npm artifact snapshot was restored in each arm; this is not a cold Cargo or production Actions-cache sample. Snapshot artifacts were 3,772,546,413 bytes (Check), 565,987,711 (MSRV) and 3,092,693,398 (Coverage), stored once per lane for the attempt with 14-day retention. Production lanes and caches remained unchanged.

Evidence is retained in the run's `ci-image-full-report`, per-measurement observation, and `ci-image-publisher` artifacts. The final job timeline includes all 36 completed jobs. Review downloaded only small observations and logs; no seed tarballs were needed.
