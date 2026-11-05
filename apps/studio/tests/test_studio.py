import http.client
import io
import json
from pathlib import Path
import stat
import sys
import tempfile
import threading
import time
import unittest
import zipfile
from http.server import ThreadingHTTPServer

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from server import Studio, handler, unpack


def archive(entries):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, 'w') as z:
        for name, contents in entries:
            z.writestr(name, contents)
    return buffer.getvalue()


class StudioTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        analyzer = self.root / 'analyzer'
        analyzer.write_text('#!' + sys.executable + '\nimport json\nprint(json.dumps(dict(kind="pe32", machine="I386", imports=0, libraries=[], compatibility={}, blocking=[])))\n')
        analyzer.chmod(0o755)
        self.adapter = self.root / 'adapter.py'
        self.adapter.write_text('''import pathlib, sys
source, assets, output = map(pathlib.Path, sys.argv[1:])
assert source.read_bytes() == b'MZfixture'
assert (assets / 'game/data/level.txt').read_text() == 'level one'
# A browser bundle fixture, not a PE recompiler.
(output / 'index.html').write_text('<h1>Adapter fixture</h1>')
(output / 'guest.wasm').write_bytes(bytes.fromhex('0061736d01000000'))
print('fixture bundle emitted')
''')
        self.studio = Studio(self.root / 'projects', str(analyzer), [sys.executable, str(self.adapter), '{input}', '{assets}', '{output}'])

    def tearDown(self):
        self.studio.pool.shutdown(wait=True)
        self.temp.cleanup()

    def upload(self):
        return self.studio.upload(archive([('game/game.exe', b'MZfixture'), ('game/data/level.txt', b'level one')]), 'Game.zip')

    def wait(self, ident):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            p = self.studio.get(ident)
            if p['state'] not in ('queued', 'analyzing', 'compiling'):
                return p
            time.sleep(.01)
        self.fail('Worker did not finish')

    def test_archive_rejects_traversal_absolute_windows_and_symlinks(self):
        for name in ['../escape.exe', '/absolute.exe', 'a/../../escape.exe', 'C:/windows.exe', 'a\\b.exe']:
            with self.subTest(name=name), self.assertRaises(ValueError):
                unpack(archive([(name, b'bad')]), self.root / 'extract')
        entry = zipfile.ZipInfo('link.exe')
        entry.external_attr = (stat.S_IFLNK | 0o777) << 16
        with self.assertRaises(ValueError):
            unpack(archive([(entry, b'/etc/passwd')]), self.root / 'extract')

    def test_rejects_missing_exe_and_cleans_failed_upload(self):
        with self.assertRaises(ValueError):
            self.studio.upload(archive([('readme.txt', 'hi')]), 'Bad.zip')
        self.assertEqual(list(self.studio.data.iterdir()), [])

    def test_build_adapter_receives_exe_assets_and_output(self):
        p = self.upload()
        self.studio.enqueue(p['id'], p['selected'], 'build')
        p = self.wait(p['id'])
        self.assertEqual(p['state'], 'ready', p)
        self.assertIn('fixture bundle emitted', p['log'])
        self.assertTrue((self.studio.folder(p['id']) / 'output/index.html').is_file())
        reopened = Studio(self.studio.data, self.studio.analyzer, None)
        self.assertEqual(reopened.get(p['id'])['state'], 'ready')
        reopened.pool.shutdown()

    def test_missing_adapter_is_explicit_failure_with_analysis(self):
        self.studio.compiler = None
        p = self.upload()
        self.studio.enqueue(p['id'], p['selected'], 'build')
        p = self.wait(p['id'])
        self.assertEqual(p['state'], 'failed')
        self.assertIn('not configured', p['error'])
        self.assertIsNotNone(p['analysis'])

    def test_adapter_success_without_runtime_is_failure(self):
        self.studio.compiler = [sys.executable, '-c', 'print("no runtime")']
        p = self.upload()
        self.studio.enqueue(p['id'], p['selected'], 'build')
        p = self.wait(p['id'])
        self.assertEqual(p['state'], 'failed')
        self.assertIn('index.html', p['error'])

    def test_blockers_stop_compilation_but_analysis_is_available(self):
        Path(self.studio.analyzer).write_text('#!' + sys.executable + '\nimport json\nprint(json.dumps({"blocking":[{"symbol":"Missing"}]}))\nraise SystemExit(2)\n')
        p = self.upload()
        self.studio.enqueue(p['id'], p['selected'], 'analyze')
        self.assertEqual(self.wait(p['id'])['state'], 'analyzed')
        self.studio.enqueue(p['id'], p['selected'], 'build')
        p = self.wait(p['id'])
        self.assertEqual(p['state'], 'failed')
        self.assertIn('Blocking imports', p['error'])
        self.assertFalse((self.studio.folder(p['id']) / 'output').exists())

    def test_restart_marks_interrupted_jobs_failed(self):
        p = self.upload()
        p['state'] = 'compiling'
        self.studio.save(p)
        reopened = Studio(self.studio.data, self.studio.analyzer, None)
        self.assertEqual(reopened.get(p['id'])['state'], 'failed')
        reopened.pool.shutdown()

    def test_selection_cannot_escape_archive(self):
        p = self.upload()
        with self.assertRaises(ValueError):
            self.studio.enqueue(p['id'], '../evil.exe', 'build')

    def test_http_upload_build_download_and_origin_separation(self):
        api_server = ThreadingHTTPServer(('127.0.0.1', 0), handler(self.studio))
        player = ThreadingHTTPServer(('127.0.0.1', 0), handler(self.studio, True))
        api_server.player_port = player.server_port
        for server in [api_server, player]:
            threading.Thread(target=server.serve_forever, daemon=True).start()

        def request(server, method, path, body=None, headers=None):
            conn = http.client.HTTPConnection('127.0.0.1', server.server_port)
            conn.request(method, path, body, headers or {})
            response = conn.getresponse()
            result = response.status, response.read(), dict(response.getheaders())
            conn.close()
            return result

        try:
            headers = {'Origin': f'http://127.0.0.1:{api_server.server_port}', 'Content-Type': 'application/zip'}
            data = archive([('game/game.exe', b'MZfixture'), ('game/data/level.txt', b'level one')])
            self.assertEqual(request(api_server, 'POST', '/api/projects', data)[0], 403)
            status, body, _ = request(api_server, 'POST', '/api/projects', data, headers)
            self.assertEqual(status, 201)
            p = json.loads(body)
            status, _, _ = request(api_server, 'POST', f'/api/projects/{p["id"]}/build', json.dumps({'selected':p['selected']}), headers)
            self.assertEqual(status, 202)
            self.assertEqual(self.wait(p['id'])['state'], 'ready')
            status, body, _ = request(api_server, 'GET', f'/api/projects/{p["id"]}/download')
            self.assertEqual(status, 200)
            self.assertEqual(set(zipfile.ZipFile(io.BytesIO(body)).namelist()), {'index.html', 'guest.wasm'})
            status, body, response_headers = request(player, 'GET', f'/{p["id"]}/guest.wasm')
            self.assertEqual(status, 200)
            self.assertEqual(response_headers['Content-Type'], 'application/wasm')
            self.assertEqual(response_headers['Cross-Origin-Embedder-Policy'], 'require-corp')
            self.assertEqual(request(player, 'GET', '/api/projects')[0], 404)
            self.assertEqual(request(api_server, 'GET', f'/{p["id"]}/source/game/game.exe')[0], 404)
            self.assertEqual(request(player, 'GET', f'/{p["id"]}/../project.json')[0], 404)
            self.assertEqual(request(api_server, 'GET', '/')[0], 200)
            self.assertEqual(request(api_server, 'GET', '/api/config', headers={'Host':'attacker.invalid'})[0], 403)
        finally:
            for server in [api_server, player]:
                server.shutdown()
                server.server_close()


if __name__ == '__main__':
    unittest.main()
