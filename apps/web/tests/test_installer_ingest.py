"""Tests for the installer/archive ingest front-end.

Run from the repository root:

    python3 -m unittest discover -s apps/web/tests -p 'test_*.py' -v

These cover format detection, ZIP/SFX-ZIP extraction, path-traversal and
symlink rejection, size/entry limits, and the "needs run" fallback. CAB and
MSI extraction depend on optional system tools, so those cases only assert
detection and graceful failure unless the tools exist.
"""
import io
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts'))
import installer_ingest as ingest  # noqa: E402

MAX_EXPANDED = 10 * 1024 * 1024
MAX_ENTRIES = 100


def make_zip(entries):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, 'w') as archive:
        for name, contents in entries:
            archive.writestr(name, contents)
    return buffer.getvalue()


def make_sfx_zip(entries):
    """An MZ stub with a ZIP appended, as most self-extracting archives are."""
    return b'MZ' + b'\x90' * 256 + make_zip(entries)


def make_pe(payload=b''):
    """A minimal PE header plus optional trailing bytes."""
    stub = bytearray(b'MZ' + b'\x00' * 0x3e)
    stub[0x3c:0x40] = (0x40).to_bytes(4, 'little')
    stub += b'PE\x00\x00'
    return bytes(stub) + payload


def make_ne():
    stub = bytearray(b'MZ' + b'\x00' * 0x3e)
    stub[0x3c:0x40] = (0x40).to_bytes(4, 'little')
    stub += b'NE'
    return bytes(stub)


def make_cab():
    return b'MSCF' + b'\x00' * 64


def make_msi():
    return bytes.fromhex('d0cf11e0a1b11ae1') + b'\x00' * 64


class DetectFormatTests(unittest.TestCase):
    def detect(self, data, name='upload.bin'):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / name
            path.write_bytes(data)
            return ingest.detect_format(path)

    def test_zip(self):
        info = self.detect(make_zip([('a.txt', 'hi')]))
        self.assertEqual(info['format'], 'zip')

    def test_sfx_zip(self):
        info = self.detect(make_sfx_zip([('setup.exe', 'MZ'), ('a.txt', 'hi')]))
        self.assertEqual(info['format'], 'sfx-zip')
        self.assertEqual(info['machine'], 'pe')

    def test_cab_magic(self):
        info = self.detect(make_cab())
        self.assertEqual(info['format'], 'cab')

    def test_msi_magic(self):
        info = self.detect(make_msi())
        self.assertEqual(info['format'], 'msi')

    def test_pe_image(self):
        info = self.detect(make_pe())
        self.assertEqual(info['format'], 'pe')
        self.assertEqual(info['machine'], 'pe')

    def test_ne_image(self):
        info = self.detect(make_ne())
        self.assertEqual(info['format'], 'ne16')
        self.assertEqual(info['machine'], 'ne')

    def test_unknown(self):
        info = self.detect(b'not an installer at all')
        self.assertEqual(info['format'], 'unknown')

    def test_installer_brand(self):
        data = make_pe(b'... Inno Setup ...')
        info = self.detect(data)
        self.assertEqual(info['brand'], 'inno')


class ZipIngestTests(unittest.TestCase):
    def ingest_data(self, data, **limits):
        options = {'max_expanded': MAX_EXPANDED, 'max_entries': MAX_ENTRIES}
        options.update(limits)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name) / 'upload.bin'
        path.write_bytes(data)
        destination = Path(temporary.name) / 'out'
        return ingest.ingest(path, destination, **options), destination

    def test_plain_zip(self):
        result, destination = self.ingest_data(make_zip([('readme.txt', 'hi')]))
        self.assertEqual(result.format, 'zip')
        self.assertEqual(result.tool, 'zipfile')
        self.assertFalse(result.needs_run)
        self.assertEqual(result.payload_files, ['readme.txt'])
        self.assertTrue((destination / 'readme.txt').is_file())

    def test_sfx_zip_payload(self):
        data = make_sfx_zip([('setup.exe', b'MZ' + b'\x00' * 32), ('game.exe', b'MZ' + b'\x00' * 32)])
        result, destination = self.ingest_data(data)
        self.assertEqual(result.format, 'sfx-zip')
        self.assertEqual(result.tool, 'zipfile')
        # setup.exe is treated as an installer stub, game.exe as payload.
        self.assertIn('setup.exe', result.installer_files)
        self.assertIn('game.exe', result.payload_files)
        self.assertTrue((destination / 'game.exe').is_file())

    def test_rejects_parent_traversal(self):
        for name in ('../evil.txt', '/absolute.txt', 'a\\b.txt'):
            with self.assertRaises(ingest.IngestError):
                self.ingest_data(make_zip([(name, 'x')]))

    def test_rejects_symlink(self):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, 'w') as archive:
            info = zipfile.ZipInfo('link')
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            archive.writestr(info, '/etc/passwd')
        with self.assertRaises(ingest.IngestError):
            self.ingest_data(buffer.getvalue())

    def test_enforces_entry_limit(self):
        data = make_zip([('a.txt', 'a'), ('b.txt', 'b')])
        with self.assertRaises(ingest.IngestError):
            self.ingest_data(data, max_entries=1)

    def test_enforces_expanded_limit(self):
        data = make_zip([('big.txt', 'x' * 4096)])
        with self.assertRaises(ingest.IngestError):
            self.ingest_data(data, max_expanded=128)


class InstallerFallbackTests(unittest.TestCase):
    def ingest_data(self, data):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name) / 'setup.exe'
        path.write_bytes(data)
        destination = Path(temporary.name) / 'out'
        return ingest.ingest(path, destination, max_expanded=MAX_EXPANDED,
                             max_entries=MAX_ENTRIES)

    def test_pe_without_payload_needs_run(self):
        result = self.ingest_data(make_pe(b'\x00' * 4096))
        self.assertTrue(result.needs_run)
        self.assertEqual(result.payload_files, [])
        self.assertTrue(result.warnings)

    def test_pe_with_embedded_zip(self):
        result = self.ingest_data(make_pe(make_zip([('data.txt', 'hi')])))
        self.assertFalse(result.needs_run)
        self.assertIn('data.txt', result.payload_files)


class TreeValidationTests(unittest.TestCase):
    def test_rejects_symlink_in_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'ok.txt').write_text('hi')
            os.symlink('/etc/passwd', root / 'link')
            with self.assertRaises(ingest.IngestError):
                ingest._validate_tree(root, MAX_EXPANDED, MAX_ENTRIES)

    def test_accepts_plain_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'ok.txt').write_text('hi')
            entries, total = ingest._validate_tree(root, MAX_EXPANDED, MAX_ENTRIES)
            self.assertEqual(entries, 1)
            self.assertEqual(total, 2)


class ToolInventoryTests(unittest.TestCase):
    def test_available_tools_has_known_names(self):
        tools = ingest.available_tools()
        for name in ('cabextract', '7z', 'unshield', 'innoextract', 'msiextract'):
            self.assertIn(name, tools)


class CabDetectionTests(unittest.TestCase):
    def test_cab_ingest_reports_failure_without_tools(self):
        if ingest._find_tool('cabextract') or ingest._find_tool('7z', '7za'):
            self.skipTest('CAB extractor is installed; failure path not exercised')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'upload.cab'
            path.write_bytes(make_cab())
            with self.assertRaises(ingest.IngestError):
                ingest.ingest(path, Path(directory) / 'out',
                              max_expanded=MAX_EXPANDED, max_entries=MAX_ENTRIES)


if __name__ == '__main__':
    unittest.main()
