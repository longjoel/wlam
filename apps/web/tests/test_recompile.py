"""Tests for the desktop recompile service and its HTTP routes.

Run from the repository root:

    python3 -m unittest discover -s apps/web/tests -p 'test_*.py' -v
"""
import http.client
import importlib.util
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

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts'))
from web_recompile import RecompileService, choose_executable, extract_archive  # noqa: E402

spec = importlib.util.spec_from_file_location('serve_web', ROOT / 'scripts' / 'serve-web.py')
serve_web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(serve_web)
Handler = serve_web.Handler


def archive(entries):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, 'w') as package:
        for name, contents in entries:
            package.writestr(name, contents)
    return buffer.getvalue()


def fake_wlam(directory, blocking=False, fail=False):
    path = Path(directory) / 'wlam'
    analysis = {
        'kind': 'pe32',
        'machine': 'I386',
        'image_base': '0x400000',
        'entry_point': '0x1000',
        'subsystem': 2,
        'imports': 2,
        'exports': 0,
        'libraries': [{'name': 'USER32.dll', 'symbols': 1}, {'name': 'KERNEL32.dll', 'symbols': 1}],
        'compatibility': {'native': 1, 'emulated': 1},
        'blocking': [{'symbol': 'Missing', 'library': 'USER32.dll', 'state': 'unknown', 'reason': 'user32'}] if blocking else [],
    }
    manifest = {'version': 1, 'module': 'guest.wasm', 'image': 'image.bin',
                'imports': [{'library': 'USER32.dll', 'name': 'CreateWindowExA'}]}
    path.write_text(
        '#!' + sys.executable + '\n'
        'import json, pathlib, sys\n'
        'pathlib.Path("wlam-arguments.txt").open("a").write(json.dumps(sys.argv[1:]) + "\\n")\n'
        'command = sys.argv[1]\n'
        'if command == "analyze":\n'
        '    print(json.dumps(' + repr(analysis) + '))\n'
        '    print("analysis notes", file=sys.stderr)\n'
        '    if "--matrix" in sys.argv:\n'
        '        pathlib.Path(sys.argv[sys.argv.index("--matrix") + 1]).write_text("# Compatibility matrix: fixture\\n\\n| Symbol | State |\\n| --- | --- |\\n| CreateWindowExA | native |\\n")\n'
        f'    raise SystemExit({2 if blocking else 0})\n'
        'if command == "recompile":\n'
        + ('    print("recompile failed", file=sys.stderr)\n    raise SystemExit(1)\n' if fail else
           '    output = pathlib.Path(sys.argv[sys.argv.index("-o") + 1])\n'
           '    output.mkdir()\n'
           '    (output / "guest.wasm").write_bytes(b"\\x00asm\\x01\\x00\\x00\\x00")\n'
           '    (output / "image.bin").write_bytes(b"image")\n'
           '    (output / "manifest.json").write_text(' + repr(json.dumps(manifest)) + ')\n'
           '    print("recompiled fixture")\n'
           '    raise SystemExit(0)\n')
        + 'raise SystemExit(9)\n'
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


class RecompileServiceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.wlam = fake_wlam(self.root)
        self.service = RecompileService(self.root, self.wlam)

    def tearDown(self):
        self.service.close()
        self.temp.cleanup()

    def upload(self):
        return self.service.upload(
            archive([
                ('game/game.exe', b'MZfixture'),
                ('game/data/level.txt', b'level one'),
                ('game/readme.txt', b'hello'),
            ]),
            'My Game.zip')

    def wait(self, ident, states=('analyzed', 'ready', 'failed')):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            job = self.service.get(ident)
            if job['state'] in states:
                return job
            time.sleep(.01)
        self.fail(f'job stuck in {self.service.get(ident)["state"]}')

    def test_archive_rejects_traversal_absolute_windows_and_symlinks(self):
        for name in ['../escape.exe', '/absolute.exe', 'a/../../escape.exe', 'C:/windows.exe', 'a\\b.exe']:
            with self.subTest(name=name), self.assertRaises(ValueError):
                extract_archive(archive([(name, b'bad')]), self.root / 'extract')
        entry = zipfile.ZipInfo('link.exe')
        entry.external_attr = (stat.S_IFLNK | 0o777) << 16
        with self.assertRaises(ValueError):
            extract_archive(archive([(entry, b'/etc/passwd')]), self.root / 'extract')

    def test_upload_finds_executables_and_cleans_failed_archives(self):
        job = self.upload()
        self.assertEqual(job['state'], 'review')
        self.assertEqual(job['executables'], ['game/game.exe'])
        self.assertEqual(job['selected'], 'game/game.exe')
        self.assertEqual(job['name'], 'My Game')
        with self.assertRaises(ValueError):
            self.service.upload(archive([('readme.txt', b'hi')]), 'Bad.zip')
        self.assertEqual(len(list(self.service.work_root.iterdir())), 1)

    def test_archive_name_prefers_matching_executable(self):
        executables = ['a/launcher.exe', 'a/game.exe']
        self.assertEqual(choose_executable(executables, 'Game.zip'), 'a/game.exe')

    def test_analyze_then_build_publishes_bundle_assets_and_reports(self):
        job = self.upload()
        self.service.analyze(job['id'], job['selected'], True)
        job = self.wait(job['id'], ('analyzed', 'failed'))
        self.assertEqual(job['state'], 'analyzed', job['error'])
        self.assertEqual(job['analysis']['imports'], 2)
        self.assertIn('analysis notes', job['log'])

        self.service.build(job['id'])
        job = self.wait(job['id'])
        self.assertEqual(job['state'], 'ready', job['error'])
        bundle = self.root / 'target' / job['bundle']['id'] / 'bundle'
        self.assertEqual(job['bundle']['id'], 'my-game')
        self.assertEqual(job['bundle']['manifest'], '/target/my-game/bundle/manifest.json')
        for name in ('guest.wasm', 'image.bin', 'manifest.json', 'report.json', 'report.md',
                     'compatibility.md'):
            self.assertTrue((bundle / name).is_file(), name)
        manifest = json.loads((bundle / 'manifest.json').read_text())
        self.assertEqual(manifest['name'], 'My Game')
        self.assertEqual({asset['path'] for asset in manifest['files']},
                         {'C:\\game\\data\\level.txt', 'C:\\game\\readme.txt'})
        self.assertTrue((bundle / 'files' / 'game' / 'data' / 'level.txt').is_file())
        report = json.loads((bundle / 'report.json').read_text())
        self.assertEqual(report['kind'], 'wlam-recompile-report')
        self.assertEqual(report['source']['executable'], 'game/game.exe')
        self.assertEqual(report['analysis']['compatibility'], {'native': 1, 'emulated': 1})
        self.assertIn('# Compatibility matrix: fixture', report['matrix'])
        self.assertEqual(report['bundle']['matrix'], '/target/my-game/bundle/compatibility.md')
        self.assertEqual(report['bundle']['renderer'], 'cpu')
        self.assertEqual(len(report['bundle']['artifacts']), 6)
        self.assertEqual(report['assets'][0]['path'].startswith('C:\\game\\'), True)
        markdown = (bundle / 'report.md').read_text()
        self.assertIn('# Recompile report: My Game', markdown)
        self.assertIn('## Import compatibility matrix', markdown)

    def test_second_build_with_same_name_gets_a_unique_desktop_id(self):
        for _ in range(2):
            job = self.upload()
            self.service.analyze(job['id'], job['selected'])
            self.wait(job['id'], ('analyzed', 'failed'))
            self.service.build(job['id'])
            job = self.wait(job['id'])
            self.assertEqual(job['state'], 'ready', job['error'])
        self.assertEqual(sorted(path.name for path in (self.root / 'target').iterdir() if path.is_dir()),
                         ['.recompile', 'my-game', 'my-game-2'])

    def test_supplied_dll_paths_reach_analyzer_and_compiler(self):
        job = self.service.upload(
            archive([
                ('game/game.exe', b'MZfixture'),
                ('game/mfc30.dll', b'MZfixtureDll'),
            ]),
            'Game.zip')
        self.assertEqual(job['dlls'], ['game/mfc30.dll'])
        self.assertTrue(job['includeDlls'])
        self.service.analyze(job['id'], job['selected'], True, True)
        job = self.wait(job['id'], ('analyzed', 'failed'))
        self.assertEqual(job['state'], 'analyzed', job['error'])
        self.service.build(job['id'])
        job = self.wait(job['id'])
        self.assertEqual(job['state'], 'ready', job['error'])
        lines = [json.loads(line) for line in
                 (self.service.work_root / job['id'] / 'wlam-arguments.txt').read_text().splitlines()]
        dll = str(self.service.work_root / job['id'] / 'source' / 'game' / 'mfc30.dll')
        analyze = next(args for args in lines if args[0] == 'analyze')
        recompile = next(args for args in lines if args[0] == 'recompile')
        for args in (analyze, recompile):
            self.assertIn('--dll', args)
            self.assertEqual(args[args.index('--dll') + 1], dll)

    def test_blocking_imports_stop_recompiling(self):
        self.service.wlam = fake_wlam(self.root, blocking=True)
        job = self.upload()
        self.service.analyze(job['id'], job['selected'])
        job = self.wait(job['id'], ('analyzed', 'failed'))
        self.assertEqual(job['state'], 'analyzed')
        self.assertTrue(job['analysis']['blocking'])
        with self.assertRaises(ValueError):
            self.service.build(job['id'])
        self.assertFalse((self.root / 'target' / 'my-game').exists())

    def test_recompile_failure_leaves_no_bundle(self):
        self.service.wlam = fake_wlam(self.root, fail=True)
        job = self.upload()
        self.service.analyze(job['id'], job['selected'])
        self.wait(job['id'], ('analyzed', 'failed'))
        self.service.build(job['id'])
        job = self.wait(job['id'])
        self.assertEqual(job['state'], 'failed')
        self.assertIn('status 1', job['error'])
        self.assertFalse((self.root / 'target' / 'my-game').exists())

    def test_selection_cannot_escape_archive(self):
        job = self.upload()
        with self.assertRaises(ValueError):
            self.service.analyze(job['id'], '../evil.exe')


class RecompileHTTPTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.service = RecompileService(self.root, fake_wlam(self.root))
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.service = self.service
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.service.close()
        self.temp.cleanup()

    def request(self, method, path, body=None, headers=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_port)
        connection.request(method, path, body, headers or {})
        response = connection.getresponse()
        result = response.status, response.read()
        connection.close()
        return result

    def test_http_upload_analyze_build_and_config(self):
        status, body = self.request('GET', '/apps/web/recompile/config')
        self.assertEqual(status, 200)
        self.assertTrue(json.loads(body)['available'])
        data = archive([('game/game.exe', b'MZfixture'), ('game/data/level.txt', b'level one')])
        self.assertEqual(self.request('POST', '/apps/web/recompile', data)[0], 403)
        origin = {'Origin': f'http://127.0.0.1:{self.server.server_port}'}
        status, body = self.request('POST', '/apps/web/recompile', data,
                                    {**origin, 'Content-Type': 'application/zip', 'X-Filename': 'Game.zip'})
        self.assertEqual(status, 201)
        job = json.loads(body)
        status, _ = self.request('POST', f'/apps/web/recompile/{job["id"]}/analyze',
                                 json.dumps({'selected': 'game/game.exe'}), {**origin, 'Content-Type': 'application/json'})
        self.assertEqual(status, 202)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            status, body = self.request('GET', f'/apps/web/recompile/{job["id"]}')
            job = json.loads(body)
            if job['state'] in ('analyzed', 'failed'):
                break
            time.sleep(.01)
        self.assertEqual(job['state'], 'analyzed', job['error'])
        status, _ = self.request('POST', f'/apps/web/recompile/{job["id"]}/build', b'{}',
                                 {**origin, 'Content-Type': 'application/json'})
        self.assertEqual(status, 202)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            status, body = self.request('GET', f'/apps/web/recompile/{job["id"]}')
            job = json.loads(body)
            if job['state'] in ('ready', 'failed'):
                break
            time.sleep(.01)
        self.assertEqual(job['state'], 'ready', job['error'])
        status, body = self.request('GET', f'/apps/web/recompile/{job["id"]}/report')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)['kind'], 'wlam-recompile-report')
        self.assertEqual(self.request('GET', '/apps/web/recompile/missing')[0], 404)
        status, _ = self.request('GET', '/apps/web/recompile/config', headers={'Host': 'attacker.invalid'})
        self.assertEqual(status, 403)


if __name__ == '__main__':
    unittest.main()
