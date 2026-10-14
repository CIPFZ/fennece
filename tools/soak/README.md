# tools/soak — REL-05 soak tooling

The 24-hour soak (AGENT.md §7.1 REL-05) has three parts:

* `soak-churn`, a small Rust tool (workspace package `soak`, std + anyhow + chrono). It
  creates file changes on NTFS, which reach fennec as USN events, and it renders the report.
* `soak.ps1` starts fennec, starts the churn, samples the processes into a CSV, and
  writes the report at the end.
* `cpu-probe.ps1` measures fennec CPU in the four UI states (hidden or visible window,
  empty search or a typed query), idle and under churn.

Pass criteria: RSS growth < 10 % over 24 h, stable handle count, no crash files.

## Commands for the 24 h run (0.2.0 MSI installed, FennecService running)

Run these in PowerShell on the VM. Any context works, including a SYSTEM `win_job`,
because `-Detach` returns at once:

```powershell
# 1. The tools: build in a checkout of the release commit and copy them out.
cargo build --release -p soak
New-Item -ItemType Directory -Force D:\soak-tools | Out-Null
Copy-Item target\release\soak-churn.exe, tools\soak\soak.ps1, tools\soak\cpu-probe.ps1 D:\soak-tools\

# 2. Start: 24 h, 5-min samples, 50 changes/s below D:\soak, own instance "soak".
& D:\soak-tools\soak.ps1 -Detach -InstallDir 'C:\Program Files\Fennec' -Instance soak `
    -ChurnRoot D:\soak -Duration 24h -IntervalMinutes 5 -OutDir D:\soak-out -ExpectService

# 3. Progress (one line per sample); stop early with: New-Item D:\soak-out\soak.stop
Get-Content (Get-ChildItem D:\soak-out\soak-*.log -Exclude *churn* | Select-Object -Last 1).FullName -Tail 5

# 4. When it ends, soak.ps1 has already written D:\soak-out\soak-<start>.md (+ -charts\*.svg).
#    The report for the repository (run from the checkout):
D:\soak-tools\soak-churn.exe report D:\soak-out\soak-<start>.csv `
    --out docs\reports\soak-0.2.0.md --svg-dir docs\reports\soak-0.2.0 `
    --title "Fennec 0.2.0 soak (REL-05)"
```

Notes for that run:

* `-Detach` registers a scheduled task (`fennec-soak-<instance>`) that runs as the user
  logged on at the console. The task is interactive and elevated, with no execution time
  limit, so fennec.exe starts on the real desktop. Doing this by hand with
  `schtasks /Create /RU Administrator /IT /RL HIGHEST /TR …` works too (verified on the
  VM, no password needed), but `/TR` is cut at 261 characters, which truncated the
  parameters in the first attempt. The script uses the ScheduledTasks cmdlets instead.
* Started in session 0 without `-Detach`, the script still runs. Only fennec.exe goes to
  the desktop, through a one-shot interactive task.
* **Service path:** on this VM the built-in Administrator always gets a full token. A
  task with `-RunLevel LIMITED` was still `mode="direct"` in the fennec log (checked).
  So fennec run by Administrator indexes NTFS itself, and FennecService is only sampled
  (`svc_*` columns). A soak of the service path needs fennec in the interactive session
  of a standard user (fnuser logged on at the console), with `-InteractiveUser fnuser`.
* At the end the script stops fennec with `fes -instance <i> -exit`, which works from the
  same user. From SYSTEM the IPC answers `Denied`, so the script kills fennec after 60 s.
  `-KeepApp` leaves fennec running.
* fennec starts hidden in the tray (`-AppArgs '-startup'`). `-AppArgs ''` keeps the
  window visible, which costs more CPU under churn (see the CPU table below).

## soak-churn

```text
soak-churn [run] --root DIR [--rate 50] [--duration 24h] [--seed N] [--files 20000]
           [--dirs 100] [--big 10000] [--subtree 15000] [--subtree-every 15m]
           [--stop-file PATH] [--log FILE]
soak-churn prepare --root DIR [size options]      prefill only
soak-churn report SAMPLES.csv [--out FILE.md] [--svg-dir DIR] [--min-hours 24] [--warmup 0m] [--title T]
soak-churn seconds DURATION
```

Layout below `--root`:

| Folder | Contents |
|---|---|
| `work\dNNN\` | Working set of `--files` files. |
| `big\` | Exactly `--big` children, always. |
| `burst\gN\sNNN\` | A `--subtree`-entry subtree, deleted and recreated every `--subtree-every`. |

* **Steady changes.** A token bucket holds `--rate` changes/s, with one second of
  burst so stalls are made up. Creates, renames and deletes come in about equal shares.
  The create/delete split leans toward the target file count, so the tree size stays
  constant. A quarter of the renames go to `big\`.
* **Recovery.** All state is re-derived from the directory listing at start. A kill or
  Ctrl+C at any moment is harmless (there is no handler; nothing needs flushing). The
  run also ends at `--duration` or when `<root>\soak-churn.stop` appears, which is
  checked every second.
* **Logging.** One line per minute: `t= ops= rate= create= rename= delete= errors=
  work= big= cycles= subtree_deleted= subtree_created=`.
* **Compaction load.** Live compaction needs ≥ 10,000 dead rows that are also ≥ 20 % of
  all rows (checked every 30 s). With N live entries, that means about 0.25 N dead rows.
  The defaults kill about 1,000 rows/min through deletes plus about 1,000 rows/min
  through the subtree. On the VM (N ≈ 650k) that gives a compaction about every 80 min,
  so about 17 in 24 h. The trials below used `--subtree 20000 --subtree-every 5m` to get
  one inside 40 minutes.

Unit tests (19) cover: the token bucket (rate held, no burst after a stall), the mix
(working set stays within 5 %, all three kinds about 1/3 each), the period schedule
(no catch-up), subtree shape, PRNG, durations, prefill/recovery/re-layout on disk,
stop by duration and by stop file, CSV parsing, window growth/slope, hourly rows,
restarts, and the pass/fail/trial verdicts and SVG.

## soak.ps1

Parameters: `-InstallDir` (default `C:\Program Files\Fennec`), `-Instance`, `-DataDir`,
`-ChurnRoot`, `-Duration`, `-IntervalMinutes` (default 5), `-Rate` (50), `-ChurnArgs`,
`-OutDir`, `-ExpectService`, `-AppArgs`, `-InteractiveUser`, `-RunLevel`, `-KeepApp`,
`-NoRestart`, `-LoadTimeoutMinutes`, `-Detach`. Each one is described in the script header.

`-DataDir` defaults to the instance's data folder: `<InstallDir>\data\…` for a portable
copy, else `%APPDATA%\Fennec\instances\<i>` of the console user.

The script runs these steps in order:

1. `soak-churn prepare` (unthrottled prefill).
2. Start fennec, or attach to a running one with the same exe and instance.
3. Wait until the `fes -get-result-count` total holds still for 10 s.
4. Take sample 0 and start `soak-churn run`.
5. Every interval, write one CSV row (columns below). fennec is restarted if it exits,
   which shows up in the note column and as a PID change.
6. At the end: stop the churn, stop fennec, take the final sample, run `soak-churn report`.

CSV columns:

| Column | Meaning |
|---|---|
| `time`, `elapsed_s` | Sample time and seconds since sample 0. |
| `app_pid` | fennec.exe PID; empty if it was not running. |
| `app_ws_mb`, `app_private_mb` | fennec working set and private bytes. |
| `app_cpu_s`, `app_cpu_pct` | CPU time, and % of one core since the last sample. |
| `app_handles`, `app_threads` | Handle and thread counts. |
| `svc_*` | The same values for the FennecService process (PID from SCM). |
| `entries` | `fes -get-result-count`. |
| `soak_entries` | Count for `<ChurnRoot>\`. |
| `index_rows` | Rows including tombstones, from the newest "database saved rows=" log line. |
| `crash_files` | `crash-*.log` newer than the start, in `<data>\logs` and `%ProgramData%\Fennec\logs`. |
| `compactions` | "index compacted while running" log lines since the start. |
| `note` | Events such as restarts. |

## Report (`soak-churn report`)

The report has:

* Criteria with PASS/FAIL.
* first/last/min/max/mean, raw and window growth, and slope per hour, for each series.
* CPU average (Δ CPU / Δ wall).
* Restarts and crash files.
* A sample table: hourly rows when there are more than 48 samples, else every sample.
* Optional SVG charts (memory, handles) without any external tool.

Window growth compares the mean of the first window with the mean of the last window.
A window is 1 h, or a quarter of the run if the run is shorter than 4 h. `--warmup`
skips the start of the first window.

Criteria:

| Criterion | Rule |
|---|---|
| Working set | Window growth < 10 %. |
| Private bytes | Window growth < 10 %. Added because Windows trims the working set, which can hide a leak. |
| Handles | Window growth < 10 %, or at most 50 more handles. |
| Crash files | None. |
| fennec exits | None. |
| Live compaction | At least one. |
| Run length | ≥ `--min-hours`, else the overall verdict says TRIAL. |

## Trial runs (Windows Server 2022 VM, 10 logical CPUs)

All trials used:

* a release build from this branch, `--no-default-features`;
* a portable copy in `D:\soak-rel05-bin`, `-Instance rel05`, elevated, on the console
  desktop through `-Detach`, hidden in the tray;
* churn root `D:\soak-rel05`, 50 changes/s plus `--subtree 20000 --subtree-every 5m`,
  samples every 2 min;
* C:, D: and R: indexed, about 560–660k entries.

Three other agents' fennec instances indexed the same volumes during the trials, so the
CPU baselines are noisy.

### Trial 1: master 7090085 (before the fixes), 60 min, 31 samples → FAIL

CSV excerpt:

```text
time,elapsed_s,app_pid,app_ws_mb,app_private_mb,app_cpu_s,app_cpu_pct,app_handles,app_threads,…,entries,soak_entries,crash_files,compactions,note
2026-10-14 11:57:26,0,8488,215.6,318.3,17.95,,460,42,…,562574,45061,0,0,"start"
2026-10-14 11:59:27,121,8488,210.2,336.8,459.03,365.37,449,35,…,562612,45069,0,0,""
2026-10-14 12:29:30,…,8488,186.4,362.4,…,451.9,446,34,…,638423,50119,0,0,""
2026-10-14 12:57:30,3604,8488,213.0,375.2,12006.61,466.85,445,34,…,621547,30076,0,0,"end: duration reached"
```

Report criteria:

| Criterion | Measured | Result |
|---|---|---|
| Working set growth < 10 % | 158.4 → 206.1 MiB (+30.1 %) | FAIL |
| Private bytes growth < 10 % | 342.4 → 371.4 MiB (+8.5 %); raw +17.9 % | PASS |
| Handles stable | 448.6 → 445.4 (−0.7 %) | PASS |
| No crash files | 0 | PASS |
| fennec never exited | 0 | PASS |
| Live compaction ran | 0 times | FAIL |

Average CPU was 333 % of one core, rising from about 75 % to about 470 % as the
tombstones piled up. Two problems showed:

1. The hidden window re-ran the current (empty) search on every index change.
2. Live compaction never ran. At 12:32 the index had 839,167 rows against 638,426 live,
   about 24 % dead, and it stayed above the threshold from about 12:22 to the end.
   `compact_live` threw away every attempt because the index changed during its copy.

Both were reported to the lead and are fixed on master in efa1ebf and 706fac5.

### Trial 2: master efa1ebf + 706fac5, 40 min, 21 samples → PASS (trial only)

| Criterion | Measured | Result |
|---|---|---|
| Working set growth < 10 % | 158.6 → 141.1 MiB (−11.0 %) | PASS |
| Private bytes growth < 10 % | 346.8 → 371.2 MiB (+7.0 %); raw 329.1 → 377.2 (+14.6 %) | PASS |
| Handles stable | 387.2 → 414.3 (+7.0 %); raw 396 → 448 | PASS |
| No crash files | 0 | PASS |
| fennec never exited | 0 | PASS |
| Live compaction ran | 1 time: 13:57:28, removed 164,702 rows in 250 ms | PASS |

* Index rows climbed 643,881 → 802,483, then fell to 662,761 after the compaction, so
  rows stay bounded.
* Average CPU was 5.3 % of one core, down from 333 %.
* After the compaction, handles stepped from about 381 to 448 and stayed there.
* Private bytes did not fall after the compaction.

### Trial 3: same build, 60 min, 31 samples → FAIL (handles)

| Criterion | Measured | Result |
|---|---|---|
| Working set growth < 10 % | 141.2 → 129.2 MiB (−8.5 %) | PASS |
| Private bytes growth < 10 % | 351.1 → 377.8 MiB (+7.6 %); raw 327.1 → 384.1 (+17.4 %) | PASS |
| Handles stable | 383.4 → 447.9 (+16.8 %) | FAIL |
| No crash files | 0 | PASS |
| fennec never exited | 0 | PASS |
| Live compaction ran | 1 time: 14:57:18, removed 180,053 rows | PASS |

Average CPU was 5.8 % of one core. CSV excerpt:

```text
time,app_pid,app_ws_mb,app_private_mb,app_handles,app_threads,entries,index_rows,compactions
2026-10-14 14:17:00,15708,217.5,327.1,396,43,638989,,0
2026-10-14 14:57:00,15708,161.9,365.8,381,34,658773,818398,0
2026-10-14 14:59:00,15708,159.1,373.7,448,35,658748,818398,1
2026-10-14 15:17:05,15708,156.9,384.1,447,34,638774,738142,1
```

### Handles at live compaction (targeted run: `--subtree 150000 --subtree-every 3m`)

To get many compactions quickly, two shorter runs used this heavier churn.

**Run A** (25 min) had 3 compactions:

| Compaction | Handles |
|---|---|
| 1 | 379 → 448 |
| 2 | 448 → 448 |
| 3 | 448 → 531 |

**Run B** recorded handles by type (NtQuerySystemInformation) around 2 compactions:

| Compaction | Handles | Change by type |
|---|---|---|
| 1 | 356 → 429 | Key +28, Mutant +16, EtwRegistration +8, Event +7, File +6, IoCompletion +4, Section +2 |
| 2 | no change | — |

* About 70–80 handles appear at some compactions, not all. The mix of registry keys,
  mutexes and ETW registrations looks like a component (DLL) being initialised.
  Something in the work after a compaction, in the app or the stores, probably does it
  lazily.
* Private bytes peaked at 661 MiB during a compaction of about 790k rows (the copy),
  then fell back to about 400 MiB.
* Private bytes never drop below the pre-compaction level.

### Early signals for the 24 h run

* **Handles.** Each step is +67 to +83, against a 50-handle slack on about 380 handles.
  If such steps recur over 24 h, the handles criterion fails. Run A suggests they can
  recur (2 steps in 3 compactions).
* **Private bytes.** They rose about 35–45 MiB/h in every 40–60 min run (raw +15–17 %),
  even with compaction working. Window growth stayed under 10 %, but only the 24 h run
  shows whether this levels off.
* **Working set.** It swings between 90 and 220 MiB as Windows trims it, so it says
  little about leaks.
* **CPU.** With the window visible and the empty search, fennec still uses about 0.7
  core under 50 changes/s (table below).

### CPU by UI state (`cpu-probe.ps1`)

Each cell is 60 s at 50 changes/s, in % of one core. The probe passes the query as
`-s notepad`.

| State | 7090085 idle | 7090085 churn | efa1ebf idle | efa1ebf churn |
|---|---|---|---|---|
| Hidden in tray, empty search | 8.7 | **430.3** | 3.3 | 5.8 |
| Hidden in tray, `notepad` | 2.9 | 11.8 | 3.1 | 2.7 |
| Window visible, empty search | 164.8 | 269.3 | 4.7 | **71.6** |
| Window visible, `notepad` | 6.0 | 10.9 | 6.4 | 9.6 |

"Idle" means no churn from this tool; other agents' builds still changed files.

On efa1ebf, a visible window showing the empty search (all ≈ 650k entries) still uses
about 0.7 core under 50 changes/s. That is the rate-limited re-search, at most once per
clamp(4 × last, 250 ms, 1 s).

Command used:

```powershell
cpu-probe.ps1 -InstallDir D:\soak-rel05-bin -Instance rel05cpu -ChurnRoot D:\soak-rel05 -OutDir D:\soak-rel05-out
```

Run it on the desktop, elevated, while holding the GUI lock.

## Root causes of the trial signals (REL-05, second pass)

Found with `handle-probe.py` (this folder: handle changes by type and name, thread
names, DLL loads, private bytes; Python with ctypes, no Sysinternals) and a counting
allocator (Linux harness and a temporary Windows build).

| Signal | Cause | Fix |
|---|---|---|
| +35 to +80 handles at most live compactions | `-startup` did not keep the window hidden: eframe creates every window hidden and shows it after painting the first frame, whatever `with_visible(false)` said. Fennec treated it as hidden (no refresh on index changes), but the window was on screen and drawn. A compaction re-ran the search, the rows were drawn again and their icons were read through the shell on the thumbnail threads, which loaded more shell components each time: Explorer icon-cache mutexes (16 per load), known-folder and namespace keys, `thumbcache.dll`, namespace extensions. These handles are never released (a separate test, 712 icons on one thread, gave a one-time +112 to +139 handles whether or not the thread pumped messages). | The window is hidden again after the first frame, and a hidden window draws nothing (eframe still runs the UI of hidden windows). |
| Private bytes +35–45 MiB/h, no drop after compactions | (1) After a load or a live compaction every index column is exactly full, so the next new entry doubled all of them, and Windows commits that capacity. (2) A live compaction cloned the whole index and then compacted the clone, so three copies existed at the peak. (3) The index journal (`journal.tsv`, up to 1.1 M records) was trimmed every ~100 k changes by reading the whole file into memory: 323 MiB and 3.3 M allocations each time, under the index write lock. | Large columns grow by a quarter. The compacted index is built straight from the live one, and the old one is freed outside the lock. The journal is trimmed by streaming, outside the lock. |

### A/B run: master 3d10abe vs the fixes, same time, same load

* Both builds ran at the same time for 100 min, hidden in the tray (`-startup`).
* Each used its own churn root, with `--subtree 50000 --subtree-every 4m`. Both indexed
  both roots, so each saw about 100 changes/s plus two 50 k subtrees every 4 min.
* Each had 12 live compactions, and its index varied between 750 k and 1.03 M rows.

| | master 3d10abe | fixes |
|---|---|---|
| Verdict (`soak-churn report`, trial) | **FAIL** | **PASS** |
| Handles, first → last sample | 402 → 499 | 371 → 358 |
| Handle steps at compactions | +47, +28, +12, +28, then flat at ~499 | none (357–364 throughout) |
| Private bytes, window growth | +15.2 % (421 → 486 MiB) | +2.6 % (330 → 339 MiB) |
| Private bytes, max | 565 MiB | 347 MiB |
| Private bytes, means per 20 min | 418, 454, 484, 477, 488 | 330, 333, 335, 336, 339 |
| Working set, window growth | +18.6 % | −5.0 % |
| CPU, mean | 9.8 % of one core | 4.6 % |

Each compaction removed about 215 k rows. It took 237–393 ms with the fixes (median
266 ms) and 311–414 ms on master (median 332 ms), because there is no full clone now.
