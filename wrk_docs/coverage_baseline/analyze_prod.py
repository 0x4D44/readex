"""Production-only branch-coverage analysis (excludes #[cfg(test)] mod tests).

cargo-llvm-cov counts test-module branches (assert! panic-sides etc.) in the
file summary. This script separates production functions from test-module
functions (Rust v0 mangling: a `mod tests` segment encodes as `5tests`) so we
can see the real production-code branch gap and rank the highest-value targets.

Function-level branch arrays are summed across BOTH instantiations of the lib
(the lib unit-test binary AND each integration-test binary link the lib
separately), so per-function totals run ~2x the file summary. Use the output
for RELATIVE ranking of production gaps, not absolute percentages.
"""
import json
from collections import defaultdict

with open(r'wrk_docs/coverage_baseline/stage10.json', 'r') as f:
    data = json.load(f)
funcs = data['data'][0]['functions']

file_prod = defaultdict(lambda: [0, 0])   # file -> [covered, total]
file_test = defaultdict(lambda: [0, 0])

for fn in funcs:
    name = fn.get('name', '')
    filenames = fn.get('filenames', [])
    if not filenames:
        continue
    f0 = filenames[0].replace('\\', '/')
    if '/src/' not in f0 or 'benchmark' in f0:
        continue
    branches = fn.get('branches', [])
    if not branches:
        continue
    rel = f0.split('/src/', 1)[1]
    cov = sum(1 for b in branches if (b[4] > 0 and b[5] > 0))
    tot = len(branches)
    bucket = file_test if '5tests' in name else file_prod
    bucket[rel][0] += cov
    bucket[rel][1] += tot

rows = []
for rel, (cov, tot) in file_prod.items():
    miss = tot - cov
    pct = cov / tot * 100 if tot else 100
    rows.append((miss, pct, rel, cov, tot))
rows.sort(reverse=True)

print('Production-only branch gaps (per-function arrays, ~2x instantiation):')
print(f'{"miss":>5} {"%":>6}  file  (cov/tot)')
for miss, pct, rel, cov, tot in rows:
    if miss == 0:
        continue
    print(f'{miss:>5} {pct:>6.1f}  {rel}  ({cov}/{tot})')

prod_miss = sum(r[0] for r in rows)
prod_tot = sum(r[4] for r in rows)
test_miss = sum(v[1] - v[0] for v in file_test.values())
test_tot = sum(v[1] for v in file_test.values())
print()
print(f'Production missed (2x): {prod_miss} / {prod_tot} '
      f'({(prod_tot - prod_miss) / prod_tot * 100:.2f}%)')
print(f'Test-module missed (2x): {test_miss} / {test_tot} '
      f'(these are assert! panic-sides + test control flow — NOT coverable)')
