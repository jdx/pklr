#!/usr/bin/env python3
"""Compare pklr with the `pkl` CLI on Apple's language snippet tests.

Each file under pkl-core/src/test/files/LanguageSnippetTests/input in the
apple/pkl repository (checked out at the tag matching `pkl --version`) is
evaluated by `pkl eval -f json` and by pklr's `eval_json` example, and the
JSON results are compared.

Files are classified as:

  match        pklr's JSON equals pkl's
  mismatch     both succeed but the JSON differs
  error        pkl succeeds, pklr fails
  timeout      pklr did not finish in time
  expected-err pkl rejects the file and pklr fails too
  missed-err   pkl rejects the file but pklr succeeds
  skipped      pkl fails for a reason outside the language: the value can't be
               rendered as JSON, or the test needs a project, the network or a
               module that isn't in the checkout

Usage:
  scripts/conformance.py                 # summary
  scripts/conformance.py -v              # also list every non-matching file
  scripts/conformance.py basic/ lambdas  # only files whose path contains these
  scripts/conformance.py --save base.json; ...; scripts/conformance.py --compare base.json
"""

import argparse
import concurrent.futures
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "conformance"
SNIPPETS = "pkl-core/src/test/files/LanguageSnippetTests/input"


def pkl_version():
    out = subprocess.run(["pkl", "--version"], capture_output=True, text=True, check=True).stdout
    m = re.search(r"Pkl (\S+)", out)
    if not m:
        sys.exit(f"could not parse `pkl --version`: {out!r}")
    return m.group(1)


def checkout(version, repo):
    if repo:
        return Path(repo)
    dest = WORK / f"pkl-{version}"
    if not (dest / SNIPPETS).is_dir():
        WORK.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["git", "clone", "-q", "--depth", "1", "--branch", version,
             "https://github.com/apple/pkl", str(dest)],
            check=True,
        )
    return dest


def build():
    # Ask cargo where it put the binary, so CARGO_TARGET_DIR and the like work.
    # With --message-format=json, compiler diagnostics arrive on stdout as JSON;
    # stderr (cargo's own errors) is passed through.
    p = subprocess.run(
        ["cargo", "build", "-q", "--release", "--example", "eval_json",
         "--message-format=json-diagnostic-rendered-ansi"],
        cwd=ROOT, stdout=subprocess.PIPE, text=True,
    )
    messages = [json.loads(line) for line in p.stdout.splitlines() if line.startswith("{")]
    if p.returncode != 0:
        for msg in messages:
            if msg.get("reason") == "compiler-message":
                print(msg["message"]["rendered"], end="", file=sys.stderr)
        sys.exit(f"cargo build failed (exit {p.returncode})")
    for msg in messages:
        if (msg.get("reason") == "compiler-artifact"
                and msg["target"]["name"] == "eval_json" and msg.get("executable")):
            return Path(msg["executable"])
    sys.exit("cargo did not report the eval_json executable")


def run(cmd, timeout):
    try:
        p = subprocess.run(
            cmd, capture_output=True, encoding="utf-8", errors="replace", timeout=timeout
        )
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return None, "", "timed out"


def normalize(v):
    """Make JSON values comparable with ==: integral floats equal their ints,
    but booleans stay distinct from 0 and 1 (Python treats True == 1)."""
    if isinstance(v, bool):
        return ("bool", v)
    if isinstance(v, float) and v.is_integer():
        return int(v)
    if isinstance(v, dict):
        return {k: normalize(x) for k, x in v.items()}
    if isinstance(v, list):
        return [normalize(x) for x in v]
    return v


# pkl errors that say nothing about whether pklr should accept the file.
SKIP_ERRORS = re.compile(
    r"Cannot render|as JSON|no project found|Exception when making request|"
    r"Cannot find module|has invalid syntax|Cannot find resource|I/O error"
)


def pkl_message(stderr):
    lines = [line.strip() for line in stderr.splitlines() if line.strip()]
    if lines and lines[0].startswith("–– Pkl Error ––"):
        lines = lines[1:]
    return lines[0][:200] if lines else ""


def first_line(text):
    lines = [line.strip() for line in text.strip().splitlines() if line.strip()]
    return lines[0][:200] if lines else ""


def diff_paths(a, b, path=""):
    if isinstance(a, dict) and isinstance(b, dict):
        out = []
        for k in sorted(set(a) | set(b)):
            if k not in b:
                out.append(f"{path}/{k} (missing)")
            elif k not in a:
                out.append(f"{path}/{k} (extra)")
            else:
                out += diff_paths(a[k], b[k], f"{path}/{k}")
        return out
    if isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        out = []
        for i, (x, y) in enumerate(zip(a, b)):
            out += diff_paths(x, y, f"{path}[{i}]")
        return out
    return [] if a == b else [path or "/"]


def check(rel, inputs, exe, timeout):
    path = str(inputs / rel)
    prc, pout, perr = run(["pkl", "eval", "-f", "json", path], timeout)
    if prc is None:
        return "skipped", "pkl timed out"
    if prc != 0 and SKIP_ERRORS.search(perr):
        return "skipped", pkl_message(perr)
    rrc, rout, rerr = run([str(exe), path], timeout)
    if prc != 0:
        if rrc is None:
            return "timeout", ""
        return ("expected-err", "") if rrc != 0 else ("missed-err", pkl_message(perr))
    if rrc is None:
        return "timeout", ""
    if rrc != 0:
        return "error", first_line(rerr)
    try:
        a = normalize(json.loads(pout))
        b = normalize(json.loads(rout))
    except json.JSONDecodeError:
        # The module sets its own output renderer, so pkl's output isn't JSON.
        return "skipped", "pkl output is not JSON"
    if a == b:
        return "match", ""
    return "mismatch", ", ".join(diff_paths(a, b)[:5])


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("filters", nargs="*", help="only files whose path contains one of these")
    ap.add_argument("-v", "--verbose", action="store_true", help="list every non-matching file")
    ap.add_argument("--repo", help="existing apple/pkl checkout to use")
    ap.add_argument("--timeout", type=float, default=20)
    ap.add_argument("-j", "--jobs", type=int, default=os.cpu_count())
    ap.add_argument("--save", help="write per-file results to this JSON file")
    ap.add_argument("--compare", help="report changes against results saved with --save")
    args = ap.parse_args()

    # Read the baseline first: --save may point at the same file.
    base = json.loads(Path(args.compare).read_text()) if args.compare else None

    version = pkl_version()
    inputs = checkout(version, args.repo) / SNIPPETS
    if not inputs.is_dir():
        sys.exit(f"no snippet tests at {inputs}")
    exe = build()
    files = sorted(str(p.relative_to(inputs)) for p in inputs.rglob("*.pkl"))
    if args.filters:
        files = [f for f in files if any(x in f for x in args.filters)]
    if not files:
        sys.exit("no snippet tests selected")

    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        results = dict(zip(files, pool.map(lambda f: check(f, inputs, exe, args.timeout), files)))

    counts = {}
    for status, _ in results.values():
        counts[status] = counts.get(status, 0) + 1
    if args.verbose:
        for f, (status, detail) in results.items():
            if status not in ("match", "expected-err", "skipped"):
                print(f"{status:12} {f}  {detail}")
        print()
    order = ["match", "mismatch", "error", "timeout", "expected-err", "missed-err", "skipped"]
    print(f"pkl {version}: " + ", ".join(f"{s} {counts.get(s, 0)}" for s in order))
    comparable = sum(counts.get(s, 0) for s in order if s != "skipped")
    passing = counts.get("match", 0) + counts.get("expected-err", 0)
    print(f"passing: {passing}/{comparable}")

    if args.save:
        Path(args.save).write_text(json.dumps({f: s for f, (s, _) in results.items()}, indent=1))
    if base is not None:
        good = {"match", "expected-err"}
        fixed = [f for f, (s, _) in results.items() if s in good and base.get(f) not in good]
        broke = [f for f, (s, _) in results.items() if s not in good and base.get(f) in good]
        for f in fixed:
            print(f"fixed   {f}")
        for f in broke:
            print(f"BROKE   {f}  ({results[f][0]}: {results[f][1]})")
        print(f"{len(fixed)} fixed, {len(broke)} broken vs {args.compare}")
        if broke:
            sys.exit(1)


if __name__ == "__main__":
    main()
