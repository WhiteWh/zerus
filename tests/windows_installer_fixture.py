"""Synthetic installer bytes and their CI build metadata; never executable tests."""
import json
from pathlib import Path
import windows_installer_release


def installer_files(directory, portable, version, product, commit):
    directory = Path(directory)
    executable = directory / (Path(portable).stem + '-setup.exe')
    executable.write_bytes(b'MZsynthetic installer fixture\n' + version.encode())
    info = directory / (Path(portable).stem + '-installer-info.json')
    record = dict(version=version, product_version=product, source_commit=commit)
    info.write_text(json.dumps(windows_installer_release.build_info(executable, portable, record)))
    return executable, info
