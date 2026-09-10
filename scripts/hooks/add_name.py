"""Add a word to the pre-push guard without writing it down.

    python3 scripts/hooks/add_name.py <word> [<word> ...]

Prints the line to paste into FORBIDDEN in name_guard.py. The word itself never
touches the repository — that is the entire point of storing digests.
"""
import hashlib
import sys

if len(sys.argv) < 2:
    print(__doc__.strip())
    raise SystemExit(2)

for word in sys.argv[1:]:
    print(f'    "{hashlib.sha256(word.lower().encode()).hexdigest()[:16]}",')
