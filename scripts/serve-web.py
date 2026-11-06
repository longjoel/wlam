#!/usr/bin/env python3
"""Serve the repository locally with the guest input headers and recompile API."""
from http.server import ThreadingHTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
from urllib.parse import unquote, urlsplit
import argparse
import json
import os
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(Path(__file__).resolve().parent))
from web_recompile import MAX_UPLOAD, RecompileService  # noqa: E402  (local module)

RECOMPILE_API = '/apps/web/recompile'
PREFERRED_ORDER = ['interactive', 'tolympic', 'hover', 'gdi', 'opengl-fixture']
PREFERRED_NAMES = {
    'interactive': 'Interactive Fixture',
    'tolympic': 'TOLYMPIC',
    'hover': 'Hover',
    'gdi': 'GDI Fixture',
    'opengl-fixture': 'OpenGL Fixture',
}
SPECIAL_WORDS = {
    'gdi': 'GDI', 'opengl': 'OpenGL', 'gl': 'GL', 'win16': 'Win16',
    'mfc': 'MFC', 'api': 'API', 'pe': 'PE', 'dll': 'DLL', 'x87': 'x87',
}


def display_name(slug):
    if slug in PREFERRED_NAMES:
        return PREFERRED_NAMES[slug]
    words = [word for word in re.split(r'[-_]+', slug) if word]
    return ' '.join(SPECIAL_WORDS.get(word.lower(), word[:1].upper() + word[1:]) for word in words)


def bundle_catalog():
    """List bundles built into target/<name>/bundle/. Temporary build dirs are skipped."""
    target = ROOT / 'target'
    bundles = []
    if not target.is_dir():
        return bundles
    for manifest_path in target.glob('*/bundle/manifest.json'):
        slug = manifest_path.parent.parent.name
        if not slug or slug.startswith('.') or '.' in slug:
            continue
        try:
            manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
        except (OSError, ValueError):
            continue
        if manifest.get('version') != 1:
            continue
        bundle_dir = manifest_path.parent
        if not all(isinstance(manifest.get(key), str) and (bundle_dir / manifest[key]).is_file()
                   for key in ('module', 'image')):
            continue
        imports = manifest.get('imports')
        libraries = {
            str(item.get('library', '')).lower()
            for item in imports if isinstance(item, dict)
        } if isinstance(imports, list) else set()
        try:
            built = int(manifest_path.stat().st_mtime)
        except OSError:
            built = 0
        name = manifest.get('name')
        if not isinstance(name, str) or not name.strip():
            name = display_name(slug)
        report_path = bundle_dir / 'report.json'
        entry = {
            'id': slug,
            'name': name.strip()[:120],
            'manifest': '/' + manifest_path.relative_to(ROOT).as_posix(),
            'renderer': 'webgl' if 'opengl32.dll' in libraries else 'cpu',
            'built': built,
        }
        if report_path.is_file():
            entry['report'] = '/' + report_path.relative_to(ROOT).as_posix()
        bundles.append(entry)
    order = {slug: index for index, slug in enumerate(PREFERRED_ORDER)}
    bundles.sort(key=lambda bundle: (order.get(bundle['id'], len(order)), bundle['name'].lower()))
    return bundles


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def end_headers(self):
        self.send_header('Cross-Origin-Opener-Policy', 'same-origin')
        self.send_header('Cross-Origin-Embedder-Policy', 'require-corp')
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        super().end_headers()

    def send_json(self, code, payload):
        body = json.dumps(payload).encode('utf-8')
        self.send_response(code)
        self.send_header('Content-Type', 'application/json; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def service(self):
        return getattr(self.server, 'service', None)

    def local_request(self, write=False):
        """Reject DNS rebinding and cross-site writes on the local dev server."""
        host = self.headers.get('Host')
        port = self.server.server_address[1]
        if host not in (f'127.0.0.1:{port}', f'localhost:{port}'):
            return False
        if write and self.headers.get('Origin') != f'http://{host}':
            return False
        return True

    def read_body(self, limit):
        try:
            length = int(self.headers.get('Content-Length', '0'))
        except ValueError:
            raise ValueError('Invalid Content-Length')
        if not 0 < length <= limit:
            raise ValueError(f'Upload limit is {limit // (1024 * 1024)} MiB')
        return self.rfile.read(length)

    def job_id(self, path):
        parts = path.strip('/').split('/')
        if len(parts) < 4 or parts[:3] != ['apps', 'web', 'recompile']:
            return None, None
        return parts[3], parts

    def do_GET(self):
        path = urlsplit(self.path).path
        # Extracted uploads are compiler input, not browser artifacts.
        if path == '/target/.recompile' or path.startswith('/target/.recompile/'):
            return self.send_json(404, {'error': 'Not found'})
        if path == '/apps/web/bundles.json':
            return self.send_json(200, {'bundles': bundle_catalog()})
        if path.startswith(RECOMPILE_API):
            if not self.local_request():
                return self.send_json(403, {'error': 'Invalid host'})
            service = self.service()
            if service is None:
                return self.send_json(404, {'error': 'Recompile service is unavailable'})
            if path == RECOMPILE_API + '/config':
                return self.send_json(200, service.config())
            ident, parts = self.job_id(path)
            try:
                job = service.get(ident)
                if len(parts) == 5 and parts[4] == 'report':
                    if not job.get('report'):
                        raise KeyError('Report is not ready')
                    body = json.dumps(job['report'], indent=2).encode('utf-8')
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json; charset=utf-8')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                if len(parts) != 4:
                    raise KeyError('Not found')
                return self.send_json(200, service.public(job))
            except KeyError as error:
                return self.send_json(404, {'error': str(error).strip("'")})
        super().do_GET()

    def do_POST(self):
        path = urlsplit(self.path).path
        if not path.startswith(RECOMPILE_API):
            return self.send_json(404, {'error': 'Not found'})
        if not self.local_request(write=True):
            return self.send_json(403, {'error': 'Same-origin request required'})
        service = self.service()
        if service is None:
            return self.send_json(404, {'error': 'Recompile service is unavailable'})
        try:
            if path == RECOMPILE_API:
                body = self.read_body(MAX_UPLOAD)
                name = unquote(self.headers.get('X-Filename', 'Untitled.zip'))
                mode = unquote(self.headers.get('X-Mode', 'auto'))
                return self.send_json(201, service.upload(body, name, mode))
            ident, parts = self.job_id(path)
            if len(parts) != 5 or parts[4] not in ('analyze', 'build'):
                return self.send_json(404, {'error': 'Not found'})
            payload = json.loads(self.read_body(1024 * 1024) or b'{}')
            if parts[4] == 'analyze':
                job = service.analyze(ident, payload.get('selected'),
                                      payload.get('includeAssets', True),
                                      payload.get('includeDlls', True))
            else:
                job = service.build(ident)
            return self.send_json(202, job)
        except KeyError as error:
            return self.send_json(404, {'error': str(error).strip("'")})
        except (ValueError, OSError) as error:
            return self.send_json(400, {'error': str(error)})

    def do_DELETE(self):
        path = urlsplit(self.path).path
        if not path.startswith(RECOMPILE_API):
            return self.send_json(404, {'error': 'Not found'})
        if not self.local_request(write=True):
            return self.send_json(403, {'error': 'Same-origin request required'})
        service = self.service()
        if service is None:
            return self.send_json(404, {'error': 'Recompile service is unavailable'})
        try:
            ident, parts = self.job_id(path)
            if len(parts) != 4:
                raise KeyError('Not found')
            service.delete(ident)
            return self.send_json(200, {'deleted': ident})
        except KeyError as error:
            return self.send_json(404, {'error': str(error).strip("'")})
        except (ValueError, OSError) as error:
            return self.send_json(400, {'error': str(error)})


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--port', type=int, default=8000)
    parser.add_argument('--wlam', default=os.environ.get('WLAM_BIN', str(ROOT / 'target/debug/wlam')),
                        help='wlam binary used for analysis and recompilation')
    args = parser.parse_args()
    service = RecompileService(ROOT, args.wlam)
    server = ThreadingHTTPServer(('127.0.0.1', args.port), Handler)
    server.service = service
    print(f'Guest runner: http://127.0.0.1:{args.port}/apps/web/', flush=True)
    if service.config()['available']:
        print(f'Recompile: drag a ZIP onto the desktop (binary: {service.wlam})', flush=True)
    else:
        print(f'Recompile disabled: wlam binary not found at {service.wlam}', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        service.close()
