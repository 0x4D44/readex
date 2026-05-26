"""Insert `#[cfg_attr(coverage_nightly, coverage(off))]` above each
`#[cfg(test)]\nmod tests {` so cargo-llvm-cov (which sets cfg coverage_nightly)
excludes test modules from instrumentation. Idempotent: skips files already
annotated. Touches only the test-module declaration line; no other content.
"""
import glob
import os

MARKER = '#[cfg_attr(coverage_nightly, coverage(off))]'
ROOT = os.path.join(os.path.dirname(__file__), '..', '..', 'src')

changed = []
skipped = []
for path in glob.glob(os.path.join(ROOT, '**', '*.rs'), recursive=True):
    with open(path, 'r', encoding='utf-8') as f:
        src = f.read()
    # Target the canonical pattern: a line `#[cfg(test)]` immediately followed
    # by `mod tests {`.
    needle = '#[cfg(test)]\nmod tests {'
    if needle not in src:
        continue
    if MARKER + '\n#[cfg(test)]\nmod tests {' in src:
        skipped.append(path)
        continue
    new = src.replace(needle, f'#[cfg(test)]\n{MARKER}\nmod tests {{', 1)
    with open(path, 'w', encoding='utf-8') as f:
        f.write(new)
    changed.append(path)

print(f'Annotated {len(changed)} files:')
for p in sorted(changed):
    print('  ', os.path.relpath(p, os.path.join(ROOT, '..')))
if skipped:
    print(f'Already annotated (skipped {len(skipped)}):')
    for p in sorted(skipped):
        print('  ', os.path.relpath(p, os.path.join(ROOT, '..')))
