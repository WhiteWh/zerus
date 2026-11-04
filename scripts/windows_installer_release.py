"""Bind a CI-built installer to its tested ZIP before candidate publication."""
import json
from pathlib import Path
import re
import hashlib

from release_common import require, sha256
from windows_installer import installer_name
import windows_portable

INFO_LIMIT = 16 << 10
INSTALLER_LIMIT = windows_portable.ZIP_LIMIT
FIELDS = {'schema', 'version', 'product_version', 'source_commit', 'portable_size',
          'portable_sha256', 'installer_size', 'installer_sha256'}
HEX64 = re.compile(r'[0-9a-f]{64}')


def regular(path, limit):
    path = Path(path)
    require(path.is_file() and not path.is_symlink() and 0 < path.stat().st_size <= limit,
            'Invalid Windows installer publication input.')
    return path


def build_info(installer, portable, record):
    installer = regular(installer, INSTALLER_LIMIT)
    portable = regular(portable, windows_portable.ZIP_LIMIT)
    with installer.open('rb') as stream:
        require(stream.read(2) == b'MZ', 'Installer is not a Windows executable.')
    return dict(schema=1, version=record['version'], product_version=record['product_version'],
                source_commit=record['source_commit'], portable_size=portable.stat().st_size,
                portable_sha256=sha256(portable), installer_size=installer.stat().st_size,
                installer_sha256=sha256(installer))


def unique_fields(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'Duplicate Windows installer build field.')
        result[key] = value
    return result


def require_build(installer, portable, info_path, version, product, commit):
    # Validate all inputs before candidate assembly creates output files. No
    # executable is extracted or run by Linux assembly or publication.
    windows_portable.require_build(portable, commit, product)
    path = regular(info_path, INFO_LIMIT)
    with path.open('rb') as stream:
        raw = stream.read(INFO_LIMIT + 1)
    require(len(raw) <= INFO_LIMIT, 'Windows installer build metadata exceeds its bound.')
    info = json.loads(raw, object_pairs_hook=unique_fields)
    require(isinstance(info, dict) and set(info) == FIELDS and type(info['schema']) is int
            and info['schema'] == 1, 'Invalid Windows installer build metadata.')
    require(info['version'] == version and info['product_version'] == product
            and info['source_commit'] == commit, 'Windows installer belongs to another build/version.')
    for key in ('portable_size', 'installer_size'):
        require(type(info[key]) is int and info[key] > 0, 'Invalid Windows installer build size.')
    for key in ('portable_sha256', 'installer_sha256'):
        require(isinstance(info[key], str) and HEX64.fullmatch(info[key]),
                'Invalid Windows installer build digest.')
    actual = build_info(installer, portable, dict(version=version, product_version=product,
                                                source_commit=commit))
    require(info == actual, 'Windows installer or paired ZIP differs from its CI build metadata.')
    return info


def add(directory, installer, build, version, info):
    asset = installer_name(version)
    source = regular(installer, INSTALLER_LIMIT)
    require(sha256(directory / info['windows_portable']) == build['portable_sha256'],
            'Paired ZIP changed during candidate assembly.')
    digest = hashlib.sha256()
    total = 0
    with source.open('rb') as incoming, (directory / asset).open('xb') as output:
        for block in iter(lambda: incoming.read(1 << 20), b''):
            total += len(block)
            require(total <= build['installer_size'], 'Installer changed during candidate assembly.')
            digest.update(block)
            output.write(block)
    require(total == build['installer_size'] and digest.hexdigest() == build['installer_sha256'],
            'Installer changed during candidate assembly.')
    info['windows_installer'] = dict(asset=asset, portable_sha256=build['portable_sha256'])


def validate_metadata(value, version, portable, sums):
    require(isinstance(value, dict) and set(value) == {'asset', 'portable_sha256'}
            and value['asset'] == installer_name(version)
            and portable == windows_portable.asset_name(version)
            and isinstance(value['portable_sha256'], str)
            and HEX64.fullmatch(value['portable_sha256'])
            and sums.get(portable) == value['portable_sha256'],
            'Invalid Windows installer asset or paired ZIP digest.')
    return value['asset']
