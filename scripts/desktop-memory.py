#!/usr/bin/env python3
"""Measure ritornel-gpui and ritornel-server memory under wall flinging and page visits.

Runs the desktop app against the invented 8,600-album library (never a real
library) with a silent `--output null` server and an isolated temporary `HOME`
so saved preferences (`remembered.json`) are neither read nor modified.
Flings both the Albums and Artists walls with momentum, opens and leaves
artist and album pages, and samples macOS `footprint` (plus optional
`vmmap --summary` checkpoints) for both processes separately.

With `--waits`, it also times how long each tile waits for its picture,
from the frame it is first shown to the first that draws its cover or
portrait (`apps/desktop/src/waits.rs`), and reports the median and the
slowest while flinging each wall, opening an artist's page, and going Back
to an artist's page and to the wall. `--slow` keeps the app and its
server on the efficiency cores (`taskpolicy -c background`), a rough
stand-in for a slower Mac: it slows the CPU, not the GPU.

Usage:
    scripts/desktop-memory.py --out-dir /tmp/ritornel-mem --label run1
    scripts/desktop-memory.py --out-dir /tmp/ritornel-mem --label run2 --seed 19
    RITORNEL_OFFSCREEN=1 scripts/desktop-memory.py --out-dir /tmp/ritornel-mem --label hidden
    RITORNEL_OFFSCREEN=1 scripts/desktop-memory.py --out-dir /tmp/ritornel-mem --label waits --waits --slow
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Mapping
from pathlib import Path


def parse_footprint(pids: list[int]) -> dict[int, dict]:
    """Run macOS `footprint -f bytes -j` on `pids` and return per-PID metrics."""
    fd, tmp_path = tempfile.mkstemp(prefix="ritornel-fp-", suffix=".json")
    os.close(fd)
    try:
        cmd = ["footprint", "-f", "bytes", "-j", tmp_path]
        for pid in pids:
            cmd.extend(["-p", str(pid)])
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=15)
        if res.returncode != 0 or not os.path.exists(tmp_path) or os.path.getsize(tmp_path) == 0:
            return {}
        with open(tmp_path, "r", encoding="utf-8") as handle:
            data = json.load(handle)
        out: dict[int, dict] = {}
        for proc in data.get("processes", []):
            pid = proc.get("pid")
            cats = proc.get("categories", {})

            # `footprint -j` already includes swapped/compressed bytes inside `dirty`.
            def cat_dirty(prefixes: tuple[str, ...]) -> int:
                total = 0
                for key, val in cats.items():
                    low = key.lower()
                    if any(low.startswith(prefix) for prefix in prefixes):
                        total += int(val.get("dirty", 0))
                return total

            def cat_reclaimable(prefixes: tuple[str, ...]) -> int:
                total = 0
                for key, val in cats.items():
                    low = key.lower()
                    if any(low.startswith(prefix) for prefix in prefixes):
                        total += int(val.get("reclaimable", 0))
                return total

            top_cats = sorted(
                (
                    (
                        key,
                        int(val.get("dirty", 0)),
                        int(val.get("reclaimable", 0)),
                        int(val.get("regions", 0)),
                    )
                    for key, val in cats.items()
                ),
                key=lambda item: item[1],
                reverse=True,
            )[:12]

            aux = proc.get("auxiliary", {})
            out[pid] = {
                "name": proc.get("name"),
                "footprint": int(proc.get("footprint", 0)),
                "phys_footprint": int(aux.get("phys_footprint", proc.get("footprint", 0))),
                "phys_footprint_peak": int(aux.get("phys_footprint_peak", 0)),
                "gpu_ioaccel": cat_dirty(("ioaccelerator", "agx", "opengl")),
                "iosurface": cat_dirty(("iosurface",)),
                "coreanimation": cat_dirty(("coreanimation",)),
                "malloc": cat_dirty(("malloc",)),
                "vm_allocate": cat_dirty(("vm_allocate",)),
                "cg_image": cat_dirty(("cg image", "imageio")),
                "gpu_reclaimable": cat_reclaimable(("ioaccelerator", "agx", "opengl")),
                "iosurface_reclaimable": cat_reclaimable(("iosurface",)),
                "top_categories": [
                    {"name": k, "dirty": d, "reclaimable": r, "regions": n}
                    for k, d, r, n in top_cats
                ],
            }
        return out
    except Exception as exc:
        print(f"footprint error: {exc}", file=sys.stderr)
        return {}
    finally:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass


def capture_vmmap(pid: int) -> str:
    try:
        res = subprocess.run(
            ["vmmap", "--summary", str(pid)],
            capture_output=True,
            text=True,
            timeout=20,
        )
        return res.stdout if res.returncode == 0 else f"vmmap failed: {res.stderr}"
    except Exception as exc:
        return f"vmmap error: {exc}"


# What each `waits` label times, and on which surface.
WAITS = {
    "albums-fling": "wall",
    "artists-fling": "wall",
    "artist-open": "artist",
    "artist-back": "artist",
    "wall-back": "wall",
}


def build_script(
    albums_secs: int,
    artists_secs: int,
    rest_secs: int,
    pages: bool,
    final_rest_secs: int,
    waits: bool = False,
) -> str:
    # `waits <label>` reports the tiles shown since the last report;
    # `waits -` only starts the count again.
    def timed(label: str) -> list[str]:
        return [f"waits {label}"] if waits else []

    steps = [
        "wall albums artist",
        *timed("-"),
        "mem phase-start",
        f"wait {int((1.5 + albums_secs) * 1000)}",
        "mem phase-albums-end",
        *timed("albums-fling"),
    ]
    if artists_secs > 0:
        steps.extend(
            [
                "wall artists",
                f"wait {int(artists_secs * 1000)}",
                "mem phase-artists-end",
                *timed("artists-fling"),
            ]
        )
    if rest_secs > 0:
        steps.extend([f"wait {int(rest_secs * 1000)}", "mem phase-post-fling-rest"])
    if pages:
        steps.extend(["wall artists", "wait 1000", *timed("-")])
        for idx in (0, 2, 4, 6, 8, 10):
            steps.extend(
                [
                    f"open {idx}",
                    "wait 1000",
                    *timed("artist-open"),
                    "record 0",
                    "wait 900",
                    *timed("-"),
                    "back",
                    "wait 600",
                    *timed("artist-back"),
                    "back",
                    "wait 600",
                    *timed("wall-back"),
                ]
            )
        steps.extend(["wall albums artist", "wait 1200", *timed("-")])
        for idx in (0, 2, 4, 6, 8, 10):
            steps.extend(
                [
                    f"open {idx}",
                    "wait 1000",
                    *timed("-"),
                    "up",
                    "wait 900",
                    *timed("artist-open"),
                    "record 0",
                    "wait 900",
                    *timed("-"),
                    "back",
                    "wait 500",
                    *timed("artist-back"),
                    "back",
                    "wait 500",
                    *timed("-"),
                    "back",
                    "wait 500",
                    *timed("wall-back"),
                ]
            )
        # 6-deep back-stack navigation chain, then back out to the wall.
        steps.extend(
            [
                "open 1",
                "wait 900",
                "up",
                "wait 900",
                "record 1",
                "wait 900",
                "up",
                "wait 900",
                "record 2",
                "wait 900",
                "record 0",
                "wait 900",
                "back",
                "wait 400",
                "back",
                "wait 400",
                "back",
                "wait 400",
                "back",
                "wait 400",
                "back",
                "wait 400",
                "back",
                "wait 400",
                "mem phase-pages-end",
            ]
        )
    if final_rest_secs > 0:
        steps.extend([f"wait {int(final_rest_secs * 1000)}", "mem phase-final-rest"])
    steps.append("quit")
    return "; ".join(steps)


def mib(num_bytes: int) -> float:
    return num_bytes / (1024 * 1024)


def git_revision(root: Path) -> str | None:
    try:
        res = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=5,
        )
        return res.stdout.strip() if res.returncode == 0 else None
    except Exception:
        return None


def ensure_built_and_invented(root: Path, skip_build: bool) -> tuple[Path, Path, Path]:
    app_dir = root / "apps" / "desktop"
    data_dir = app_dir / ".invented" / "data"
    server_bin = root / "target" / "release" / "ritornel-server"
    gpui_bin = app_dir / "target" / "release" / "ritornel-gpui"

    if not skip_build:
        subprocess.run(
            ["cargo", "build", "--locked", "--release", "--quiet", "-p", "ritornel-server"],
            cwd=root,
            check=True,
        )
        subprocess.run(
            [
                "cargo",
                "build",
                "--locked",
                "--release",
                "--quiet",
                "--manifest-path",
                str(app_dir / "Cargo.toml"),
            ],
            cwd=root,
            check=True,
        )

    if not (data_dir / ".invented").exists():
        (app_dir / ".invented" / "root").mkdir(parents=True, exist_ok=True)
        data_dir.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [
                str(server_bin),
                "invent",
                "--data-dir",
                str(data_dir),
                "--root",
                str(app_dir / ".invented" / "root"),
                "--albums",
                "8600",
                "--showcase",
            ],
            cwd=root,
            check=True,
        )
        (data_dir / ".invented").touch()

    return server_bin, gpui_bin, data_dir


def resolve_offscreen_display(env: Mapping[str, str] | None = None) -> dict[str, str]:
    """Return hidden-display env vars to overlay when RITORNEL_OFFSCREEN=1."""
    source = os.environ if env is None else env
    if source.get("RITORNEL_OFFSCREEN") != "1":
        return {}
    if source.get("RITORNEL_TEST_DISPLAY"):
        return {}
    hidden_display = shutil.which("hidden-display", path=source.get("PATH", ""))
    if hidden_display is None:
        raise RuntimeError(
            "RITORNEL_OFFSCREEN=1: no hidden display available. "
            "Set RITORNEL_TEST_DISPLAY to a display ID, or install a tool that "
            "provides one (such as hidden-display)."
        )
    try:
        res = subprocess.run(
            [hidden_display, "ensure"],
            capture_output=True,
            text=True,
            timeout=15,
            env=None if env is None else dict(env),
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError("hidden-display ensure timed out") from exc
    if res.returncode != 0:
        detail = (res.stderr or res.stdout).strip()
        msg = f"hidden-display ensure failed (exit {res.returncode})"
        if detail:
            msg = f"{msg}: {detail}"
        raise RuntimeError(msg)
    lines = res.stdout.splitlines()
    parts = lines[0].split() if len(lines) == 1 else []
    if (
        len(parts) != 3
        or not re.fullmatch(r"[0-9]+", parts[0])
        or not re.fullmatch(r"-?[0-9]+", parts[1])
        or not re.fullmatch(r"-?[0-9]+", parts[2])
    ):
        raise RuntimeError(
            f"hidden-display ensure returned unexpected output: {res.stdout.strip()!r}"
        )
    return {
        "RITORNEL_TEST_DISPLAY": parts[0],
        "RITORNEL_TEST_DISPLAY_AT": f"{parts[1]},{parts[2]}",
    }


def summarize_waits(stderr: str) -> dict:
    """Pool the `waits` lines the app printed, by label, on the surface each label times."""
    pooled: dict[str, dict] = {}
    for line in stderr.splitlines():
        if not line.startswith("waits {"):
            continue
        report = json.loads(line[len("waits ") :])
        label = report["label"]
        if WAITS.get(label) != report["surface"]:
            continue
        entry = pooled.setdefault(label, {"waited": [], "unpainted": 0, "waiting": []})
        entry["waited"].extend(report["waitedMs"])
        entry["unpainted"] += report["unpainted"]
        entry["waiting"].extend(report["stillWaitingMs"])

    def at(values: list[float], share: float) -> float:
        # The nearest rank, rounded half up as the app rounds it.
        return values[int((len(values) - 1) * share + 0.5)] if values else 0.0

    summary = {}
    for label in WAITS:
        if label not in pooled:
            continue
        entry = pooled[label]
        waited = sorted(entry["waited"])
        summary[label] = {
            "shown": len(waited),
            "atOnce": sum(1 for ms in waited if ms == 0),
            "medianMs": at(waited, 0.5),
            "p90Ms": at(waited, 0.9),
            "slowestMs": at(waited, 1.0),
            "unpainted": entry["unpainted"],
            "stillWaiting": len(entry["waiting"]),
            "longestStillWaitingMs": max(entry["waiting"], default=0.0),
        }
    return summary


def print_summary(result: dict) -> None:
    samples = result["samples"]
    if not samples:
        return
    first = samples[0]
    peak = max(samples, key=lambda s: s["app"]["footprint"])
    final = samples[-1]
    kernel_peak = max((s["app"].get("phys_footprint_peak", 0) for s in samples), default=0)
    srv_peak = max((s["server"]["footprint"] for s in samples if s.get("server")), default=0)
    srv_end = final["server"]["footprint"] if final.get("server") else 0
    print("\nSummary (MiB):")
    print(
        f"  First sample (t={first['t']:.1f}s): "
        f"App {mib(first['app']['footprint']):.1f} MiB "
        f"(GPU {mib(first['app']['gpu_ioaccel']):.1f}, "
        f"IOSurf {mib(first['app']['iosurface']):.1f}, "
        f"Malloc {mib(first['app']['malloc']):.1f})"
    )
    print(
        f"  Sampled peak (t={peak['t']:.1f}s): "
        f"App {mib(peak['app']['footprint']):.1f} MiB "
        f"(GPU {mib(peak['app']['gpu_ioaccel']):.1f}, "
        f"IOSurf {mib(peak['app']['iosurface']):.1f}, "
        f"Malloc {mib(peak['app']['malloc']):.1f}; "
        f"kernel peak {mib(kernel_peak):.1f} MiB)"
    )
    print(
        f"  Last sample (t={final['t']:.1f}s):  "
        f"App {mib(final['app']['footprint']):.1f} MiB "
        f"(GPU {mib(final['app']['gpu_ioaccel']):.1f}, "
        f"IOSurf {mib(final['app']['iosurface']):.1f}, "
        f"Malloc {mib(final['app']['malloc']):.1f})"
    )
    print(f"  Server:                 {mib(srv_end):.1f} MiB last / {mib(srv_peak):.1f} MiB peak")
    measure = result.get("measure") or {}
    if measure:
        print(
            f"  Smoothness:             p95 {measure.get('p95IntervalMs', 0):.2f} ms, "
            f"covers {measure.get('tileFramesWithCover', 0) * 100:.1f}%, "
            f"unfinished {measure.get('enteredUnfinishedShare', 0) * 100:.1f}%, "
            f">12ms {measure.get('framesOver12ms', 0)}, "
            f">20ms {measure.get('framesOver20ms', 0)}, "
            f">34ms {measure.get('framesOver34ms', 0)}"
        )
    waits = result.get("waits") or {}
    if waits:
        print("  Picture waits (ms):")
        for label, w in waits.items():
            print(
                f"    {label:<14} median {w['medianMs']:5.0f}, p90 {w['p90Ms']:5.0f}, "
                f"slowest {w['slowestMs']:5.0f}; {w['shown']} shown, {w['atOnce']} at once, "
                f"{w['unpainted']} gone before painted, {w['stillWaiting']} still waiting "
                f"(longest {w['longestStillWaitingMs']:.0f} so far)"
            )


def main() -> int:
    if platform.system() != "Darwin" or shutil.which("footprint") is None:
        print("desktop-memory.py requires macOS with the `footprint` tool.", file=sys.stderr)
        return 1

    repo_root = Path(__file__).resolve().parent.parent
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out-dir", type=Path, required=True, help="Directory for measurement files")
    ap.add_argument("--label", default="memory", help="Prefix for output files")
    ap.add_argument("--seed", type=int, default=7, help="RNG seed for --measure flings")
    ap.add_argument("--albums-secs", type=int, default=90, help="Seconds to fling Albums wall")
    ap.add_argument("--artists-secs", type=int, default=90, help="Seconds to fling Artists wall")
    ap.add_argument("--rest-secs", type=int, default=15, help="Seconds to rest after flinging")
    ap.add_argument("--final-rest-secs", type=int, default=20, help="Seconds to rest before quit")
    ap.add_argument("--no-pages", action="store_true", help="Skip artist/album page navigation")
    ap.add_argument("--interval", type=float, default=2.0, help="Sampling interval in seconds")
    ap.add_argument("--size", default="1400x900", help="Window size WxH")
    ap.add_argument("--vmmap-at", default="", help="Comma-separated seconds for vmmap checkpoints")
    ap.add_argument("--skip-build", action="store_true", help="Reuse existing release binaries")
    ap.add_argument(
        "--waits", action="store_true", help="Time how long tiles wait for their pictures"
    )
    ap.add_argument(
        "--slow", action="store_true", help="Keep the app and server on the efficiency cores (taskpolicy)"
    )
    args = ap.parse_args()

    try:
        display_env = resolve_offscreen_display()
    except RuntimeError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    args.out_dir.mkdir(parents=True, exist_ok=True)
    server_bin, gpui_bin, data_dir = ensure_built_and_invented(repo_root, args.skip_build)

    measure_secs = args.albums_secs + args.artists_secs
    measure_json_path = args.out_dir / f"{args.label}-measure.json"
    gpui_stderr_path = args.out_dir / f"{args.label}-gpui.stderr.log"
    server_log_path = args.out_dir / f"{args.label}-server.log"
    out_json = args.out_dir / f"{args.label}-timeseries.json"
    for stale in (measure_json_path, gpui_stderr_path, server_log_path, out_json):
        stale.unlink(missing_ok=True)

    vmmap_targets = sorted(float(x) for x in args.vmmap_at.split(",") if x.strip())
    if measure_secs > 0:
        for target in vmmap_targets:
            if target < 1.5 + measure_secs:
                print(
                    f"Warning: --vmmap-at {target}s falls inside the --measure window "
                    f"and may perturb frame timings.",
                    file=sys.stderr,
                )

    isolated_home = tempfile.mkdtemp(prefix="ritornel-mem-home-")
    gpui = None
    server = None
    server_log = None
    try:
        server_env = os.environ.copy()
        server_env["RITORNEL_TEST_NO_MEDIA_SESSION"] = "1"
        server_log = open(server_log_path, "wb")
        slow = ["taskpolicy", "-c", "background"] if args.slow else []
        server = subprocess.Popen(
            [
                *slow,
                str(server_bin),
                "serve",
                "--owned",
                "--data-dir",
                str(data_dir),
                "--listen",
                "127.0.0.1:0",
                "--output",
                "null",
                "--log-level",
                "warn",
            ],
            stdin=subprocess.PIPE,
            stdout=server_log,
            stderr=subprocess.STDOUT,
            env=server_env,
        )

        ready = False
        for _ in range(100):
            if server.poll() is not None:
                print("Server exited unexpectedly", file=sys.stderr)
                return 1
            lines = server_log_path.read_text(errors="replace").splitlines()
            if any(line.startswith("ready") for line in lines):
                ready = True
                break
            time.sleep(0.1)
        if not ready:
            print("Timed out waiting for server ready", file=sys.stderr)
            return 1

        script = build_script(
            args.albums_secs,
            args.artists_secs,
            args.rest_secs,
            not args.no_pages,
            args.final_rest_secs,
            args.waits,
        )

        gpui_env = os.environ.copy()
        gpui_env["HOME"] = isolated_home
        gpui_env.update(display_env)
        gpui_env["RITORNEL_GPUI_DEBUG"] = "1"
        if args.waits:
            gpui_env["RITORNEL_GPUI_WAITS"] = "1"

        gpui_cmd = [
            str(gpui_bin),
            "--data-dir",
            str(data_dir),
            "--size",
            args.size,
            "--seed",
            str(args.seed),
            "--label",
            args.label,
            "--script",
            script,
        ]
        if measure_secs > 0:
            gpui_cmd.extend(["--measure", str(measure_secs), "--out", str(measure_json_path)])
        gpui_cmd = [*slow, *gpui_cmd]

        expected_secs = (
            1.5
            + measure_secs
            + args.rest_secs
            + (0 if args.no_pages else 65)
            + args.final_rest_secs
        )
        deadline = time.monotonic() + expected_secs + 120.0

        t0 = time.monotonic()
        server_died = False
        timed_out = False
        with open(gpui_stderr_path, "wb") as gpui_stderr:
            gpui = subprocess.Popen(
                gpui_cmd,
                stderr=gpui_stderr,
                stdout=subprocess.DEVNULL,
                env=gpui_env,
            )

            vmmap_done: set[float] = set()
            samples = []
            print(
                f"Running {args.label} (seed={args.seed}, gpui={gpui.pid}, server={server.pid})...",
                flush=True,
            )
            print(
                f"{'t(s)':>6} | {'App MiB':>8} | {'GPU MiB':>8} | {'IOS MiB':>8} | {'Mal MiB':>8} | {'Srv MiB':>8}",
                flush=True,
            )

            time.sleep(1.1)
            next_sample = t0 + 1.1

            while gpui.poll() is None:
                now = time.monotonic()
                if now > deadline:
                    timed_out = True
                    print("Timed out waiting for desktop app to finish script", file=sys.stderr)
                    gpui.terminate()
                    break
                if server.poll() is not None:
                    server_died = True
                    print("Server exited unexpectedly during benchmark", file=sys.stderr)
                    gpui.terminate()
                    break
                if now < next_sample:
                    time.sleep(min(0.1, next_sample - now))
                    continue
                elapsed = now - t0
                fp = parse_footprint([gpui.pid, server.pid])
                app_fp = fp.get(gpui.pid)
                srv_fp = fp.get(server.pid)
                if app_fp:
                    samples.append({"t": round(elapsed, 2), "app": app_fp, "server": srv_fp})
                    print(
                        f"{elapsed:6.1f} | {mib(app_fp['footprint']):7.1f}M | "
                        f"{mib(app_fp['gpu_ioaccel']):7.1f}M | {mib(app_fp['iosurface']):7.1f}M | "
                        f"{mib(app_fp['malloc']):7.1f}M | "
                        f"{mib(srv_fp['footprint']) if srv_fp else 0.0:7.1f}M",
                        flush=True,
                    )

                for target in vmmap_targets:
                    if target not in vmmap_done and elapsed >= target:
                        vmmap_done.add(target)
                        vm_out = capture_vmmap(gpui.pid)
                        (args.out_dir / f"{args.label}-vmmap-t{int(target)}s.txt").write_text(
                            vm_out, encoding="utf-8"
                        )

                next_sample = max(next_sample + args.interval, time.monotonic() + 0.1)

            rc = gpui.wait()

        measure_data = None
        measure_error = None
        if measure_secs > 0:
            if not measure_json_path.exists():
                measure_error = f"Expected measure report missing: {measure_json_path}"
            else:
                try:
                    measure_data = json.loads(measure_json_path.read_text(encoding="utf-8"))
                except Exception as exc:
                    measure_error = f"Invalid measure JSON: {exc}"

        result = {
            "label": args.label,
            "git_rev": git_revision(repo_root),
            "skip_build": args.skip_build,
            "seed": args.seed,
            "size": args.size,
            "albums_secs": args.albums_secs,
            "artists_secs": args.artists_secs,
            "rest_secs": args.rest_secs,
            "pages": not args.no_pages,
            "final_rest_secs": args.final_rest_secs,
            "exit_code": rc,
            "measure": measure_data,
            "waits": summarize_waits(gpui_stderr_path.read_text(errors="replace"))
            if args.waits
            else None,
            "slow": args.slow,
            "samples": samples,
        }
        out_json.write_text(json.dumps(result, indent=2), encoding="utf-8")
        print_summary(result)
        print(f"Saved {len(samples)} samples to {out_json} (exit={rc})", flush=True)

        if server_died or timed_out or rc != 0:
            return rc if rc != 0 else 1
        if not samples:
            print("Benchmark produced zero footprint samples", file=sys.stderr)
            return 1
        if measure_error is not None:
            print(measure_error, file=sys.stderr)
            return 1
        return 0
    finally:
        if gpui is not None and gpui.poll() is None:
            gpui.terminate()
            try:
                gpui.wait(timeout=5)
            except subprocess.TimeoutExpired:
                gpui.kill()
        if server is not None:
            if server.stdin:
                try:
                    server.stdin.close()
                except OSError:
                    pass
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.terminate()
        if server_log is not None:
            server_log.close()
        shutil.rmtree(isolated_home, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
