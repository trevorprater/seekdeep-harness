#!/usr/bin/env python3
"""Gather agent-readiness signals from a repository.

Read-only. Stdlib only. The point is to replace guesswork with observation:
what the repo is written in, whether a single command can build and test it,
which rung-2 checks already exist, and where workarounds are accumulating.

    python probe_repo.py <repo-path> [--json] [--max-marker-files N]

The output locates candidates. It does not judge them — that is your job.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

SKIP_DIRS = {
    '.git', 'node_modules', 'vendor', 'target', 'dist', 'build', '.venv',
    'venv', '__pycache__', '.mypy_cache', '.pytest_cache', '.next', '.nuxt',
    'coverage', '.gradle', '.idea', '.tox', 'Pods', 'DerivedData', '.terraform',
}

LANGS = {
    '.ts': 'TypeScript', '.tsx': 'TypeScript', '.js': 'JavaScript',
    '.jsx': 'JavaScript', '.mjs': 'JavaScript', '.cjs': 'JavaScript',
    '.py': 'Python', '.rs': 'Rust', '.go': 'Go', '.java': 'Java',
    '.kt': 'Kotlin', '.rb': 'Ruby', '.php': 'PHP', '.cs': 'C#',
    '.c': 'C', '.h': 'C/C++', '.cpp': 'C++', '.cc': 'C++', '.hpp': 'C++',
    '.swift': 'Swift', '.scala': 'Scala', '.ex': 'Elixir', '.exs': 'Elixir',
    '.sh': 'Shell', '.sql': 'SQL', '.m': 'Objective-C',
}

# filename -> (category, label). Presence is the signal.
MARKERS: dict[str, tuple[str, str]] = {}


def _add(names, category, label=None):
    for n in names:
        MARKERS[n] = (category, label or n)


_add(['package.json', 'pyproject.toml', 'setup.py', 'Cargo.toml', 'go.mod',
      'pom.xml', 'build.gradle', 'build.gradle.kts', 'Gemfile', 'composer.json',
      'mix.exs', '*.csproj'], 'manifest')
_add(['package-lock.json', 'yarn.lock', 'pnpm-lock.yaml', 'bun.lockb',
      'poetry.lock', 'uv.lock', 'Cargo.lock', 'go.sum', 'Gemfile.lock',
      'composer.lock'], 'lockfile')
_add(['Makefile', 'justfile', 'Justfile', 'Taskfile.yml', 'Taskfile.yaml',
      'Rakefile', 'noxfile.py', 'tox.ini', 'invoke.yaml'], 'task-runner')
_add(['.eslintrc', '.eslintrc.js', '.eslintrc.json', '.eslintrc.cjs',
      'eslint.config.js', 'eslint.config.mjs', 'biome.json', '.ruff.toml',
      'ruff.toml', '.flake8', '.pylintrc', 'clippy.toml', '.golangci.yml',
      '.golangci.yaml', '.rubocop.yml', 'checkstyle.xml', '.stylelintrc'],
     'linter')
_add(['.prettierrc', '.prettierrc.json', '.prettierrc.js', '.editorconfig',
      'rustfmt.toml', '.rustfmt.toml', '.clang-format', 'dprint.json'],
     'formatter')
_add(['tsconfig.json', 'mypy.ini', '.mypy.ini', 'pyrightconfig.json'],
     'type-checker')
_add(['.pre-commit-config.yaml', '.husky', 'lefthook.yml', '.lefthook.yml'],
     'commit-hook')
_add(['AGENTS.md', 'CLAUDE.md', '.cursorrules', '.windsurfrules',
      'GEMINI.md', '.clinerules', 'CONVENTIONS.md'], 'agent-instructions')
_add(['CONTRIBUTING.md', 'ARCHITECTURE.md', 'README.md', 'docs'], 'docs')

# Conventional markers are written in caps by convention, so match them
# case-sensitively. Matching "legacy" or "temporary" case-insensitively would
# hit ordinary identifiers (`let temporary = ...`) and bury the real signal.
STRONG_MARKER = re.compile(r'\b(TODO|FIXME|HACK|XXX|WORKAROUND|KLUDGE)\b')

# Softer phrases only count inside a comment, where they are commentary rather
# than code.
SOFT_MARKER = re.compile(
    r"(for now|do ?n[o']t copy|work[- ]?around|band[- ]?aid|"
    r"temporary (?:fix|hack|solution)|revisit|leave this)", re.IGNORECASE)
COMMENTISH = re.compile(r'(^|\s)(//|#|/\*|\*|--|<!--|""")')


def marker_in(line: str) -> bool:
    if STRONG_MARKER.search(line):
        return True
    return bool(COMMENTISH.search(line) and SOFT_MARKER.search(line))

CMD_HINT = re.compile(
    r'^\s*(?:\$\s*)?((?:npm|pnpm|yarn|bun|make|just|task|cargo|go|pytest|'
    r'python|uv|poetry|tox|nox|gradle|mvn|dotnet|bundle|mix)\b[^\n`]*)',
    re.MULTILINE)


def walk(root: Path):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS
                       and not d.startswith('.egg')]
        for fn in filenames:
            yield Path(dirpath) / fn


def read_text(p: Path, limit: int = 400_000) -> str:
    try:
        if p.stat().st_size > limit:
            return ''
        return p.read_text(encoding='utf-8', errors='ignore')
    except (OSError, ValueError):
        return ''


def probe(root: Path, max_marker_files: int) -> dict:
    langs: Counter = Counter()
    found: dict[str, list[str]] = defaultdict(list)
    marker_hits: Counter = Counter()
    marker_examples: dict[str, list[str]] = defaultdict(list)
    n_files = 0
    csproj_seen = False

    for p in walk(root):
        rel = str(p.relative_to(root))
        n_files += 1
        suffix = p.suffix.lower()
        if suffix in LANGS:
            langs[LANGS[suffix]] += 1
        if suffix == '.csproj' and not csproj_seen:
            found['manifest'].append(rel)
            csproj_seen = True
        entry = MARKERS.get(p.name)
        if entry:
            found[entry[0]].append(rel)
        if suffix in LANGS:
            text = read_text(p)
            for i, line in enumerate(text.splitlines(), 1):
                if marker_in(line):
                    marker_hits[rel] += 1
                    if len(marker_examples[rel]) < 3:
                        marker_examples[rel].append(f'{i}: {line.strip()[:120]}')

    # Directory-style markers that os.walk yields as dirs, not files.
    for name, (cat, _) in MARKERS.items():
        d = root / name
        if d.is_dir() and name not in found.get(cat, []):
            found[cat].append(name + '/')

    # CI lives in known directories rather than single files.
    ci = []
    for cand in ('.github/workflows', '.gitlab-ci.yml', '.circleci/config.yml',
                 'azure-pipelines.yml', 'Jenkinsfile', '.buildkite',
                 '.drone.yml', 'bitbucket-pipelines.yml'):
        t = root / cand
        if t.exists():
            if t.is_dir():
                jobs = sorted(x.name for x in t.iterdir() if x.is_file())
                ci.append(f'{cand}/ ({len(jobs)} workflows: {", ".join(jobs[:6])})')
            else:
                ci.append(cand)

    readme = root / 'README.md'
    readme_cmds = []
    if readme.exists():
        readme_cmds = [m.group(1).strip()
                       for m in CMD_HINT.finditer(read_text(readme))][:12]

    top_markers = marker_hits.most_common(max_marker_files)
    return {
        'root': str(root),
        'files_scanned': n_files,
        'languages': dict(langs.most_common()),
        'signals': {k: sorted(set(v)) for k, v in sorted(found.items())},
        'ci': ci,
        'readme_commands': readme_cmds,
        'workaround_markers': {
            'total_lines': sum(marker_hits.values()),
            'files_affected': len(marker_hits),
            'top_files': [
                {'file': f, 'count': c, 'examples': marker_examples[f]}
                for f, c in top_markers
            ],
        },
    }


def render(d: dict) -> str:
    L = [f"# Agent-readiness probe — {d['root']}", '',
         f"{d['files_scanned']} files scanned (vendor and build dirs skipped)", '']

    L.append('## Languages')
    if d['languages']:
        for lang, n in list(d['languages'].items())[:10]:
            L.append(f'- {lang}: {n} files')
    else:
        L.append('- none detected')
    L.append('')

    def brief(items, cap=8):
        # Monorepos have hundreds of manifests; a full listing buries the
        # signal, and the signal is presence plus rough scale.
        items = list(items)
        if len(items) <= cap:
            return ', '.join(items)
        return ', '.join(items[:cap]) + f' … +{len(items) - cap} more'

    L.append('## Signals present')
    order = ['manifest', 'lockfile', 'task-runner', 'linter', 'formatter',
             'type-checker', 'commit-hook', 'agent-instructions', 'docs']
    for cat in order:
        got = d['signals'].get(cat)
        L.append(f'- **{cat}**: ' + (brief(got) if got else '_none found_'))
    L.append('- **ci**: ' + (brief(d['ci']) if d['ci'] else '_none found_'))
    L.append('')

    L.append('## Documented commands (from README)')
    if d['readme_commands']:
        for c in d['readme_commands']:
            L.append(f'- `{c}`')
    else:
        L.append('- _no build/test commands found in README_')
    L.append('')

    w = d['workaround_markers']
    L.append('## Workaround markers')
    L.append(f"{w['total_lines']} marker lines across {w['files_affected']} files.")
    if w['top_files']:
        L.append('')
        for f in w['top_files']:
            L.append(f"- `{f['file']}` ({f['count']})")
            for ex in f['examples']:
                L.append(f'    - {ex}')
    L.append('')

    L.append('## Read these signals as')
    gaps = []
    s = d['signals']
    if not s.get('linter'):
        gaps.append('No linter config — rung 2 is largely unavailable; every '
                    'convention currently depends on prose or review.')
    if not s.get('type-checker') and (
            'TypeScript' in d['languages'] or 'Python' in d['languages']):
        gaps.append('No type-checker config in a language that supports one — '
                    'rung 1 is being left on the table.')
    if not d['ci']:
        gaps.append('No CI — rung-2 checks cannot be enforced, only suggested.')
    if not s.get('task-runner') and not d['readme_commands']:
        gaps.append('No task runner and no documented commands — an agent has '
                    'to infer how to build and test, so it will guess.')
    if not s.get('agent-instructions'):
        gaps.append('No agent instructions file — expected for a repo that has '
                    'not been set up for agents yet.')
    if w['total_lines'] > 50:
        gaps.append(f"{w['total_lines']} workaround markers — check whether any "
                    'have been copied; run find_repeated_blocks.py.')
    if gaps:
        for g in gaps:
            L.append(f'- {g}')
    else:
        L.append('- Baseline tooling is present. Depth and enforcement still '
                 'need checking by hand.')
    return '\n'.join(L) + '\n'


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('repo')
    ap.add_argument('--json', action='store_true')
    ap.add_argument('--max-marker-files', type=int, default=10)
    a = ap.parse_args()
    root = Path(a.repo).resolve()
    if not root.is_dir():
        print(f'not a directory: {root}', file=sys.stderr)
        return 2
    d = probe(root, a.max_marker_files)
    print(json.dumps(d, indent=1) if a.json else render(d))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
