import json, re, sys
from collections import defaultdict

path = sys.argv[1] if len(sys.argv) > 1 else 'wrk_docs/coverage_baseline/secondpass_lib.json'
data = json.load(open(path, encoding='utf-8'))
tmod = {'readability_fork':2306,'main_extractor':2688,'output':2897,'xpath_engine':1459,
        'cleaning':1692,'baseline':632,'deduplication':901,'utils':400,'settings_constants':178,'lib':1343}

want = sys.argv[2] if len(sys.argv) > 2 else None

for fobj in data['data'][0]['files']:
    fn = fobj['filename'].replace('\\','/')
    m = re.search(r'/src/(?:trafilatura/)?([a-z_]+)\.rs$', fn)
    if not m:
        continue
    name = m.group(1)
    if name not in tmod:
        continue
    if name == 'lib' and not fn.endswith('/src/lib.rs'):
        continue
    if name != 'lib' and '/trafilatura/' not in fn:
        continue
    if want and name != want:
        continue
    limit = tmod[name]
    br = fobj.get('branches', [])
    # Aggregate true/false counts per region key across instantiations.
    agg = defaultdict(lambda: [0, 0])
    for b in br:
        l1, c1, l2, c2, tc, fc = b[0], b[1], b[2], b[3], b[4], b[5]
        if l1 >= limit:
            continue
        key = (l1, c1, l2, c2)
        agg[key][0] += tc
        agg[key][1] += fc
    miss = []
    for (l1, c1, l2, c2), (tc, fc) in agg.items():
        if tc == 0 or fc == 0:
            side = ('T0 ' if tc == 0 else '') + ('F0' if fc == 0 else '')
            miss.append((l1, c1, tc, fc, side.strip()))
    miss.sort()
    print(f'=== {name}.rs: {len(miss)} prod regions with an unhit side ({len(agg)} prod regions total) ===')
    for l1, c1, tc, fc, side in miss:
        print(f'  L{l1}:{c1}  T={tc} F={fc}  [{side}]')
