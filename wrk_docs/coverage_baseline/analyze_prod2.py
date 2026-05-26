"""Production-only branch coverage — merge instantiations per function.

The lib is linked into multiple test binaries; each gets its own instantiation
of every function. llvm-cov merges these at the file/summary level but the JSON
`functions` array lists them separately. To approximate the merged production
number we group by (file, demangled-base-name) and OR the branch hit-counts
across instantiations (a branch side is covered if ANY instantiation hit it).
Test-module functions (Rust v0 `5tests` path segment) are excluded entirely.
"""
import json
import re
from collections import defaultdict

with open(r'wrk_docs/coverage_baseline/stage10.json', 'r') as f:
    data = json.load(f)
funcs = data['data'][0]['functions']

# group: (file, basename) -> list of branch arrays
groups = defaultdict(list)


def basename(mangled):
    # strip the leading crate disambiguator so the two instantiations collapse
    # e.g. _RNvNtNtCs<HASH>_6readex... -> drop Cs<HASH>_
    return re.sub(r'Cs[0-9A-Za-z]+_', 'Cs_', mangled)


for fn in funcs:
    name = fn.get('name', '')
    filenames = fn.get('filenames', [])
    if not filenames:
        continue
    f0 = filenames[0].replace('\\', '/')
    if '/src/' not in f0 or 'benchmark' in f0:
        continue
    if '5tests' in name:
        continue
    branches = fn.get('branches', [])
    if not branches:
        continue
    rel = f0.split('/src/', 1)[1]
    key = (rel, basename(name))
    groups[key].append(branches)

# Merge instantiations: a branch side counts as covered if any instantiation hit it.
file_stats = defaultdict(lambda: [0, 0])   # file -> [covered, total]
for (rel, _), insts in groups.items():
    # use the first instantiation's branch list length as canonical; OR across
    n = max(len(b) for b in insts)
    merged_true = [0] * n
    merged_false = [0] * n
    for b in insts:
        for i, br in enumerate(b):
            merged_true[i] += br[4]
            merged_false[i] += br[5]
    cov = sum(1 for i in range(n) if merged_true[i] > 0 and merged_false[i] > 0)
    file_stats[rel][0] += cov
    file_stats[rel][1] += n

rows = []
for rel, (cov, tot) in file_stats.items():
    miss = tot - cov
    pct = cov / tot * 100 if tot else 100
    rows.append((miss, pct, rel, cov, tot))
rows.sort(reverse=True)

print('Production-only branch gaps (instantiations merged):')
print(f'{"miss":>5} {"%":>6}  file  (cov/tot)')
tm = 0
tt = 0
for miss, pct, rel, cov, tot in rows:
    tm += miss
    tt += tot
    if miss > 0:
        print(f'{miss:>5} {pct:>6.1f}  {rel}  ({cov}/{tot})')
print()
print(f'PRODUCTION TOTAL: {tt - tm}/{tt} covered = {(tt - tm) / tt * 100:.2f}% '
      f'({tm} missed)')
