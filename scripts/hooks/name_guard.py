"""Pre-push gate: does this push name anyone it should not.

WHY. This repository is public and the tool is generic; whose tickets taught us a
lesson is not ours to publish. The names were scrubbed from history once — and came
straight back, not through code but through COMMIT MESSAGES: eleven of them, written
by the same hand that had done the scrubbing that morning. One of those commits was
titled "the client names left the fixtures again".

WHY HASHES AND NOT A LIST. The first version of this file spelled the forbidden words
outright, the way the guard it was modelled on does. That guard lives in a private
repository; this one does not — so a plain list would publish, in the repository we
are cleaning, exactly the names we are cleaning out of it. Caught by the check that
verifies the push from a fresh clone: one file still matched, and it was this one.
So the words live here only as truncated SHA-256 of their lowercase form. The guard
can still say WHICH word it found, because it found it; it cannot tell anyone what is
on the list without already knowing the word.

WHAT IT CHECKS, AND WHY BOTH HALVES. A diff-only guard would have caught none of that
leak. So this reads two things for the range being pushed:
  * every ADDED line of the diff — added only, or a scrubbed history stays red forever;
  * every COMMIT MESSAGE — the half that actually failed.

WORDS, NOT SUBSTRINGS. Text is split on anything that is not a letter or a digit, so
a ticket id like `<prefix>-a1r4bn` is checked as `<prefix>` and `a1r4bn`, and the id is
caught by its prefix alone. The cost is that a name glued inside another word slips
through; the gain is that no innocent word is ever a false positive, and a gate that
cries wolf gets turned off within a week.

DETERMINISTIC ONLY, AND SAID OUT LOUD. Known names are a lookup: exact, instant, never
wrong about the words it knows. It does NOT catch a name nobody added. The guard this
one is modelled on asks a model for exactly that, through its own engine; there is no
engine here to ask, and a hook spawning its own model call on every push would be a
second, worse copy of one. The list is the whole guarantee — keep it current with
`scripts/hooks/add_name.py`.

FAIL-CLOSED. A check that could not run is not a pass.
"""
from __future__ import annotations

import hashlib
import os
import re
import subprocess
import sys

EMPTY = "0" * 40

# Truncated SHA-256 of each forbidden word, lowercased. Add with add_name.py.
# Sixteen hex characters: guessing a preimage is not the threat here — the threat is
# reading the list, and there is nothing here to read.
FORBIDDEN = {
    "c3ea7278adc2ed9c",
    "e6a62276487c3a4e",
    "d9a4f1ecce5483a4",
    "264a73c3b0aced59",
    "38929aa950957bbc",
    "4c7f78fda12b7bc8",
    "a5847b472f6f6a07",
    "eb660be6c96b91dc",
    "ce0f5895e045e3ce",
    "32ed7994816eed5e",
    "f04758e0e5a41bd1",
}

WORD = re.compile(r"[^\W_]+", re.UNICODE)


def digest(word: str) -> str:
    return hashlib.sha256(word.lower().encode()).hexdigest()[:16]


def offending(text: str) -> list[str]:
    return [w for w in WORD.findall(text) if digest(w) in FORBIDDEN]


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, check=False)


def parse_refs(text: str) -> list[tuple[str, str]]:
    """pre-push feeds `<local ref> <local sha> <remote ref> <remote sha>` per line."""
    refs = []
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 4 and parts[1] != EMPTY:  # a deletion pushes nothing new
            refs.append((parts[1], parts[3]))
    return refs


def range_for(local: str, remote: str) -> str:
    """What git is about to reconcile — the one fact only the hook knows."""
    if remote != EMPTY and run(["git", "cat-file", "-e", remote]).returncode == 0:
        return f"{remote}..{local}"
    base = run(["git", "merge-base", local, "origin/HEAD"]).stdout.strip()
    return f"{base}..{local}" if base else local


def hits_in_diff(rng: str) -> list[str]:
    """ADDED lines only, or a history that was cleaned stays red forever."""
    found, path = [], ""
    for ln in run(["git", "diff", "--no-color", rng]).stdout.splitlines():
        if ln.startswith("+++ b/"):
            path = ln[6:]
        elif ln.startswith("+") and not ln.startswith("+++"):
            for w in offending(ln):
                found.append(f"{path}: {w}  ->  {ln.strip()[:90]}")
    return sorted(set(found))


def hits_in_messages(rng: str) -> list[str]:
    """The half that actually leaked. Subject and body, every commit in the range."""
    found = []
    out = run(["git", "log", "--format=%H%x00%B%x00", rng]).stdout
    for chunk in out.split("\x00\x00"):
        if not chunk.strip():
            continue
        sha, _, body = chunk.partition("\x00")
        for line in body.splitlines():
            for w in offending(line):
                found.append(f"commit {sha.strip()[:8]}: {w}  ->  {line.strip()[:90]}")
    return sorted(set(found))


def main() -> int:
    if os.environ.get("NTK_NAME_GUARD") == "off":
        print("name guard: off for this push", file=sys.stderr)
        return 0

    refs = parse_refs(sys.stdin.read())
    if not refs:
        return 0

    problems: list[str] = []
    for local, remote in refs:
        try:
            rng = range_for(local, remote)
            problems += hits_in_diff(rng) + hits_in_messages(rng)
        except Exception as e:  # a check that could not run is not a pass
            print(f"name guard could not run: {e}", file=sys.stderr)
            print("push stopped. To push anyway: NTK_NAME_GUARD=off git push", file=sys.stderr)
            return 1

    if not problems:
        return 0

    print("\nname guard: this push names somebody it should not.", file=sys.stderr)
    for p in sorted(set(problems)):
        print(f"  {p}", file=sys.stderr)
    print("\nFix the code, or reword the commit (git commit --amend / rebase -i).", file=sys.stderr)
    print("To push anyway, on purpose: NTK_NAME_GUARD=off git push", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
