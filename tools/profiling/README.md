## Native profiling

Profiling is intended for developers and optimization agents on Linux. It is
opt-in and adds no hooks/counters to default builds. The recorder and its checks
use the existing Rust toolchain; install [Samply](https://github.com/mstange/samply), Linux `perf`,
or [heaptrack](https://github.com/KDE/heaptrack) separately when needed. The
runner never installs tools or changes kernel permissions. Linux CPU recording
requires access to perf events; a permission error is reported in the run log.

Build before recording, after mise has installed the project tools:

```bash
XILA_APPLICATIONS_DIR=/path/to/applications/target/wasm32-wasip1/release-wasm mise run image:native
mise run build:profiling -- --features prebuilt-applications
cargo run --manifest-path tools/profiling/Cargo.toml -- measure \
  --scenario terminal-idle --output target/profiles/before --runs 3 --duration 30 \
  --applications-dir /path/to/applications/target/wasm32-wasip1/release-wasm \
  -- target/profiling/native_example --terminal --no-network --seed-image target/seed/native/seed.img
```

The `profiling` Cargo profile keeps release optimizations and full debug symbols.
If using an explicit Cargo target or custom target directory, adjust the binary
path. Every native build loads bundled applications from the disk. Prepare the
reference image before measurement and pass `--seed-image` for a new disk;
existing disks are mounted without reinstallation. The `prebuilt-applications`
feature is retained for compatibility. `--applications-dir` and
`XILA_PROFILE_APP_DIR` let the recorder identify and hash guest build artifacts;
they no longer cause runtime compilation or copying. See the
[native example](../../examples/native/README.md) and [image builder](../fs_image/README.md).
Measurements still include OS startup; they are not steady-state-only data.
Use the same disk fixture and settings on each run. A mutable disk can change
between runs; restore its baseline if the workload writes to it.

For allocation and task attribution, build with the optional features:

```bash
mise run build:profiling -- --features profiling,heaptrack
cargo run --manifest-path tools/profiling/Cargo.toml -- cpu \
  --scenario gui-navigation --output target/profiles/cpu --runs 1 --duration 60 \
  --applications-dir /path/to/applications/target/wasm32-wasip1/release-wasm \
  -- target/profiling/native_example --gui --no-network --resolution 800x600 --seed-image target/seed/native/seed.img
cargo run --manifest-path tools/profiling/Cargo.toml -- heap \
  --scenario gui-navigation --output target/profiles/heap --runs 1 --duration 60 \
  --applications-dir /path/to/applications/target/wasm32-wasip1/release-wasm \
  -- target/profiling/native_example --gui --no-network --resolution 800x600 --seed-image target/seed/native/seed.img
```

Replay the same manual GUI interactions for exploratory recordings. For repeatable
comparisons, use a bounded scripted workload and pass `--stdin INPUT_FILE` when
it accepts stdin; the file is replayed for every run. The default stdin is empty.
The runner also works with an already-built benchmark or test harness, so agents
can isolate a hot path without starting the whole graphical OS.

Each new output directory contains `summary.json`, per-run stdout/stderr logs,
and any profiles. Existing directories are refused to protect previous results.
The summary retains exact argv, executable hash, Git revision/dirty status,
selected build/locale settings, prebuilt application hashes, CPU model, duration, exit status and median
CPU/RSS measurements. It does not copy the full environment or credentials.
A duration limit stops the process group; forced kills and failed runs have no
successful median. Inspect the logs to establish that the intended scenario
actually ran. Profiler recordings must produce a nonempty artifact.

With the `profiling` feature, `runtime.json` is replaced atomically about once a
second. It records allocator-used/reserved bytes, task names/IDs/generations,
spawners/OS threads, poll counts and CPU time, maximum poll wall time, executor
poll/wait totals, and successful allocation request count/volume per task.
Completed task generations remain in memory until exit. Snapshots can lag exit
and are not final or peak heap measurements; counter updates during a snapshot
are not a transaction. In-progress polls/waits are counted only when they return. Tasks not spawned through the Xila manager are absent.
Executor CPU includes task CPU; do not sum both. Poll CPU uses OS-thread CPU
time, while wall time can include blocking/descheduling. Allocation volume
counts requested bytes, not retained memory or ownership of later frees.
Profiling metadata and the reporter itself add overhead in feature-enabled runs.

- Samply profiles: `samply load target/profiles/cpu/run-1/cpu.json.gz`.
- Perf alternative: add `--tool perf` to CPU recording; inspect
  `cpu.perf.data` with `perf report` or Hotspot.
- Heap reports: open the generated `heaptrack*` data file with `heaptrack_gui`
  or `heaptrack_print`. Xila's custom `mmap` allocator needs the `heaptrack`
  feature; plain malloc interception misses its suballocations. Reallocation is
  reported as the allocator's actual allocate/copy/free operations. Requested
  sizes exclude allocator padding and Xila ABI allocation headers are included
  when the manager receives them. Heaptrack also captures ordinary libc calls.

For before/after consumption comparisons, rebuild **without** `profiling` and
`heaptrack` (retain only `prebuilt-applications` for the OS workload), repeat `measure` with the same scenario/inputs, and compare medians
and individual runs. Linux `wait4` CPU accounting includes waited-for children;
peak RSS is a high-water mark, not a sum of simultaneous process-tree memory.
Instrumented CPU/heap runs explain costs; their timings are not uninstrumented
benchmarks. No heavy profiler is run automatically on PRs or merge groups.
Native findings guide shared-code optimization, but validate gains on the actual
Wasm/embedded target before claiming target-specific improvements.

Local checks:

```bash
cargo test --manifest-path tools/profiling/Cargo.toml
cargo test -p xila-drivers-std --lib --features profiling,heaptrack
cargo test -p xila-native-example --bin native_example --features profiling,heaptrack
cargo run --manifest-path tools/profiling/Cargo.toml -- heap-smoke
```

The heaptrack smoke test compiles a local C API stand-in and verifies real native
allocation events, failed requests, realloc preservation and silence without the
feature. It does not require installing heaptrack or access to perf events.
