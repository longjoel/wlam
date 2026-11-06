#!/usr/bin/env python3
"""Recompile an uploaded Windows application ZIP into a desktop bundle.

The web dev server calls this module. It extracts an archive into
``target/.recompile/<job>/source``, analyzes the selected executable with
``wlam analyze``, recompiles it with ``wlam recompile`` and publishes the
bundle at ``target/<slug>/bundle/`` so the desktop's bundle scan finds it.

Only the standard library is used. Native subprocesses analyze and compile the
PE; the archive is never executed as a native application.
"""
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import threading
import time
from urllib.parse import quote
import uuid
import zipfile

import installer_ingest

MAX_UPLOAD = 64 * 1024 * 1024
MAX_EXPANDED = 256 * 1024 * 1024
MAX_ENTRIES = 10000
MAX_ASSETS = 192 * 1024 * 1024
MAX_ACTIVE = 8
LOG_LIMIT = 512 * 1024
ANALYZE_TIMEOUT = 120
BUILD_TIMEOUT = 600
INGEST_TIMEOUT = 120

INSTALLER_MODES = ('auto', 'source', 'installer')
INSTALLER_FORMATS = {
    'msi', 'cab', 'sfx-cab', 'sfx-zip', 'ne16', 'installshield',
    'inno', 'nsis', 'wise', 'installanywhere',
}

STEP_NAMES = [
    ('archive', 'Read archive'),
    ('analyze', 'Analyze image'),
    ('recompile', 'Recompile to WebAssembly'),
    ('package', 'Package browser bundle'),
    ('report', 'Generate report'),
]
ACTIVE_STATES = ('analyzing', 'compiling')

LIMITATIONS = [
    'Static analysis and recompilation do not verify instruction, graphics, audio or filesystem compatibility.',
    'The browser host refuses unimplemented Win32 APIs by name instead of returning fabricated values.',
    'The guest runs in the same Wasm module as the host layer; only run applications you trust.',
    'Installer support extracts static payloads. It does not run setup logic, registry writes or custom install scripts.',
]


def utcnow():
    return datetime.now(timezone.utc).isoformat(timespec='seconds').replace('+00:00', 'Z')


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def safe_path(root, name):
    """Resolve an archive member below root, rejecting escapes and odd paths."""
    if not name or '\\' in name or '\x00' in name:
        raise ValueError('Invalid path in archive')
    parts = PurePosixPath(name)
    if parts.is_absolute() or '..' in parts.parts or ':' in name:
        raise ValueError('Invalid path in archive')
    result = (root / name).resolve()
    if not result.is_relative_to(root.resolve()):
        raise ValueError('Path escapes project')
    return result


def extract_archive(data, destination):
    """Safely extract a ZIP, enforcing entry, size and link limits."""
    destination = Path(destination)
    destination.mkdir(parents=True)
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            entries = archive.infolist()
            if len(entries) > MAX_ENTRIES:
                raise ValueError(f'ZIP exceeds {MAX_ENTRIES:,} entries')
            if sum(entry.file_size for entry in entries) > MAX_EXPANDED:
                raise ValueError('ZIP exceeds 256 MiB expanded')
            seen = set()
            total = 0
            for entry in entries:
                path = safe_path(destination, entry.filename)
                if path in seen or stat.S_ISLNK(entry.external_attr >> 16):
                    raise ValueError('Duplicate paths and symlinks are not supported')
                seen.add(path)
                if entry.flag_bits & 1:
                    raise ValueError('Encrypted ZIPs are not supported')
                if entry.is_dir():
                    path.mkdir(parents=True, exist_ok=True)
                    continue
                path.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(entry) as source, path.open('wb') as target:
                    for chunk in iter(lambda: source.read(1024 * 1024), b''):
                        total += len(chunk)
                        if total > MAX_EXPANDED:
                            raise ValueError('ZIP exceeds 256 MiB expanded')
                        target.write(chunk)
        return len(entries), total
    except Exception:
        shutil.rmtree(destination, ignore_errors=True)
        raise


def slugify(text):
    slug = re.sub(r'[^a-z0-9]+', '-', text.lower()).strip('-')
    slug = re.sub(r'-{2,}', '-', slug)[:60].strip('-')
    if not slug or slug.startswith('.') or '.' in slug:
        return 'application'
    return slug


def friendly_name(text):
    return ' '.join(word[:1].upper() + word[1:] for word in re.split(r'[\s._-]+', text) if word)[:120] or 'Application'


SYSTEM_DLLS = {
    'kernel32', 'kernelbase', 'user32', 'gdi32', 'advapi32', 'shell32',
    'comctl32', 'comdlg32', 'winmm', 'shlwapi', 'ole32', 'oleaut32',
    'version', 'ws2_32', 'winspool', 'setupapi', 'rpcrt4', 'uxtheme',
    'msimg32', 'imm32', 'dbghelp', 'psapi', 'crypt32', 'secur32',
    'iphlpapi', 'mpr', 'netapi32', 'wininet', 'urlmon', 'opengl32', 'glu32',
}


def is_system_dll(name):
    stem = Path(name).name.lower()
    if stem.endswith('.dll'):
        stem = stem[:-4]
    return (stem in SYSTEM_DLLS or stem.startswith('msvcr')
            or stem.startswith('vcruntime') or stem.startswith('api-ms-win-'))


def choose_executable(executables, archive_name):
    """Prefer an EXE whose name matches the archive, else the shortest path."""
    stem = Path(archive_name).stem.lower()
    for name in executables:
        if Path(name).stem.lower() == stem:
            return name
    return min(executables, key=lambda name: (name.count('/'), len(name), name.lower()))


def _safe_upload_name(name):
    name = re.sub(r'[\x00-\x1f\x7f/\\]', ' ', name or '').strip()
    return Path(name).name[:160] or 'upload.bin'


def _scan_installers(directory):
    """List installer-like files inside an extracted archive."""
    found = []
    directory = Path(directory)
    for path in sorted(directory.rglob('*')):
        if not path.is_file():
            continue
        try:
            info = installer_ingest.detect_format(path)
        except OSError:
            continue
        if (info['format'] in INSTALLER_FORMATS
                or (info['format'] == 'pe' and info.get('brand'))
                or path.name.lower() in installer_ingest.INSTALLER_EXE_NAMES):
            found.append({'path': path.relative_to(directory).as_posix(), **info})
    return found


def _should_auto_ingest(directory, installers):
    """True when an archive looks like an installer plus companions."""
    if not installers:
        return False
    directory = Path(directory)
    files = [path for path in directory.rglob('*') if path.is_file()]
    installer_paths = {item['path'] for item in installers}
    non_installer_exes = [
        path for path in files
        if path.suffix.lower() == '.exe'
        and path.relative_to(directory).as_posix() not in installer_paths
    ]
    if non_installer_exes:
        return False
    if any(item['format'] in ('msi', 'cab', 'sfx-cab') for item in installers):
        return True
    others = [
        path for path in files
        if path.relative_to(directory).as_posix() not in installer_paths
    ]
    return all(path.suffix.lower() in installer_ingest.COMPANION_SUFFIXES for path in others)


def _pick_installer(installers):
    """Prefer a setup/install stub, then an MSI/CAB, then the first installer."""
    for name in ('setup.exe', 'install.exe', 'installer.exe', 'setupex.exe', 'setup32.exe'):
        for item in installers:
            if Path(item['path']).name.lower() == name:
                return item['path']
    for format_name in ('msi', 'cab', 'sfx-cab', 'sfx-zip', 'ne16',
                        'installshield', 'inno', 'nsis', 'wise', 'installanywhere', 'pe'):
        for item in installers:
            if item['format'] == format_name:
                return item['path']
    return installers[0]['path']


def _merge_tree(source, destination):
    source = Path(source)
    destination = Path(destination)
    for path in sorted(source.rglob('*')):
        relative = path.relative_to(source)
        target = destination / relative
        if path.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        elif path.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)


def _is_installer_dll(name):
    base = Path(name).name.lower()
    return (base in ('setup.dll', '_setup.dll')
            or base.startswith('_is')
            or base.startswith('install')
            or base.startswith('uninstall'))


def markdown_cell(value):
    return str(value).replace('|', '\\|').replace('\n', ' ')


def report_markdown(report):
    def text(value):
        return '- ' if value is None or value == '' else str(value)

    lines = [
        f"# Recompile report: {report.get('name', 'Application')}",
        '',
        f"Generated: {report.get('generated')}",
        '',
        '## Source',
        '',
        f"- Archive: {text(report['source'].get('archive'))}",
        f"- Archive bytes: {report['source'].get('bytes')}",
        f"- Archive entries: {report['source'].get('entries')}",
        f"- Entry executable: {text(report['source'].get('executable'))}",
        f"- Executable SHA-256: {text(report['source'].get('executableSha256'))}",
        '',
        '## Installer',
        '',
    ]
    installer = report.get('installer') or {}
    if installer:
        lines.append(f"- Format: {text(installer.get('format'))}")
        lines.append(f"- Detail: {text(installer.get('detail'))}")
        lines.append(f"- Extractor: {text(installer.get('tool'))}")
        lines.append(f"- Static payload found: {'no' if installer.get('needsRun') else 'yes'}")
        for warning in installer.get('warnings') or []:
            lines.append(f"- Warning: {warning}")
    else:
        lines.append('No installer metadata recorded.')
    lines.extend([
        '',
        '## Toolchain',
        '',
        f"- `wlam` binary: `{text(report['tool'].get('binary'))}`",
        f"- Analysis exit: {text(report['tool'].get('analysisExit'))}",
        f"- Recompile exit: {text(report['tool'].get('recompileExit'))}",
        f"- Build seconds: {text(report['build'].get('seconds'))}",
        '',
        '## Compatibility',
        '',
    ])
    analysis = report.get('analysis') or {}
    lines.append(f"- Image: {text(analysis.get('kind'))} / {text(analysis.get('machine'))}")
    lines.append(f"- Imports: {text(analysis.get('imports'))}; exports: {text(analysis.get('exports'))}")
    for state, count in sorted((analysis.get('compatibility') or {}).items()):
        lines.append(f"- {state}: {count}")
    blockers = analysis.get('blocking') or []
    lines.append('')
    lines.append('### Blocking imports')
    lines.append('')
    if blockers:
        lines.append('| Library | Symbol | State | Reason |')
        lines.append('| --- | --- | --- | --- |')
        for blocker in blockers:
            lines.append('| {} | {} | {} | {} |'.format(
                markdown_cell(blocker.get('library', '')),
                markdown_cell(blocker.get('symbol', '')),
                markdown_cell(blocker.get('state', '')),
                markdown_cell(blocker.get('reason', '')),
            ))
    else:
        lines.append('None reported. A clean report does not prove runtime compatibility.')
    if report.get('matrix'):
        lines.extend(['', '## Import compatibility matrix', '', report['matrix'].rstrip()])
    lines.extend(['', '## Linked DLLs', ''])
    if report.get('dlls'):
        lines.append('| DLL | Guest base | DllMain |')
        lines.append('| --- | ---: | ---: |')
        for dll in report['dlls']:
            lines.append('| {} | `{:#x}` | `{:#x}` |'.format(
                markdown_cell(dll.get('name', '')),
                dll.get('base', 0),
                dll.get('entry', 0),
            ))
    else:
        lines.append('No supplied DLLs were linked.')
    lines.extend(['', '## Bundle', ''])
    bundle = report.get('bundle') or {}
    lines.append(f"- Desktop ID: `{text(bundle.get('id'))}`")
    lines.append(f"- Manifest URL: `{text(bundle.get('manifest'))}`")
    lines.append(f"- Renderer: {text((report.get('runtime') or {}).get('renderer'))}")
    lines.extend(['', '| Artifact | Bytes | SHA-256 |', '| --- | ---: | --- |'])
    for artifact in bundle.get('artifacts') or []:
        lines.append('| {} | {} | {} |'.format(
            markdown_cell(artifact.get('path', '')),
            artifact.get('bytes', ''),
            markdown_cell(artifact.get('sha256', '')),
        ))
    assets = report.get('assets') or []
    lines.extend(['', '## Data files', ''])
    if assets:
        lines.append(f'{len(assets)} archive file(s) packaged under `files/`.')
        lines.append('')
        lines.append('| Guest path | Bytes |')
        lines.append('| --- | ---: |')
        for asset in assets[:200]:
            lines.append('| {} | {} |'.format(markdown_cell(asset.get('path', '')), asset.get('bytes', '')))
        if len(assets) > 200:
            lines.append(f'| … {len(assets) - 200} more | |')
    else:
        lines.append('No data files were packaged.')
    lines.extend(['', '## Build log', '', '```text', (report.get('build') or {}).get('log', '').rstrip(), '```', ''])
    lines.extend(['## Limitations', ''])
    lines.extend(f'- {item}' for item in report.get('limitations') or [])
    lines.append('')
    return '\n'.join(lines)


class RecompileService:
    def __init__(self, root, wlam=None):
        self.root = Path(root).resolve()
        self.wlam = Path(wlam or os.environ.get('WLAM_BIN') or self.root / 'target' / 'debug' / 'wlam')
        self.target = self.root / 'target'
        self.work_root = self.target / '.recompile'
        self.work_root.mkdir(parents=True, exist_ok=True)
        self.jobs = {}
        self.lock = threading.RLock()
        self.pool = ThreadPoolExecutor(max_workers=1)

    # ---- public API -----------------------------------------------------

    def config(self):
        available = self.wlam.is_file() and os.access(self.wlam, os.X_OK)
        return {
            'available': available,
            'binary': str(self.wlam),
            'maxUpload': MAX_UPLOAD,
            'maxExpanded': MAX_EXPANDED,
            'maxEntries': MAX_ENTRIES,
            'installerModes': list(INSTALLER_MODES),
            'installerFormats': sorted(INSTALLER_FORMATS),
            'installerTools': installer_ingest.available_tools(),
            'limitations': LIMITATIONS,
        }

    def close(self):
        self.pool.shutdown(wait=True)

    def get(self, ident):
        with self.lock:
            job = self.jobs.get(ident)
            if job is None:
                raise KeyError('Recompile job not found')
            return job

    def public(self, job):
        # Pollers copy a job while the worker thread mutates its log and steps.
        with self.lock:
            return json.loads(json.dumps(job))

    def upload(self, data, name, mode='auto'):
        if not data:
            raise ValueError('The upload is empty')
        if len(data) > MAX_UPLOAD:
            raise ValueError('Upload limit is 64 MiB')
        mode = (mode or 'auto').strip().lower()
        if mode not in INSTALLER_MODES:
            raise ValueError(f'Unsupported upload mode: {mode}')
        name = _safe_upload_name(name)
        ident = uuid.uuid4().hex
        folder = self.work_root / ident
        source = folder / 'source'
        upload_path = folder / 'upload.bin'
        try:
            folder.mkdir(parents=True, exist_ok=True)
            upload_path.write_bytes(data)
            info = installer_ingest.detect_format(upload_path)
            installer = None
            entries = expanded = 0

            if mode == 'source':
                if info['format'] in ('zip', 'sfx-zip'):
                    entries, expanded = extract_archive(data, source)
                else:
                    source.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(upload_path, source / name)
                    entries, expanded = 1, len(data)
            elif info['format'] in ('zip', 'sfx-zip'):
                unpacked = folder / 'unpacked'
                entries, expanded = extract_archive(data, unpacked)
                installers = _scan_installers(unpacked)
                ingest_it = bool(installers) and (
                    mode == 'installer' or _should_auto_ingest(unpacked, installers))
                shutil.move(str(unpacked), str(source))
                if ingest_it:
                    primary = _pick_installer(installers)
                    payload = folder / 'payload'
                    try:
                        result = installer_ingest.ingest(
                            source / primary, payload,
                            max_expanded=MAX_EXPANDED, max_entries=MAX_ENTRIES,
                            timeout=INGEST_TIMEOUT, companions=source)
                    except installer_ingest.IngestError as error:
                        installer = {
                            'format': info['format'], 'detail': info['detail'],
                            'needsRun': True, 'warnings': [str(error)],
                            'installerFiles': [item['path'] for item in installers],
                        }
                    else:
                        installer = result.as_dict()
                        installer['source'] = primary
                        if result.payload_files:
                            _merge_tree(payload, source)
                            entries += result.entries
                            expanded += result.expanded_bytes
                        else:
                            installer['needsRun'] = True
                            installer['warnings'].append(
                                'Static extraction found no payload; the archive remains as a source tree.')
                    finally:
                        shutil.rmtree(payload, ignore_errors=True)
                else:
                    installer = {
                        'format': info['format'], 'detail': info['detail'],
                        'needsRun': False, 'warnings': [],
                        'installerFiles': [item['path'] for item in installers],
                    }
            else:
                source.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(upload_path, source / name)
                entries, expanded = 1, len(data)
                looks_installer = (
                    info['format'] in INSTALLER_FORMATS
                    or bool(info.get('brand'))
                    or name.lower() in installer_ingest.INSTALLER_EXE_NAMES
                    or installer_ingest.has_embedded_payload(upload_path))
                if looks_installer and mode != 'source':
                    payload = folder / 'payload'
                    try:
                        result = installer_ingest.ingest(
                            upload_path, payload,
                            max_expanded=MAX_EXPANDED, max_entries=MAX_ENTRIES,
                            timeout=INGEST_TIMEOUT)
                    except installer_ingest.IngestError as error:
                        installer = {
                            'format': info['format'], 'detail': info['detail'],
                            'needsRun': True, 'warnings': [str(error)],
                        }
                    else:
                        installer = result.as_dict()
                        installer['source'] = name
                        if result.payload_files:
                            _merge_tree(payload, source)
                            entries += result.entries
                            expanded += result.expanded_bytes
                        else:
                            installer['needsRun'] = True
                            installer['warnings'].append(
                                'Static extraction found no payload; the upload remains as an application.')
                    finally:
                        shutil.rmtree(payload, ignore_errors=True)
                else:
                    installer = {
                        'format': info['format'], 'detail': info['detail'],
                        'needsRun': False, 'warnings': [],
                    }

            total = sum(path.stat().st_size for path in source.rglob('*') if path.is_file())
            if total > MAX_EXPANDED:
                raise ValueError('Upload plus extracted payload exceeds the expanded size limit')

            files = sorted(path.relative_to(source).as_posix() for path in source.rglob('*') if path.is_file())
            executables = [path for path in files if path.lower().endswith('.exe')]
            installer_files = set(installer.get('installerFiles') or [])
            app_executables = [path for path in executables if path not in installer_files]
            if app_executables:
                executables = app_executables
            if not executables:
                if installer.get('needsRun'):
                    raise ValueError(
                        'The installer only unpacks when executed, so no Windows .exe could be '
                        'extracted statically. Extract it manually, or upload an already-extracted '
                        'folder as a ZIP.')
                raise ValueError('No Windows .exe found in this upload')
            selected = choose_executable(executables, name)
            dlls = [path for path in files
                    if path.lower().endswith('.dll') and not is_system_dll(path)
                    and path not in installer_files and not _is_installer_dll(path)]
            job = {
                'id': ident,
                'name': friendly_name(Path(name).stem),
                'archive': name,
                'archiveBytes': len(data),
                'archiveEntries': entries,
                'expandedBytes': expanded,
                'files': files,
                'executables': executables,
                'dlls': dlls,
                'selected': selected,
                'includeAssets': True,
                'includeDlls': True,
                'installer': installer,
                'installerFiles': sorted(installer_files),
                'state': 'review',
                'created': utcnow(),
                'log': '',
                'error': None,
                'analysis': None,
                'matrix': None,
                'steps': [{'name': label, 'state': 'done' if key == 'archive' else 'pending'}
                          for key, label in STEP_NAMES],
                'report': None,
                'bundle': None,
            }
            with self.lock:
                if sum(item['state'] in ACTIVE_STATES for item in self.jobs.values()) >= MAX_ACTIVE:
                    raise ValueError('Recompile queue is full; wait for a job to finish')
                self.jobs[ident] = job
            return self.public(job)
        except Exception:
            shutil.rmtree(folder, ignore_errors=True)
            raise

    def analyze(self, ident, selected, include_assets=True, include_dlls=True):
        with self.lock:
            job = self.get(ident)
            if job['state'] in ACTIVE_STATES:
                raise ValueError('This recompile job is already running')
            if selected not in job['executables']:
                raise ValueError('Select an executable from this project')
            if not self.wlam.is_file():
                raise ValueError(f'Analyzer binary not found: {self.wlam}. Build wlam-cli or set WLAM_BIN.')
            job.update(selected=selected, includeAssets=bool(include_assets),
                       includeDlls=bool(include_dlls), state='analyzing',
                       error=None, analysis=None, matrix=None, report=None, bundle=None, log='')
            job['steps'] = [{'name': label, 'state': 'done' if key == 'archive' else
                             'running' if key == 'analyze' else 'pending'} for key, label in STEP_NAMES]
            snapshot = self.public(job)
            self.pool.submit(self._analyze_work, ident)
            return snapshot

    def build(self, ident):
        with self.lock:
            job = self.get(ident)
            if job['state'] in ACTIVE_STATES:
                raise ValueError('This recompile job is already running')
            if not job.get('analysis'):
                raise ValueError('Analyze the image before recompiling it')
            if job['analysis'].get('blocking'):
                raise ValueError('Blocking imports found. Resolve them before recompiling.')
            if not self.wlam.is_file():
                raise ValueError(f'Compiler binary not found: {self.wlam}. Build wlam-cli or set WLAM_BIN.')
            job.update(state='compiling', error=None, report=None, bundle=None)
            self._set_step(job, 'analyze', 'done')
            self._set_step(job, 'recompile', 'running')
            snapshot = self.public(job)
            self.pool.submit(self._build_work, ident)
            return snapshot

    def delete(self, ident):
        with self.lock:
            job = self.jobs.get(ident)
            if job is None:
                raise KeyError('Recompile job not found')
            if job['state'] in ACTIVE_STATES:
                raise ValueError('A running recompile job cannot be deleted')
            self.jobs.pop(ident, None)
        shutil.rmtree(self.work_root / ident, ignore_errors=True)

    # ---- workers --------------------------------------------------------

    @staticmethod
    def _set_step(job, key, state):
        index = next(index for index, (name, _) in enumerate(STEP_NAMES) if name == key)
        job['steps'][index]['state'] = state

    def _append_log(self, job, text):
        if not text:
            return
        job['log'] = (job['log'] + text)[-LOG_LIMIT:]

    def _dll_args(self, job, source):
        if not job.get('includeDlls'):
            return []
        args = []
        for relative in job.get('dlls', []):
            args.extend(['--dll', str(safe_path(source, relative))])
        return args

    def _analyze_work(self, ident):
        with self.lock:
            job = self.get(ident)
        folder = self.work_root / ident
        matrix_path = folder / 'compatibility.md'
        try:
            exe = safe_path(folder / 'source', job['selected'])
            matrix_path.unlink(missing_ok=True)
            command = [str(self.wlam), 'analyze', str(exe), '--json', '--no-oracle',
                       '--matrix', str(matrix_path)]
            command.extend(self._dll_args(job, folder / 'source'))
            result = subprocess.run(command, cwd=folder, capture_output=True,
                                    timeout=ANALYZE_TIMEOUT)
            log = result.stderr.decode('utf-8', errors='replace')
            with self.lock:
                job = self.get(ident)
                self._append_log(job, log)
                job['analysisExit'] = result.returncode
                if result.returncode not in (0, 2):
                    raise ValueError(log.strip() or f'Image analysis failed with status {result.returncode}')
                try:
                    job['analysis'] = json.loads(result.stdout)
                except ValueError as error:
                    raise ValueError(f'Analyzer did not return JSON: {error}')
                if matrix_path.is_file():
                    job['matrix'] = matrix_path.read_text(encoding='utf-8', errors='replace')[:1024 * 1024]
                self._set_step(job, 'analyze', 'done')
                job['state'] = 'analyzed'
        except Exception as error:
            with self.lock:
                job = self.get(ident)
                job.update(state='failed', error=str(error))
                self._set_step(job, 'analyze', 'failed')

    def _build_work(self, ident):
        with self.lock:
            job = self.get(ident)
        folder = self.work_root / ident
        source = folder / 'source'
        work = folder / 'build'
        started = time.monotonic()
        stage = None
        try:
            exe = safe_path(source, job['selected'])
            shutil.rmtree(work, ignore_errors=True)
            with self.lock:
                job = self.get(ident)
            command = [str(self.wlam), 'recompile', str(exe), '-o', str(work)]
            command.extend(self._dll_args(job, source))
            result = subprocess.run(command, cwd=folder, capture_output=True,
                                    timeout=BUILD_TIMEOUT)
            log = (result.stdout + result.stderr).decode('utf-8', errors='replace')
            with self.lock:
                job = self.get(ident)
                self._append_log(job, log)
                job['recompileExit'] = result.returncode
                if result.returncode:
                    self._set_step(job, 'recompile', 'failed')
                    detail = log.strip()
                    raise ValueError(f'Recompile failed with status {result.returncode}' +
                                     (f': {detail}' if detail else ''))
                self._set_step(job, 'recompile', 'done')
                self._set_step(job, 'package', 'running')
            for name in ('guest.wasm', 'image.bin', 'manifest.json'):
                if not (work / name).is_file():
                    raise ValueError(f'Recompiler did not produce {name}')
            manifest = json.loads((work / 'manifest.json').read_text(encoding='utf-8'))
            if manifest.get('version') != 1:
                raise ValueError('Recompiler produced an unsupported manifest version')
            with self.lock:
                job = self.get(ident)
            stage = self._stage_bundle(job, source, work, manifest)
            with self.lock:
                job = self.get(ident)
                self._set_step(job, 'package', 'done')
                self._set_step(job, 'report', 'running')
            report = self._write_report(job, stage, time.monotonic() - started)
            with self.lock:
                job = self.get(ident)
                self._set_step(job, 'report', 'done')
                job['report'] = report
                job['bundle'] = stage['bundle']
                job['state'] = 'ready'
        except Exception as error:
            with self.lock:
                job = self.get(ident)
                job.update(state='failed', error=str(error))
                for step in job['steps']:
                    if step['state'] == 'running':
                        step['state'] = 'failed'
            if stage is not None:
                shutil.rmtree(self.target / stage['slug'], ignore_errors=True)

    def _allocate_slug(self, job):
        slug = slugify(job['name'])
        with self.lock:
            candidate = slug
            index = 2
            while (self.target / candidate).exists():
                candidate = f'{slug}-{index}'
                index += 1
            (self.target / candidate).mkdir(parents=True)
            return candidate

    def _stage_bundle(self, job, source, work, manifest):
        slug = self._allocate_slug(job)
        destination = self.target / slug / 'bundle'
        destination.mkdir(parents=True)
        try:
            for name in ('guest.wasm', 'image.bin', 'manifest.json'):
                shutil.copyfile(work / name, destination / name)
            assets = []
            if job.get('includeAssets'):
                assets = self._package_assets(job, source, destination)
            manifest['name'] = job['name']
            if assets:
                manifest['files'] = assets
            (destination / 'manifest.json').write_text(
                json.dumps(manifest, separators=(',', ':')), encoding='utf-8')
            if job.get('matrix'):
                (destination / 'compatibility.md').write_text(job['matrix'], encoding='utf-8')
            imports = manifest.get('imports')
            libraries = {
                str(item.get('library', '')).lower()
                for item in imports if isinstance(item, dict)
            } if isinstance(imports, list) else set()
            artifacts = []
            for path in sorted(destination.rglob('*')):
                if path.is_file() and not path.name.startswith('report.'):
                    relative = path.relative_to(destination).as_posix()
                    artifacts.append({'path': relative, 'bytes': path.stat().st_size,
                                      'sha256': sha256_file(path)})
            bundle = {
                'id': slug,
                'name': job['name'],
                'manifest': f'/target/{slug}/bundle/manifest.json',
                'renderer': 'webgl' if 'opengl32.dll' in libraries else 'cpu',
                'report': f'/target/{slug}/bundle/report.json',
                'reportMarkdown': f'/target/{slug}/bundle/report.md',
            }
            if job.get('matrix'):
                bundle['matrix'] = f'/target/{slug}/bundle/compatibility.md'
            return {'slug': slug, 'bundle': bundle, 'artifacts': artifacts, 'assets': assets,
                    'manifest': manifest, 'dlls': manifest.get('dlls', [])}
        except Exception:
            shutil.rmtree(self.target / slug, ignore_errors=True)
            raise

    def _package_assets(self, job, source, destination):
        assets = []
        total = 0
        installer_files = set(job.get('installerFiles') or [])
        for relative in job['files']:
            if relative == job['selected'] or relative.lower().endswith('.dll'):
                # DLL code is linked into guest.wasm/image.bin, not a data asset.
                continue
            if relative in installer_files:
                # Installer stubs and payload containers are not app data.
                continue
            path = safe_path(source, relative)
            if not path.is_file():
                continue
            total += path.stat().st_size
            if total > MAX_ASSETS:
                raise ValueError(f'Data files exceed the {MAX_ASSETS // (1024 * 1024)} MiB bundle limit. Uncheck data files.')
            target = destination / 'files' / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
            assets.append({
                'path': 'C:\\' + relative.replace('/', '\\'),
                'url': 'files/' + quote(relative, safe='/'),
                'bytes': target.stat().st_size,
            })
        return assets

    def _write_report(self, job, stage, seconds):
        source_root = self.work_root / job['id'] / 'source'
        exe = safe_path(source_root, job['selected'])
        binary = str(self.wlam)
        report = {
            'version': 1,
            'kind': 'wlam-recompile-report',
            'generated': utcnow(),
            'name': job['name'],
            'source': {
                'archive': job['archive'],
                'bytes': job['archiveBytes'],
                'entries': job['archiveEntries'],
                'expandedBytes': job['expandedBytes'],
                'executable': job['selected'],
                'executableBytes': exe.stat().st_size,
                'executableSha256': sha256_file(exe),
            },
            'installer': job.get('installer'),
            'analysis': job.get('analysis'),
            'matrix': job.get('matrix'),
            'tool': {
                'binary': binary,
                'analysisExit': job.get('analysisExit'),
                'recompileExit': job.get('recompileExit'),
            },
            'build': {
                'seconds': round(seconds, 2),
                'log': job.get('log', ''),
                'includeAssets': bool(job.get('includeAssets')),
            },
            'bundle': {
                **stage['bundle'],
                'artifacts': stage['artifacts'],
            },
            'assets': stage['assets'],
            'dlls': stage.get('dlls', []),
            'runtime': {
                'renderer': stage['bundle']['renderer'],
                'host': 'wlam-browser-host (browser Win32 host)',
            },
            'limitations': LIMITATIONS,
        }
        destination = self.target / stage['slug'] / 'bundle'
        (destination / 'report.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        (destination / 'report.md').write_text(report_markdown(report), encoding='utf-8')
        return report
