"""Detect and unpack Windows installer and archive formats into a source tree.

This module is the ingest front-end for ``web_recompile.py``. It recognises
ZIP/SFX-ZIP, Microsoft Cabinet (CAB/SFX-CAB), MSI, InstallShield, Inno Setup,
NSIS and plain PE/NE installer stubs, then extracts the payload into a
directory that the existing analyse -> recompile -> stage pipeline can consume.

Only the standard library is used. External extractors (``cabextract``, ``7z``,
``msiextract``, ``innoextract``, ``unshield``, ``i5comp``, ``i6comp``) are
optional. Uploaded files are never executed; extraction happens in a fresh
directory and the resulting tree is validated against the same limits as ZIP
uploads.

Nothing here claims to run an installer's own logic. When a stub only unpacks
at runtime and no static payload can be found, the result reports
``needsRun=True`` with a human-readable warning so the caller can tell the user
to extract the installer manually or wait for Win32/Win16 execution support.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
import os
import shutil
import stat
import subprocess
import tempfile
import zipfile

TOOL_NAMES = (
    'cabextract', '7z', '7za', 'msiextract', 'innoextract',
    'unshield', 'i5comp', 'i6comp',
)

ZIP_MAGIC = b'PK\x03\x04'
CAB_MAGIC = b'MSCF'
MSI_MAGIC = bytes.fromhex('d0cf11e0a1b11ae1')
MZ_MAGIC = b'MZ'

BRAND_MARKERS = {
    'installshield': (b'installshield', b'is setup', b'_setup.dll', b'data1.cab'),
    'inno': (b'inno setup', b'inno-setup', b'innounp'),
    'nsis': (b'nullsoft', b'nsis', b'nullsoft install system'),
    'wise': (b'wise installer', b'wise setup'),
    'installanywhere': (b'installanywhere',),
}

INSTALLER_EXE_NAMES = {
    'setup.exe', 'install.exe', 'installer.exe', 'setupex.exe', 'setup32.exe',
    '_setup.exe', '_ins5576._mp', '_ins0432._mp', 'uninst.exe', 'uninstall.exe',
}

# Payload companions that travel with an installer stub.
COMPANION_SUFFIXES = {
    '.cab', '.msi', '.hdr', '.z', '.dat', '.ini', '.txt', '.log', '.cfg',
    '.inf', '.dll', '.ocx', '.exe', '.bin', '.pak', '.arc', '.001',
}

MAX_PROBE = 8 * 1024 * 1024


class IngestError(ValueError):
    """Raised when an upload cannot be identified or unpacked."""


@dataclass
class IngestResult:
    format: str
    detail: str = ''
    tool: str | None = None
    warnings: list[str] = field(default_factory=list)
    needs_run: bool = False
    entries: int = 0
    expanded_bytes: int = 0
    installer_files: list[str] = field(default_factory=list)
    payload_files: list[str] = field(default_factory=list)

    def as_dict(self):
        return {
            'format': self.format,
            'detail': self.detail,
            'tool': self.tool,
            'warnings': list(self.warnings),
            'needsRun': bool(self.needs_run),
            'entries': self.entries,
            'expandedBytes': self.expanded_bytes,
            'installerFiles': list(self.installer_files),
            'payloadFiles': list(self.payload_files),
        }


def available_tools():
    """Return the optional extractor tools that exist on this machine."""
    return {name: shutil.which(name) for name in TOOL_NAMES}


def _read_probe(path, limit=MAX_PROBE):
    with open(path, 'rb') as handle:
        return handle.read(limit)


def _u32(data, offset):
    if offset < 0 or offset + 4 > len(data):
        return None
    return int.from_bytes(data[offset:offset + 4], 'little')


def _header_signature(data):
    """Return ('pe'|'ne'|None, e_lfanew) for an MZ image."""
    if len(data) < 0x40 or not data.startswith(MZ_MAGIC):
        return None, None
    offset = _u32(data, 0x3c)
    if offset is None or offset + 2 > len(data):
        return None, offset
    signature = data[offset:offset + 2]
    if signature == b'PE':
        return 'pe', offset
    if signature == b'NE':
        return 'ne', offset
    return None, offset


def _brand(data):
    lowered = data.lower()
    for brand, markers in BRAND_MARKERS.items():
        if any(marker in lowered for marker in markers):
            return brand
    return None


def detect_format(path):
    """Identify an upload by magic bytes and well-known installer strings."""
    path = Path(path)
    data = _read_probe(path)
    head = data[:8]

    if head.startswith(MSI_MAGIC):
        return {'format': 'msi', 'detail': 'OLE compound document (MSI)',
                'machine': None, 'brand': None, 'confidence': 'high'}
    if head.startswith(CAB_MAGIC):
        return {'format': 'cab', 'detail': 'Microsoft Cabinet (MSCF)',
                'machine': None, 'brand': None, 'confidence': 'high'}
    if head.startswith(ZIP_MAGIC):
        return {'format': 'zip', 'detail': 'ZIP archive',
                'machine': None, 'brand': None, 'confidence': 'high'}
    if zipfile.is_zipfile(path):
        # An appended ZIP after an EXE stub still reports as a ZIP to zipfile.
        if data.startswith(MZ_MAGIC):
            return {'format': 'sfx-zip', 'detail': 'self-extracting ZIP (EXE stub + appended ZIP)',
                    'machine': 'pe', 'brand': _brand(data), 'confidence': 'high'}
        return {'format': 'zip', 'detail': 'ZIP archive',
                'machine': None, 'brand': None, 'confidence': 'high'}

    kind, offset = _header_signature(data)
    if kind == 'pe':
        brand = _brand(data)
        detail = 'PE32 image'
        if brand:
            detail = f'PE32 image ({brand} installer)'
        return {'format': 'pe', 'detail': detail, 'machine': 'pe',
                'brand': brand, 'confidence': 'medium' if brand else 'low'}
    if kind == 'ne':
        return {'format': 'ne16', 'detail': '16-bit NE image',
                'machine': 'ne', 'brand': _brand(data), 'confidence': 'high'}

    # An EXE may carry a CAB or ZIP payload without being a ZIP to zipfile.
    if data.startswith(MZ_MAGIC):
        if CAB_MAGIC in data:
            return {'format': 'sfx-cab', 'detail': 'self-extracting CAB (EXE stub + MSCF payload)',
                    'machine': 'pe', 'brand': _brand(data), 'confidence': 'medium'}
        brand = _brand(data)
        if brand:
            return {'format': brand, 'detail': f'installer stub ({brand})',
                    'machine': 'pe', 'brand': brand, 'confidence': 'medium'}
        return {'format': 'unknown', 'detail': 'MZ image without a recognised payload',
                'machine': 'pe', 'brand': None, 'confidence': 'low'}

    return {'format': 'unknown', 'detail': 'unrecognised upload',
            'machine': None, 'brand': None, 'confidence': 'low'}


def _run(args, timeout):
    try:
        result = subprocess.run(args, capture_output=True, timeout=timeout)
    except FileNotFoundError:
        raise IngestError(f'{args[0]} is not installed')
    except subprocess.TimeoutExpired:
        raise IngestError(f'{args[0]} timed out after {timeout}s')
    if result.returncode != 0:
        detail = (result.stdout + result.stderr).decode('utf-8', 'replace').strip()
        raise IngestError(f'{args[0]} failed ({result.returncode})' +
                          (f': {detail[:500]}' if detail else ''))
    return result


def _find_tool(*names):
    for name in names:
        found = shutil.which(name)
        if found:
            return found
    return None


def _extract_zip(path, destination, max_expanded, max_entries):
    """Extract a ZIP or appended SFX-ZIP with the standard library."""
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    total = 0
    entries = 0
    try:
        with zipfile.ZipFile(path) as archive:
            infos = archive.infolist()
            if len(infos) > max_entries:
                raise IngestError(f'ZIP exceeds {max_entries:,} entries')
            for info in infos:
                name = info.filename
                if not name or '\x00' in name or name.startswith('/') or '\\' in name:
                    raise IngestError(f'Unsafe ZIP member: {name!r}')
                parts = Path(name).parts
                if '..' in parts or (parts and parts[0].endswith(':')):
                    raise IngestError(f'Unsafe ZIP member: {name!r}')
                if stat.S_ISLNK(info.external_attr >> 16):
                    raise IngestError('Symlinks in ZIPs are not supported')
                target = destination / name
                if info.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(info) as source, open(target, 'wb') as sink:
                    while True:
                        chunk = source.read(1024 * 1024)
                        if not chunk:
                            break
                        total += len(chunk)
                        if total > max_expanded:
                            raise IngestError('ZIP exceeds the expanded size limit')
                        sink.write(chunk)
                entries += 1
    except zipfile.BadZipFile as error:
        raise IngestError(f'Invalid ZIP: {error}') from error
    return entries, total


def _extract_cab(path, destination, timeout):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    tool = _find_tool('cabextract')
    if tool:
        _run([tool, '-q', '-n', '-d', str(destination), str(path)], timeout)
        return 'cabextract'
    tool = _find_tool('7z', '7za')
    if tool:
        _run([tool, 'x', '-y', f'-o{destination}', str(path)], timeout)
        return Path(tool).name
    raise IngestError('CAB extraction needs cabextract or 7z')


def _extract_msi(path, destination, timeout):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    tool = _find_tool('msiextract')
    if tool:
        _run([tool, '-C', str(destination), str(path)], timeout)
        return 'msiextract'
    tool = _find_tool('7z', '7za')
    if tool:
        _run([tool, 'x', '-y', f'-o{destination}', str(path)], timeout)
        return Path(tool).name
    raise IngestError('MSI extraction needs msiextract or 7z')


def _extract_inno(path, destination, timeout):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    tool = _find_tool('innoextract')
    if tool:
        _run([tool, '-d', str(destination), str(path)], timeout)
        return 'innoextract'
    tool = _find_tool('7z', '7za')
    if tool:
        _run([tool, 'x', '-y', f'-o{destination}', str(path)], timeout)
        return Path(tool).name
    raise IngestError('Inno Setup extraction needs innoextract or 7z')


def _extract_unshield(path, destination, timeout):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    tool = _find_tool('unshield')
    if tool:
        _run([tool, 'x', '-d', str(destination), str(path)], timeout)
        return 'unshield'
    for name in ('i6comp', 'i5comp'):
        tool = _find_tool(name)
        if tool:
            _run([tool, 'x', str(path)], timeout)
            return name
    return None


def _extract_7z(path, destination, timeout):
    tool = _find_tool('7z', '7za')
    if not tool:
        return None
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    _run([tool, 'x', '-y', f'-o{destination}', str(path)], timeout)
    return Path(tool).name


def _find_magic_offsets(path, magic):
    """Stream a file looking for an embedded signature."""
    offsets = []
    position = 0
    tail = b''
    with open(path, 'rb') as handle:
        while True:
            chunk = handle.read(4 * 1024 * 1024)
            if not chunk:
                break
            buffer = tail + chunk
            base = position - len(tail)
            start = 0
            while True:
                found = buffer.find(magic, start)
                if found < 0:
                    break
                offsets.append(base + found)
                start = found + 1
            tail = buffer[-(len(magic) - 1):]
            position += len(chunk)
    return offsets


def _carve(path, offset, length, destination):
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with open(path, 'rb') as source, open(destination, 'wb') as sink:
        source.seek(offset)
        remaining = length
        while remaining != 0:
            chunk = source.read(min(1024 * 1024, remaining) if remaining > 0 else 1024 * 1024)
            if not chunk:
                break
            sink.write(chunk)
            if remaining > 0:
                remaining -= len(chunk)
    return destination


def _validate_tree(root, max_expanded, max_entries):
    root = Path(root).resolve()
    entries = 0
    total = 0
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for name in dirnames + filenames:
            path = Path(dirpath) / name
            if path.is_symlink():
                raise IngestError(f'Symlink in extracted tree: {path.relative_to(root)}')
        for name in filenames:
            path = Path(dirpath) / name
            resolved = path.resolve()
            if not resolved.is_relative_to(root):
                raise IngestError(f'Extracted path escapes destination: {path}')
            entries += 1
            if entries > max_entries:
                raise IngestError(f'Extracted tree exceeds {max_entries:,} entries')
            total += path.stat().st_size
            if total > max_expanded:
                raise IngestError('Extracted tree exceeds the expanded size limit')
    return entries, total


def _installer_relative_paths(destination):
    found = []
    for path in sorted(Path(destination).rglob('*')):
        if not path.is_file():
            continue
        try:
            info = detect_format(path)
        except OSError:
            continue
        if info['format'] in ('msi', 'cab', 'sfx-cab', 'sfx-zip', 'ne16',
                              'installshield', 'inno', 'nsis', 'wise',
                              'installanywhere'):
            found.append(path.relative_to(destination).as_posix())
        elif info['format'] == 'pe' and info.get('brand'):
            found.append(path.relative_to(destination).as_posix())
        elif path.name.lower() in INSTALLER_EXE_NAMES:
            found.append(path.relative_to(destination).as_posix())
    return found


def has_embedded_payload(path, max_scan=64 * 1024 * 1024):
    """Return True when an EXE carries a ZIP or CAB payload after its stub."""
    path = Path(path)
    scanned = 0
    for magic in (ZIP_MAGIC, CAB_MAGIC):
        for offset in _find_magic_offsets(path, magic):
            if offset > 0:
                return True
            scanned += 1
            if scanned > 64:
                return False
            if offset + max_scan < 0:
                return False
    return False


def _payload_from_embedded(path, destination, max_expanded, max_entries, timeout, result):
    """Try ZIP and CAB signatures embedded inside an EXE/installer."""
    extracted = False
    for magic, kind in ((ZIP_MAGIC, 'zip'), (CAB_MAGIC, 'cab')):
        for offset in _find_magic_offsets(path, magic):
            if kind == 'cab':
                with open(path, 'rb') as handle:
                    handle.seek(offset)
                    header = handle.read(16)
                size = _u32(header, 8) or 0
                length = size if size > 0 else -1
            else:
                length = -1
            carved = Path(tempfile.mkdtemp(prefix='wlam-carve-')) / f'payload.{kind}'
            try:
                _carve(path, offset, length, carved)
                if kind == 'zip':
                    try:
                        _extract_zip(carved, destination, max_expanded, max_entries)
                        result.warnings.append(f'Extracted embedded ZIP at offset {offset}')
                        extracted = True
                    except IngestError:
                        continue
                else:
                    try:
                        _extract_cab(carved, destination, timeout)
                        result.warnings.append(f'Extracted embedded CAB at offset {offset}')
                        extracted = True
                    except IngestError:
                        continue
            finally:
                shutil.rmtree(carved.parent, ignore_errors=True)
    return extracted


def _extract_installer(path, destination, max_expanded, max_entries, timeout, result):
    """Best-effort extraction for PE/NE/InstallShield/Inno/NSIS/unknown stubs."""
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    fmt = result.format

    # Brand-specific tools first, then generic 7z, then cabextract, then carving.
    if fmt == 'installshield':
        tool = _extract_unshield(path, destination, timeout)
        if tool:
            result.tool = tool
            return
    if fmt == 'inno':
        try:
            result.tool = _extract_inno(path, destination, timeout)
            return
        except IngestError as error:
            result.warnings.append(str(error))
    if fmt in ('nsis', 'installshield', 'wise', 'installanywhere', 'pe', 'ne16', 'unknown', 'sfx-cab', 'sfx-zip'):
        try:
            tool = _extract_7z(path, destination, timeout)
            if tool:
                result.tool = tool
                return
        except IngestError as error:
            result.warnings.append(str(error))
        try:
            result.tool = _extract_cab(path, destination, timeout)
            return
        except IngestError as error:
            result.warnings.append(str(error))

    if _payload_from_embedded(path, destination, max_expanded, max_entries, timeout, result):
        result.tool = 'embedded-payload'
        return

    result.needs_run = True


def _extract_companions(destination, max_expanded, max_entries, timeout, result, companions=None, skip=()):
    """Extract CAB/MSI companions sitting next to an installer stub."""
    destination = Path(destination)
    search_root = Path(companions) if companions is not None else destination
    skip = {Path(item).resolve() for item in skip}
    extracted = False
    for path in sorted(search_root.rglob('*')):
        if not path.is_file() or path.resolve() in skip:
            continue
        info = detect_format(path)
        if info['format'] == 'cab':
            try:
                _extract_cab(path, destination, timeout)
                result.warnings.append(f'Extracted companion CAB {path.name}')
                extracted = True
            except IngestError as error:
                result.warnings.append(str(error))
        elif info['format'] == 'msi':
            try:
                _extract_msi(path, destination, timeout)
                result.warnings.append(f'Extracted companion MSI {path.name}')
                extracted = True
            except IngestError as error:
                result.warnings.append(str(error))
    return extracted


def _payload_files(destination, installer_files):
    installer_set = set(installer_files)
    files = []
    for path in sorted(Path(destination).rglob('*')):
        if not path.is_file():
            continue
        relative = path.relative_to(destination).as_posix()
        if relative not in installer_set:
            files.append(relative)
    return files


def ingest(path, destination, *, max_expanded, max_entries, timeout=120, companions=None):
    """Detect and unpack ``path`` into ``destination``.

    ``companions`` optionally points at the directory that holds the installer
    stub and its sibling CAB/MSI parts. Extracted payload always goes to
    ``destination``; the companion directory is only read.

    Returns an :class:`IngestResult`. Raises :class:`IngestError` when the
    upload cannot be identified or when no payload can be found without running
    the installer.
    """
    path = Path(path)
    destination = Path(destination)
    info = detect_format(path)
    result = IngestResult(format=info['format'], detail=info['detail'])
    destination.mkdir(parents=True, exist_ok=True)

    try:
        if info['format'] in ('zip', 'sfx-zip'):
            entries, total = _extract_zip(path, destination, max_expanded, max_entries)
            result.entries, result.expanded_bytes = entries, total
            result.tool = 'zipfile'
        elif info['format'] in ('cab', 'sfx-cab'):
            result.tool = _extract_cab(path, destination, timeout)
        elif info['format'] == 'msi':
            result.tool = _extract_msi(path, destination, timeout)
        elif info['format'] == 'inno':
            result.tool = _extract_inno(path, destination, timeout)
        else:
            _extract_installer(path, destination, max_expanded, max_entries, timeout, result)

        if result.format in ('pe', 'ne16', 'installshield', 'wise', 'installanywhere',
                             'unknown', 'sfx-cab', 'sfx-zip', 'inno', 'nsis'):
            _extract_companions(destination, max_expanded, max_entries, timeout, result,
                                companions=companions, skip=(path,))

        result.entries, result.expanded_bytes = _validate_tree(
            destination, max_expanded, max_entries)
        result.installer_files = _installer_relative_paths(destination)
        result.payload_files = _payload_files(destination, result.installer_files)

        if not result.payload_files and not result.needs_run:
            result.needs_run = True
        if result.needs_run:
            result.warnings.append(
                'No static payload was found. The installer likely unpacks at run time; '
                'extract it manually or wait for Win32/Win16 installer execution support.')
        return result
    except IngestError:
        shutil.rmtree(destination, ignore_errors=True)
        raise
