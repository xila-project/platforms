"""Print the commit Cargo.lock resolves for a Xila git dependency.

Xila repositories track each other with `branch = "main"`; the exact commit
lives in Cargo.lock. Workflows that check out a matching repository use this
script instead of duplicating the commit.

Usage: locked_revision.py <repository> [<key>]
Prints `<key>=<commit>` (default key: `revision`), ready for $GITHUB_OUTPUT.
"""
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def locked_revision(repository, lockfile=ROOT / 'Cargo.lock'):
    pattern = re.compile(
        rf'^git\+https://github\.com/xila-project/{re.escape(repository)}\?branch=main#([0-9a-f]{{40}})$')
    revisions = {match.group(1)
                 for package in tomllib.loads(Path(lockfile).read_text())['package']
                 if (match := pattern.match(package.get('source', '')))}
    if len(revisions) != 1:
        raise SystemExit(f'expected one locked {repository} revision, found {sorted(revisions)}')
    return revisions.pop()


if __name__ == '__main__':
    if len(sys.argv) not in (2, 3):
        raise SystemExit(__doc__)
    key = sys.argv[2] if len(sys.argv) == 3 else 'revision'
    print(f'{key}={locked_revision(sys.argv[1])}')
