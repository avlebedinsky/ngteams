#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    strip)
        python3 - "${2:-target/test-shards.json}" <<'PY'
import json
from pathlib import Path
import subprocess
import sys
shards = json.loads(Path(sys.argv[1]).read_text())
executables = {Path(test['executable']) for shard in shards for test in shard}
before = sum(path.stat().st_size for path in executables)
for path in sorted(executables):
    subprocess.run(['strip', '--strip-unneeded', '--keep-section=__llvm_covmap',
                    '--keep-section=__llvm_covfun', '--', str(path)], check=True)
after = sum(path.stat().st_size for path in executables)
print(f'Test executables: {before} bytes before strip, {after} bytes after strip')
PY
        ;;
    pack)
        python3 - <<'PY'
import json
from pathlib import Path
paths = {Path('target/test-shards.json'), Path('target/ci-bootstrap-build/debug/proto-lab')}
root = Path('target/coverage-continuous-build')
paths.update(root.glob('debug/.fingerprint/*/*.json'))
for path in root.rglob('*'):
    if path.is_file() and (path.suffix == '.so' or (path.stat().st_mode & 0o111 and not path.suffix)):
        paths.add(path)
shards = json.loads(Path('target/test-shards.json').read_text())
assigned = [{Path(test['executable']).relative_to('/workspace') for test in shard} for shard in shards]
all_tests = set().union(*assigned)
if not all_tests <= paths:
    raise ValueError(f'missing test executables: {all_tests - paths}')
shared = {path for path in all_tests if sum(path in tests for tests in assigned) > 1}
lists = {'common': (paths - all_tests) | shared,
         **{f'shard-{i}': tests - shared for i, tests in enumerate(assigned)}}
for name, files in lists.items():
    Path(f'target/bundle-{name}-files').write_text(''.join(f'{path}\n' for path in sorted(files)))
PY
        for list in target/bundle-*-files; do
            name=${list#target/bundle-}
            name=${name%-files}
            tar -I 'zstd --long=30 -T4 -3' -cf "target/coverage-$name.tar.zst" -T "$list"
            if [[ "$name" == shard-* ]]; then
                while IFS= read -r executable; do
                    rm -- "${executable:?}"
                done < "$list"
            fi
        done
        du -h target/coverage-*.tar.zst
        ;;
    cache)
        source .github/ci/environment.sh
        sdk=$(find target/coverage-continuous-build -path '*/linux-x64-release/lib/libwebrtc.a' -print -quit)
        if [[ -n "$sdk" && ! -d .ci-cache/webrtc/include ]]; then
            mkdir -p .ci-cache/webrtc
            cp -a "${sdk%/lib/libwebrtc.a}/." .ci-cache/webrtc/
        fi
        cargo llvm-cov clean --workspace
        find target/coverage-continuous-build -name '*.profraw' -delete
        find target/coverage-continuous-build -name '*.profdata' -delete
        find target/coverage-continuous-build -type d -name livekit_webrtc -prune -exec rm -rf -- {} +
        mkdir -p .ci-cache/build
        tar --format=posix -I 'zstd --long=30 -T4 -3' -cf .ci-cache/build/dependencies.tar.zst target/coverage-continuous-build
        bytes=$(stat -c %s .ci-cache/build/dependencies.tar.zst)
        echo "Compressed dependency cache: $bytes bytes (limit 1 GiB)"
        if ((bytes > 1073741824)); then
            rm .ci-cache/build/dependencies.tar.zst
        fi
        ;;
    *) exit 2 ;;
esac
