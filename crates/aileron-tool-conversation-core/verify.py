#!/usr/bin/env python3
"""Verify the production source and reject behavioral mutations; retain all inputs.

Usage: python3 crates/aileron-tool-conversation-core/verify.py VERUS NEW_EVIDENCE_DIR
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

verus = Path(sys.argv[1]).resolve()
output = Path(sys.argv[2]).resolve()
output.mkdir(parents=True, exist_ok=False)
source = Path(__file__).with_name("src").joinpath("lib.rs").read_text()
version = subprocess.check_output([str(verus), "--version"], text=True)
if "0.2026.10.04.426d8b0" not in version:
    raise SystemExit("unexpected verifier version")
(output / "version.txt").write_text(version)

mutations = {
    "stale-completion": (
        "if matches!(self.phase, Phase::Idle) || turn.serial != self.serial { return false; }",
        "if matches!(self.phase, Phase::Idle) { return false; }",
    ),
    "cancel-keeps-running": (
        "self.phase = Phase::Idle;\n        self.pending = Vec::new();",
        "self.pending = Vec::new();",
    ),
    "wrapping-reuse": (
        "if self.serial == u64::MAX { return None; }\n        self.serial += 1;",
        "self.serial = self.serial.wrapping_add(1);",
    ),
    "duplicate-results": (
        "if ids[j].as_str() == ids[i].as_str() { return false; }",
        "if false { return false; }",
    ),
    "unknown-results": (
        "if !contains(pending, &results[i]) { return false; }",
        "if false { return false; }",
    ),
}
cases = {"production": source}
for name, (old, new) in mutations.items():
    assert source.count(old) == 1, (name, "mutation no longer matches")
    cases[name] = source.replace(old, new)

summary = []
for name, text in cases.items():
    directory = output / name
    directory.mkdir()
    path = directory / "core.rs"
    path.write_text(text)
    command = [str(verus), "--crate-type", "lib", "--edition", "2024",
               "--triggers-mode", "silent", str(path)]
    started = time.monotonic()
    result = subprocess.run(command, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=120)
    (directory / "output.txt").write_text(result.stdout)
    passed = (result.returncode == 0 and "0 errors" in result.stdout) if name == "production" else (
        result.returncode != 0 and "verification results::" in result.stdout
        and ("postcondition not satisfied" in result.stdout or "invariant not satisfied" in result.stdout)
    )
    record = dict(case=name, command=command, exit_code=result.returncode,
                  elapsed_seconds=time.monotonic() - started,
                  source_sha256=hashlib.sha256(text.encode()).hexdigest(), expected_outcome=passed)
    summary.append(record)
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(name, "PASS" if passed else "FAIL", flush=True)
    if not passed:
        raise SystemExit(result.stdout)
