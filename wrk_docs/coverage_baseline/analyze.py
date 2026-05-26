"""Quick analysis of cargo-llvm-cov JSON output."""
import json
import sys

with open(r'wrk_docs/coverage_baseline/baseline.json', 'r') as f:
    data = json.load(f)

totals = data['data'][0]['totals']
print('Per-metric totals:')
for k in ('regions', 'functions', 'lines', 'branches', 'instantiations'):
    v = totals.get(k, {})
    if v:
        miss = v['count'] - v['covered']
        print(f'  {k:15s}: covered {v["covered"]}/{v["count"]} ({v["percent"]:.2f}%) missed {miss}')

# Files grouped by module
files = data['data'][0]['files']
mods = {}
for fi in files:
    fn = fi['filename']
    if 'benchmark' in fn or 'tests' in fn or 'cargo' in fn or 'rustc' in fn or '.cargo' in fn:
        continue
    norm = fn.replace('\\', '/')
    rel = norm.split('/src/', 1)[-1] if '/src/' in norm else norm
    parts = rel.split('/')
    mod = parts[0] if len(parts) > 1 else '_root'
    mods.setdefault(mod, []).append(fi)

print()
print('Module-level totals:')
hdr = f'{"module":<14} {"branches":>17} {"lines":>17} {"regions":>17} {"functions":>15}'
print(hdr)
print('-' * len(hdr))

for mod, fis in sorted(mods.items()):
    br_c = sum(f['summary']['branches']['covered'] for f in fis)
    br_t = sum(f['summary']['branches']['count'] for f in fis)
    li_c = sum(f['summary']['lines']['covered'] for f in fis)
    li_t = sum(f['summary']['lines']['count'] for f in fis)
    rg_c = sum(f['summary']['regions']['covered'] for f in fis)
    rg_t = sum(f['summary']['regions']['count'] for f in fis)
    fu_c = sum(f['summary']['functions']['covered'] for f in fis)
    fu_t = sum(f['summary']['functions']['count'] for f in fis)
    bp = (br_c / br_t * 100) if br_t else 0
    lp = (li_c / li_t * 100) if li_t else 0
    rp = (rg_c / rg_t * 100) if rg_t else 0
    fp = (fu_c / fu_t * 100) if fu_t else 0
    print(f'{mod:<14} {br_c}/{br_t} ({bp:.1f}%)  {li_c}/{li_t} ({lp:.1f}%)  '
          f'{rg_c}/{rg_t} ({rp:.1f}%)  {fu_c}/{fu_t} ({fp:.1f}%)')

# Per-file by missed branches
print()
print('Files by missed branches:')
file_stats = []
for fi in files:
    fn = fi['filename']
    if 'benchmark' in fn or 'tests' in fn or 'cargo' in fn or 'rustc' in fn:
        continue
    s = fi['summary']
    br = s.get('branches', {})
    if br.get('count', 0) > 0:
        miss = br['count'] - br['covered']
        file_stats.append((miss, br['percent'], fn))
file_stats.sort(reverse=True)
print(f'{"miss":>5} {"%":>6}  file')
for miss, pct, fn in file_stats:
    norm = fn.replace('\\', '/')
    rel = norm.split('/src/', 1)[-1] if '/src/' in norm else norm
    print(f'{miss:>5} {pct:>6.2f}  {rel}')
