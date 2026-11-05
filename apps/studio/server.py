#!/usr/bin/env python3
"""Local WLAM Studio. Python 3.11+, no third-party dependencies."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import io
import json
import mimetypes
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import threading
import uuid
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[2]
STATIC = Path(__file__).with_name('static')
MAX_UPLOAD = 64 * 1024 * 1024
MAX_EXPANDED = 256 * 1024 * 1024


def safe_path(root, name):
    if '\\' in name or '\x00' in name:
        raise ValueError('Invalid path')
    parts = PurePosixPath(name)
    if parts.is_absolute() or '..' in parts.parts or ':' in name:
        raise ValueError('Invalid path')
    result = (root / name).resolve()
    if not result.is_relative_to(root.resolve()):
        raise ValueError('Path escapes project')
    return result


def unpack(data, destination):
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        if len(entries) > 10000 or sum(e.file_size for e in entries) > MAX_EXPANDED:
            raise ValueError('ZIP exceeds 10,000 files or 256 MiB expanded')
        seen = set()
        for entry in entries:
            path = safe_path(destination, entry.filename)
            if path in seen or stat.S_ISLNK(entry.external_attr >> 16):
                raise ValueError('Duplicate paths and symlinks are not supported')
            seen.add(path)
            if entry.flag_bits & 1:
                raise ValueError('Encrypted ZIPs are not supported')
            if entry.is_dir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(entry) as source, path.open('wb') as target:
                    shutil.copyfileobj(source, target)


class Studio:
    def __init__(self, data, analyzer, compiler):
        self.data = data.resolve()
        self.data.mkdir(parents=True, exist_ok=True)
        self.analyzer = analyzer
        self.compiler = compiler
        self.lock = threading.RLock()
        self.pool = ThreadPoolExecutor(max_workers=1)
        # A process restart cannot resume a native compiler invocation.
        for project in self.projects():
            if project['state'] in ('queued', 'analyzing', 'compiling'):
                project.update(state='failed', error='Studio restarted during this job. Retry the build.')
                self.save(project)

    def folder(self, ident):
        if not re.fullmatch('[0-9a-f]{32}', ident):
            raise ValueError('Invalid project ID')
        return self.data / ident

    def get(self, ident):
        with self.lock:
            return json.loads((self.folder(ident) / 'project.json').read_text())

    def save(self, project):
        with self.lock:
            folder = self.folder(project['id'])
            temporary = folder / 'project.tmp'
            temporary.write_text(json.dumps(project, indent=2))
            temporary.replace(folder / 'project.json')

    def projects(self):
        with self.lock:
            return [json.loads(p.read_text()) for p in sorted(self.data.glob('*/project.json'), key=lambda p: p.stat().st_mtime, reverse=True)]

    def upload(self, data, name):
        ident = uuid.uuid4().hex
        folder = self.folder(ident)
        source = folder / 'source'
        source.mkdir(parents=True)
        try:
            unpack(data, source)
            files = sorted(p.relative_to(source).as_posix() for p in source.rglob('*') if p.is_file())
            executables = [p for p in files if p.lower().endswith('.exe')]
            if not executables:
                raise ValueError('No Windows .exe found in this ZIP')
            project = dict(id=ident, name=name[:120], files=files, executables=executables,
                           selected=executables[0], state='uploaded', log='', analysis=None, error=None)
            self.save(project)
            return project
        except Exception:
            shutil.rmtree(folder)
            raise

    def enqueue(self, ident, selected, action):
        with self.lock:
            project = self.get(ident)
            if selected not in project['executables']:
                raise ValueError('Select an executable from this project')
            if project['state'] in ('queued', 'analyzing', 'compiling'):
                raise ValueError('This project already has an active job')
            if sum(p['state'] in ('queued', 'analyzing', 'compiling') for p in self.projects()) >= 8:
                raise ValueError('Build queue is full; wait for a job to finish')
            project.update(selected=selected, state='queued', log='', error=None, analysis=None)
            self.save(project)
            self.pool.submit(self.work, ident, action)
            return project

    def command(self, argv, cwd):
        # Output goes to disk so a noisy subprocess cannot exhaust server memory.
        with (cwd / 'command.log').open('w+b') as output:
            result = subprocess.run(argv, cwd=cwd, stdout=output, stderr=subprocess.STDOUT, timeout=300)
            output.seek(0)
            log = output.read(1024 * 1024).decode('utf-8', errors='replace')
        return result.returncode, log

    def work(self, ident, action):
        project = self.get(ident)
        folder = self.folder(ident)
        source = folder / 'source'
        exe = safe_path(source, project['selected'])
        try:
            project['state'] = 'analyzing'
            self.save(project)
            # stdout must remain separate from stderr: exit 2 is a valid analysis.
            result = subprocess.run([self.analyzer, 'analyze', str(exe), '--json', '--no-oracle'],
                                    cwd=folder, capture_output=True, timeout=60)
            project['log'] = result.stderr.decode(errors='replace')[:1024 * 1024]
            if result.returncode not in (0, 2):
                raise ValueError(project['log'] or 'Image analysis failed')
            project['analysis'] = json.loads(result.stdout)
            if action == 'analyze':
                project['state'] = 'analyzed'
            else:
                if project['analysis'].get('blocking'):
                    raise ValueError('Blocking imports found. See compatibility report before recompiling.')
                if not self.compiler:
                    raise ValueError('Compiler adapter is not configured. Set WLAM_STUDIO_COMPILER; see Studio README.')
                project['state'] = 'compiling'
                self.save(project)
                output = folder / 'output'
                if output.exists():
                    shutil.rmtree(output)
                output.mkdir()
                values = dict(input=str(exe), assets=str(source), output=str(output))
                argv = [arg.format_map(values) for arg in self.compiler]
                code, log = self.command(argv, folder)
                project['log'] += log
                if code:
                    raise ValueError(f'Compiler adapter exited with status {code}')
                for path in output.rglob('*'):
                    if path.is_symlink():
                        raise ValueError('Compiler output must not contain symbolic links')
                if not (output / 'index.html').is_file():
                    raise ValueError('Compiler adapter must produce output/index.html and its browser runtime')
                project['state'] = 'ready'
        except Exception as error:
            project.update(state='failed', error=str(error))
        self.save(project)


def handler(studio, player=False):
    class Handler(BaseHTTPRequestHandler):
        def respond(self, code, body, content_type='application/json'):
            if not isinstance(body, bytes):
                body = json.dumps(body).encode()
            self.send_response(code)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Cross-Origin-Opener-Policy', 'same-origin')
            self.send_header('Cross-Origin-Embedder-Policy', 'require-corp')
            self.send_header('Cross-Origin-Resource-Policy', 'cross-origin')
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            try:
                if self.headers.get('Host') not in (f'127.0.0.1:{self.server.server_port}', f'localhost:{self.server.server_port}'):
                    return self.respond(403, {'error': 'Invalid host'})
                path = unquote(urlsplit(self.path).path)
                if player:
                    parts = path.strip('/').split('/', 1)
                    project = studio.get(parts[0])
                    if project['state'] != 'ready':
                        raise FileNotFoundError('Build is not ready')
                    file = safe_path(studio.folder(parts[0]) / 'output', parts[1] if len(parts) > 1 and parts[1] else 'index.html')
                elif path == '/api/projects':
                    return self.respond(200, studio.projects())
                elif path == '/api/config':
                    return self.respond(200, dict(compiler=bool(studio.compiler), analyzer=Path(studio.analyzer).is_file(), player_port=self.server.player_port))
                elif path.startswith('/api/projects/'):
                    parts = path.strip('/').split('/')
                    project = studio.get(parts[2])
                    if len(parts) == 3:
                        return self.respond(200, project)
                    if len(parts) == 4 and parts[3] == 'download' and project['state'] == 'ready':
                        buffer = io.BytesIO()
                        with zipfile.ZipFile(buffer, 'w', zipfile.ZIP_DEFLATED) as archive:
                            root = studio.folder(project['id']) / 'output'
                            for item in root.rglob('*'):
                                if item.is_file():
                                    archive.write(item, item.relative_to(root))
                        return self.respond(200, buffer.getvalue(), 'application/zip')
                    raise FileNotFoundError()
                else:
                    file = safe_path(STATIC, path.lstrip('/') or 'index.html')
                self.respond(200, file.read_bytes(), mimetypes.guess_type(file.name)[0] or 'application/octet-stream')
            except (ValueError, FileNotFoundError, NotADirectoryError):
                self.respond(404, {'error': 'Not found'})

        def do_POST(self):
            try:
                if player:
                    return self.respond(405, {'error': 'Read only'})
                # Browser requests must originate from this studio, including on localhost.
                if self.headers.get('Origin') != f'http://{self.headers.get("Host")}':
                    return self.respond(403, {'error': 'Same-origin request required'})
                if self.headers.get('Host') not in (f'127.0.0.1:{self.server.server_port}', f'localhost:{self.server.server_port}'):
                    return self.respond(403, {'error': 'Invalid host'})
                length = int(self.headers.get('Content-Length', '0'))
                if not 0 < length <= MAX_UPLOAD:
                    return self.respond(413, {'error': 'Upload limit is 64 MiB'})
                body = self.rfile.read(length)
                path = urlsplit(self.path).path
                if path == '/api/projects':
                    project = studio.upload(body, unquote(self.headers.get('X-Filename', 'Untitled.zip')))
                    return self.respond(201, project)
                parts = path.strip('/').split('/')
                if len(parts) == 4 and parts[:2] == ['api', 'projects'] and parts[3] in ('analyze', 'build'):
                    payload = json.loads(body)
                    project = studio.enqueue(parts[2], payload['selected'], parts[3])
                    return self.respond(202, project)
                self.respond(404, {'error': 'Not found'})
            except (ValueError, KeyError, zipfile.BadZipFile, NotImplementedError, EOFError, RuntimeError, OSError) as error:
                self.respond(400, {'error': str(error)})
    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8787)
    parser.add_argument('--player-port', type=int, default=8788)
    parser.add_argument('--data', type=Path, default=Path(__file__).with_name('.data'))
    args = parser.parse_args()
    default_compiler = [sys.executable, str(Path(__file__).with_name('bundle.py')), '--input', '{input}', '--assets', '{assets}', '--output', '{output}']
    compiler = json.loads(os.environ['WLAM_STUDIO_COMPILER']) if 'WLAM_STUDIO_COMPILER' in os.environ else default_compiler
    if compiler is not None and (not isinstance(compiler, list) or not compiler or not all(isinstance(s, str) for s in compiler)):
        parser.error('WLAM_STUDIO_COMPILER must be a nonempty JSON array of command arguments')
    studio = Studio(args.data, os.environ.get('WLAM_BIN', str(ROOT / 'target/debug/wlam')), compiler)
    player_server = ThreadingHTTPServer(('127.0.0.1', args.player_port), handler(studio, True))
    server = ThreadingHTTPServer(('127.0.0.1', args.port), handler(studio))
    server.player_port = args.player_port
    threading.Thread(target=player_server.serve_forever, daemon=True).start()
    print(f'WLAM Studio: http://127.0.0.1:{args.port}', flush=True)
    print(f'Artifact origin: http://127.0.0.1:{args.player_port}', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        player_server.shutdown()
        player_server.server_close()
        studio.pool.shutdown(wait=True)


if __name__ == '__main__':
    main()
