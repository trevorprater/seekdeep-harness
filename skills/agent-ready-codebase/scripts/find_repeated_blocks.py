#!/usr/bin/env python3
"""Find near-duplicate code blocks — the fingerprint of a copied workaround.

Read-only. Stdlib only. Agents extend what they see, so a block that already
appears in several files is actively teaching itself to the next agent. Those
clusters are the highest-leverage findings in an agent-readiness audit.

    python find_repeated_blocks.py <repo-path> [--window 6] [--top 15]
                                   [--min-files 2] [--json]

Clusters whose text contains a workaround marker are ranked first: those are
the ones where the thing being propagated is known to be a compromise.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from collections import defaultdict
from pathlib import Path

SKIP_DIRS = {
    '.git', 'node_modules', 'vendor', 'target', 'dist', 'build', '.venv',
    'venv', '__pycache__', '.mypy_cache', '.pytest_cache', '.next', '.nuxt',
    'coverage', '.gradle', '.idea', '.tox', 'Pods', 'DerivedData', '.terraform',
}
EXTS = {'.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs', '.py', '.rs', '.go',
        '.java', '.kt', '.rb', '.php', '.cs', '.c', '.h', '.cpp', '.cc',
        '.hpp', '.swift', '.scala', '.ex', '.exs', '.sh', '.m'}

MARKER = re.compile(
    r'\b(TODO|FIXME|HACK|XXX|WORKAROUND|KLUDGE)\b'
    r"|(?i:for now|work[- ]?around|band[- ]?aid|do ?n[o']t copy)")
IMPORTISH = re.compile(
    r'^\s*(import|from|use|using|require|include|#include|package|export\s+\*)\b')
# Lines that carry no design information — matching on these finds nothing.
NOISE = re.compile(r'^[\s{}()\[\];,]*$|^\s*(else|end|fi|done|break|return)\s*[;{]?\s*$')


def norm(line: str) -> str:
    return re.sub(r'\s+', ' ', line.strip())


def walk(root: Path):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for fn in filenames:
            p = Path(dirpath) / fn
            if p.suffix.lower() in EXTS:
                yield p


def substantive(lines: list[str]) -> bool:
    """A window worth reporting has real content, not boilerplate."""
    if len(set(lines)) < 3:
        return False
    if sum(len(x) for x in lines) < 120:
        return False
    if sum(bool(IMPORTISH.match(x)) for x in lines) > len(lines) * 0.5:
        return False
    return True


def scan(root: Path, window: int, min_files: int):
    buckets: dict[str, list[tuple[str, int, tuple[str, ...]]]] = defaultdict(list)
    for p in walk(root):
        try:
            if p.stat().st_size > 1_000_000:
                continue
            raw = p.read_text(encoding='utf-8', errors='ignore').splitlines()
        except (OSError, ValueError):
            continue
        kept = [(i + 1, norm(l)) for i, l in enumerate(raw)
                if norm(l) and not NOISE.match(norm(l))]
        if len(kept) < window:
            continue
        rel = str(p.relative_to(root))
        for i in range(len(kept) - window + 1):
            chunk = tuple(t for _, t in kept[i:i + window])
            if not substantive(list(chunk)):
                continue
            h = hashlib.sha1('\n'.join(chunk).encode()).hexdigest()
            buckets[h].append((rel, kept[i][0], chunk))

    clusters = []
    for h, hits in buckets.items():
        files = {f for f, _, _ in hits}
        if len(files) < min_files:
            continue
        chunk = hits[0][2]
        text = '\n'.join(chunk)
        clusters.append({
            'files': len(files),
            'occurrences': len(hits),
            'has_marker': bool(MARKER.search(text)),
            'locations': [f'{f}:{ln}' for f, ln, _ in sorted(hits)[:8]],
            'more': max(0, len(hits) - 8),
            'snippet': list(chunk),
            '_spans': [(f, ln) for f, ln, _ in hits],
        })

    # Overlapping windows produce near-identical clusters; keep the widest
    # spread first and let the caller read down the list.
    clusters.sort(key=lambda c: (c['has_marker'], c['occurrences'], c['files']),
                  reverse=True)
    return clusters


def dedupe(clusters, limit, window):
    """Collapse overlapping windows that describe one duplication.

    A repeated 20-line block yields a window at every offset inside it. Those
    are one finding, so a cluster is dropped when most of its occurrences sit
    within a window's distance of one already reported.
    """
    emitted: list[dict[str, list[int]]] = []
    out = []
    for c in clusters:
        spans: dict[str, list[int]] = {}
        for f, ln in c['_spans']:
            spans.setdefault(f, []).append(ln)
        overlap = 0
        for prev in emitted:
            hit = sum(1 for f, lns in spans.items()
                      for ln in lns
                      if any(abs(ln - q) <= window for q in prev.get(f, [])))
            overlap = max(overlap, hit)
        if overlap > len(c['_spans']) * 0.5:
            continue
        emitted.append(spans)
        out.append({k: v for k, v in c.items() if k != '_spans'})
        if len(out) >= limit:
            break
    return out


def render(clusters, window: int) -> str:
    if not clusters:
        return (f'No repeated {window}-line blocks found across multiple files.\n'
                'Either the codebase is not propagating copied blocks, or the '
                'window is too wide — try --window 4.\n')
    L = [f'# Repeated blocks ({window}-line windows, across 2+ files)', '',
         f'{len(clusters)} clusters shown, marker-bearing ones first.', '']
    for i, c in enumerate(clusters, 1):
        flag = '  ⚠ contains workaround marker' if c['has_marker'] else ''
        L.append(f"## Cluster {i} — {c['occurrences']} occurrences in "
                 f"{c['files']} files{flag}")
        for loc in c['locations']:
            L.append(f'- {loc}')
        if c['more']:
            L.append(f"- … +{c['more']} more")
        L.append('')
        L.append('```')
        L.extend(c['snippet'])
        L.append('```')
        L.append('')
    L.append('Read these before acting. A repeated block can be legitimate '
             '(generated code, a test fixture, a genuinely shared idiom). What '
             'you are looking for is a compromise that got copied — especially '
             'the marker-bearing clusters.')
    return '\n'.join(L) + '\n'


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('repo')
    ap.add_argument('--window', type=int, default=6)
    ap.add_argument('--top', type=int, default=15)
    ap.add_argument('--min-files', type=int, default=2)
    ap.add_argument('--json', action='store_true')
    a = ap.parse_args()
    root = Path(a.repo).resolve()
    if not root.is_dir():
        print(f'not a directory: {root}', file=sys.stderr)
        return 2
    clusters = dedupe(scan(root, a.window, a.min_files), a.top, a.window)
    print(json.dumps(clusters, indent=1) if a.json
          else render(clusters, a.window))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
