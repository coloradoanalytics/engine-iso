#!/usr/bin/env python3
"""Package the two cross-built binaries as releases/<version>/; called by build_binaries.sh.

An existing release with identical files is left as it is; one whose files
differ is kept unless REPLACE=1. Other versions are never changed. PREBUILT may
name a folder holding engine-iso, engine-iso.exe and the BUILD.json or
BUILD-INFO.json release record they shipped with, to repackage them instead of
the fresh cargo outputs; their hashes and source file hashes
must match that record and this source.

A release identifies its source by the SHA-256 of each source file, recorded in
BUILD-INFO.json; the source offer names the repository, not a commit.
"""
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
import tarfile
import time
import zipfile

root = Path('/src')
build = Path('/build')
releases = Path('/releases')
download_url = 'https://github.com/coloradoanalytics/engine-iso/raw/refs/heads/main/releases'
# A release folder is named by the raw package version: SemVer, no "v" prefix.
VERSION = re.compile(r'(\d+)\.(\d+)\.(\d+)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?')
source_url = 'https://github.com/coloradoanalytics/engine-iso'
epoch = int(os.environ['SOURCE_DATE_EPOCH'])
prebuilt = os.environ.get('PREBUILT')
replace = os.environ.get('REPLACE') == '1'
metadata = json.loads((build / 'metadata.json').read_text())
source_files = [root / 'Cargo.toml', root / 'Cargo.lock', *sorted((root / 'src').rglob('*.rs'))]
source_hashes = {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in source_files}
# Executables are distributed under the package version, as engine-iso-<version>.
version = next(p['version'] for p in metadata['packages'] if p['name'] == 'engine-iso')
if not VERSION.fullmatch(version):
    sys.exit(f'invalid package version for a release folder: {version!r}')
common = {name: (root / name).read_bytes() for name in ('LICENSE', 'COPYRIGHT')}
notices = ['Third-party Rust dependencies retain their own copyrights and licenses.\n']
for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
    if package['name'] == 'engine-iso':
        continue
    key = f"{package['name']}-{package['version']}"
    notices.append(f"{key}: {package['license']}\nSource: https://crates.io/crates/{package['name']}/{package['version']}\n")
    directory = Path(package['manifest_path']).parent
    for path in sorted(directory.iterdir()):
        if path.is_file() and path.name.lower().startswith(('license', 'licence', 'copying', 'notice', 'copyright')):
            common[f'licenses/{key}/{path.name}'] = path.read_bytes()
common['THIRD_PARTY_NOTICES.txt'] = '\n'.join(notices).encode()
# Include the standard-library and static C-runtime notices as well as crates.
for name in ('musl', 'mingw-w64-common', 'gcc-mingw-w64-base'):
    common[f'licenses/toolchain/{name}-copyright'] = Path(f'/usr/share/doc/{name}/copyright').read_bytes()
for directory in Path('/usr/local/rustup/toolchains').glob('*/share/doc/rust'):
    common['licenses/rust/COPYRIGHT-library.html'] = (directory / 'COPYRIGHT-library.html').read_bytes()
    for path in sorted((directory / 'licenses').rglob('*')):
        if path.is_file():
            common[f'licenses/rust/licenses/{path.relative_to(directory / "licenses")}'] = path.read_bytes()

# An archive holds only the executable and LICENSES.txt, every license and
# attribution text in one file, headed by the corresponding-source offer.
licenses = [f'''engine-iso {version}: ISO 9613-2:2024 calculation engine (community)
Copyright (C) 2026 Colorado Analytics. Licensed under GPL-3.0-only (below).
Corresponding source: {source_url}, the repository commit whose Cargo.toml,
Cargo.lock and src/ files have the SHA-256 hashes listed as source_files_sha256 in
{source_url}/blob/main/releases/{version}/BUILD-INFO.json
'''.encode()]
for name in ['COPYRIGHT', 'LICENSE', 'THIRD_PARTY_NOTICES.txt', *sorted(n for n in common if n.startswith('licenses/'))]:
    licenses.append(f'\n{"=" * 78}\n{name}\n{"=" * 78}\n\n'.encode() + common[name].rstrip(b'\n') + b'\n')
licenses = b''.join(licenses)


def binary(target, suffix):
    if prebuilt:
        return Path(prebuilt) / f'engine-iso{suffix}'
    return build / 'target' / target / 'release' / f'engine-iso{suffix}'


if prebuilt:
    record = next(Path(prebuilt) / n for n in ('BUILD-INFO.json', 'BUILD.json') if (Path(prebuilt) / n).exists())
    record = json.loads(record.read_text())
    # Verify the actual build inputs rather than Git ancestry: the same source
    # remains verifiable after importing it into a fresh public repository.
    assert record['source_files_sha256'] == source_hashes, 'prebuilt binaries were built from different source'
    shipped = {a['target']: a['binary_sha256'] for a in record['artifacts'].values()}
    for target, suffix in (('x86_64-unknown-linux-musl', ''), ('x86_64-pc-windows-gnu', '.exe')):
        assert hashlib.sha256(binary(target, suffix).read_bytes()).hexdigest() == shipped[target], target
identity = json.loads(subprocess.check_output([str(binary('x86_64-unknown-linux-musl', '')), '--describe', '--threads', '1']))
assert identity['version'] == version, (identity['version'], version)
manifest = {'version': version, 'engine_identity': identity,
            'source_files_sha256': source_hashes, 'rustc': (build / 'rustc.txt').read_text().strip(), 'artifacts': {}}
outputs = {}
for platform, target, suffix, extension in (
    ('linux-x86_64', 'x86_64-unknown-linux-musl', '', 'tar.gz'),
    ('windows-x86_64', 'x86_64-pc-windows-gnu', '.exe', 'zip'),
):
    data = binary(target, suffix).read_bytes()
    binary_hash = hashlib.sha256(data).hexdigest()
    executable = f'engine-iso-{version}{suffix}'
    prefix = f'engine-iso-{version}-{platform}'
    files = {executable: data, 'LICENSES.txt': licenses}
    archive = io.BytesIO()
    if extension == 'zip':
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            for name, content in sorted(files.items()):
                entry = zipfile.ZipInfo(f'{prefix}/{name}', time.gmtime(max(epoch, 315532800))[:6])
                entry.compress_type = zipfile.ZIP_DEFLATED
                entry.external_attr = (0o100755 if name == executable else 0o100644) << 16
                z.writestr(entry, content)
    else:
        with gzip.GzipFile(filename='', mode='wb', fileobj=archive, mtime=epoch) as gz:
            with tarfile.open(fileobj=gz, mode='w') as tar:
                for name, content in sorted(files.items()):
                    entry = tarfile.TarInfo(f'{prefix}/{name}')
                    entry.size, entry.mtime = len(content), epoch
                    entry.mode = 0o755 if name == executable else 0o644
                    tar.addfile(entry, io.BytesIO(content))
    name = f'{prefix}.{extension}'
    outputs[name] = archive.getvalue()
    manifest['artifacts'][name] = {'target': target, 'executable': f'{prefix}/{executable}',
                                   'sha256': hashlib.sha256(outputs[name]).hexdigest(),
                                   'binary_sha256': binary_hash, 'bytes': len(outputs[name])}
outputs['BUILD-INFO.json'] = (json.dumps(manifest, indent=2) + '\n').encode()
outputs['SHA256SUMS'] = ''.join(f"{v['sha256']}  {k}\n" for k, v in sorted(manifest['artifacts'].items())).encode()
linux, windows = sorted(manifest['artifacts'])
outputs['README.md'] = f'''# engine-iso {version}

The community ISO 9613-2:2024 calculation engine for baseline Intel/AMD x86-64.
No Git or compiler is required. Each archive holds only the engine executable
and `LICENSES.txt` (its license, third-party notices and source offer).

| Computer | Download | Engine executable |
| --- | --- | --- |
| Linux x86-64 | [{linux}]({download_url}/{version}/{linux}) | `engine-iso-{version}` (static musl) |
| Windows x86-64 | [{windows}]({download_url}/{version}/{windows}) | `engine-iso-{version}.exe` (Windows system DLLs) |

Extract the archive into the `engines` folder beside `acalc-engine` (Windows:
`acalc-engine.exe`), keeping the archive's own folder, so the engine is at
`engines/{linux.removesuffix('.tar.gz')}/engine-iso-{version}` (Windows:
`engines\\{windows.removesuffix('.zip')}\\engine-iso-{version}.exe`). On Linux,
`tar -xzf {linux} -C engines`; on Windows, **Extract All** into `engines`. Start
acalc-engine; it finds the engine at startup and starts it when needed. If
several engine-iso versions are installed, choose one on its **Engines** page;
use **Refresh** there after adding an engine while the service runs.
The complete guide is the
[engine-iso README](https://github.com/coloradoanalytics/engine-iso#download-and-install).

`SHA256SUMS` identifies the archives. On Linux, run `sha256sum -c SHA256SUMS`
with both archives in the same folder. On Windows, use
`Get-FileHash .\\{windows} -Algorithm SHA256` and compare the hash with
`SHA256SUMS`. `BUILD-INFO.json` records the source file hashes,
compiler, targets, engine identity and executable and archive hashes; its
one-worker identity is only for reproducibility.

This is an engineering implementation, not a verified ISO conformity release.
Use the same target, build and math library on all nodes of a calculation farm.
Corresponding source: <{source_url}>, the commit whose source files match
`source_files_sha256` in `BUILD-INFO.json`.
'''.encode()

release = releases / version
if release.exists():
    existing = {p.name: p.read_bytes() for p in release.iterdir() if p.is_file()}
    changed = sorted(n for n in existing.keys() | outputs.keys() if existing.get(n) != outputs.get(n))
    if not changed:
        print(f'releases/{version} is already up to date')
    elif not replace:
        sys.exit(f'releases/{version} exists with different files ({", ".join(changed)}); '
                 'set REPLACE=1 to replace this release')
if not release.exists() or replace:
    staging = releases / f'.{version}.tmp'
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    for name, content in outputs.items():
        (staging / name).write_bytes(content)
    if release.exists():
        shutil.rmtree(release)
    staging.rename(release)


def key(name):
    major, minor, patch, pre, _ = VERSION.fullmatch(name).groups()
    return int(major), int(minor), int(patch), pre is None, pre or ''


# The index lists every release, current (highest stable, else highest) first.
versions = sorted((p.name for p in releases.iterdir() if p.is_dir() and VERSION.fullmatch(p.name)), key=key, reverse=True)
current = next((v for v in versions if '-' not in v.split('+')[0]), versions[0])
index = ['# engine-iso releases\n', 'Download the current release for your computer. Each release folder has an',
         'installation guide, `SHA256SUMS` and `BUILD-INFO.json`.\n']
labels = {'linux-x86_64': 'Linux x86-64', 'windows-x86_64': 'Windows x86-64'}
for v in [current] + [v for v in versions if v != current]:
    info = json.loads((releases / v / 'BUILD-INFO.json').read_text())
    index.append(f'## {v}' + (' (current)' if v == current else '') + '\n')
    index.append('| Computer | Download | SHA-256 |\n| --- | --- | --- |')
    for name, artifact in sorted(info['artifacts'].items()):
        platform = next(p for p in labels if name.endswith(f'-{p}.tar.gz') or name.endswith(f'-{p}.zip'))
        index.append(f"| {labels[platform]} | [{name}]({download_url}/{v}/{name}) | `{artifact['sha256']}` |")
    index.append(f'\n[Installation guide]({v}/README.md) · [SHA256SUMS]({v}/SHA256SUMS) · '
                 f'[BUILD-INFO.json]({v}/BUILD-INFO.json)\n')
(releases / 'README.md').write_text('\n'.join(index))
print((release / 'SHA256SUMS').read_text())
