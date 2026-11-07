"""Document conversion and cache publication regressions."""

import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest

import crawl_docs


class GuideCacheTests(unittest.TestCase):
    def test_mixed_heading_emphasis_preserves_words(self):
        result = crawl_docs.extract_markdown(
            '<div class="markdown-body"><h4><strong>What</strong> '
            '<strong>is</strong> Agent OS</h4></div>', '/guide',
        )
        self.assertIn('#### What is Agent OS', result['content'])

    def test_html_code_preserves_heading_like_text(self):
        code = '# set **BOLD** flag\n\n\n\n- keep spacing\n'
        result = crawl_docs.extract_markdown(
            f'<div class="markdown-body"><pre><code>{code}</code></pre></div>',
            '/guide',
        )
        self.assertIn(code, result['content'])

    def test_long_tilde_and_unclosed_fences_are_untouched(self):
        for fence in ('```', '````', '~~~'):
            for closed in (True, False):
                with self.subTest(fence=fence, closed=closed):
                    code = fence + '\n# **keep**\n\n\n\n[keep  ]\n'
                    if closed:
                        code += fence + '\n'
                    self.assertEqual(crawl_docs.post_process_markdown(code), code)
        mixed = '#### **Before**\n```\n# **literal**\n```\n#### **After**\n'
        self.assertEqual(crawl_docs.post_process_markdown(mixed),
                         '#### Before\n```\n# **literal**\n```\n#### After\n')

    def test_heading_literal_double_asterisks_survive(self):
        result = crawl_docs.extract_markdown(
            '<div class="markdown-body"><h4>使用 **kwargs 参数</h4></div>',
            '/guide',
        )
        # markdownify escapes literal asterisks; the point is that the paired
        # emphasis unwrapping no longer deletes them as if they were markup.
        self.assertIn('#### 使用 \\*\\*kwargs 参数', result['content'])

    def test_cjk_split_bold_keeps_separator(self):
        # Pins the chosen behaviour: CJK runs separated in the source keep the
        # space rather than being joined into #3627's ideal form.
        result = crawl_docs.extract_markdown(
            '<div class="markdown-body"><h4><strong>什么</strong> '
            '<strong>是</strong> Agent OS</h4></div>', '/guide',
        )
        self.assertIn('#### 什么 是 Agent OS', result['content'])

    def test_empty_extraction_preserves_cache_and_records_failure(self):
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                # HTTP 200 soft-404: no markdown-body container at all.
                self.wfile.write(b'<h1>404 Not Found</h1>')

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        old_base, old_urls = crawl_docs.BASE_URL, crawl_docs.DOC_URLS
        try:
            crawl_docs.BASE_URL = f'http://127.0.0.1:{server.server_port}'
            crawl_docs.DOC_URLS = [('faq', '/soft-404')]
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                old = root / 'faq.md'
                old.write_text('# FAQ\n\nReal, useful FAQ content.', encoding='utf-8')
                summary = crawl_docs.crawl_all_docs(root)
                self.assertEqual(
                    old.read_text(encoding='utf-8'),
                    '# FAQ\n\nReal, useful FAQ content.',
                )
                self.assertEqual(summary['success_count'], 0)
                self.assertEqual(summary['failed_count'], 1)
                self.assertEqual(summary['results'][0]['status'], 'failed')
                self.assertFalse(list(root.glob('.*.tmp')))
        finally:
            crawl_docs.BASE_URL, crawl_docs.DOC_URLS = old_base, old_urls
            server.shutdown()
            server.server_close()
            thread.join()

    def test_http_error_preserves_cache_and_records_failure(self):
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == '/missing':
                    self.send_response(404)
                    body = b'<h1>Not found</h1>'
                else:
                    self.send_response(200)
                    body = b'<div class="markdown-body"><h2>Current guide</h2></div>'
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        old_base, old_urls = crawl_docs.BASE_URL, crawl_docs.DOC_URLS
        try:
            crawl_docs.BASE_URL = f'http://127.0.0.1:{server.server_port}'
            crawl_docs.DOC_URLS = [('faq', '/missing'), ('guide', '/ok')]
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                old = root / 'faq.md'
                old.write_text('Complete cached FAQ', encoding='utf-8')
                summary = crawl_docs.crawl_all_docs(root)
                self.assertEqual(old.read_text(), 'Complete cached FAQ')
                self.assertEqual(summary['failed_count'], 1)
                self.assertEqual(summary['success_count'], 1)
                self.assertIn('Current guide', (root / 'guide.md').read_text())
                self.assertFalse(list(root.glob('.*.tmp')))
        finally:
            crawl_docs.BASE_URL, crawl_docs.DOC_URLS = old_base, old_urls
            server.shutdown()
            server.server_close()
            thread.join()

    @unittest.skipUnless(os.name == 'posix', 'Requires a real OS file-size limit')
    def test_failed_write_keeps_existing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'faq.md'
            target.write_text('Original complete FAQ', encoding='utf-8')
            # Impose a real kernel write limit in a child, not a mocked write.
            program = '''
import errno, resource, signal, sys
import crawl_docs
signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
resource.setrlimit(resource.RLIMIT_FSIZE, (4096, 4096))
try:
    crawl_docs.save_markdown_file(dict(title='FAQ', full_url='/faq',
        last_modified='', content='x' * 65536), sys.argv[1])
except OSError as error:
    assert error.errno == errno.EFBIG, error
else:
    raise AssertionError('Expected a real file-size write failure')
'''
            child = subprocess.run([sys.executable, '-c', program, str(target)],
                                   cwd=Path(crawl_docs.__file__).parent,
                                   capture_output=True, text=True)
            self.assertEqual(child.returncode, 0, child.stdout + child.stderr)
            self.assertEqual(target.read_text(), 'Original complete FAQ')
            self.assertEqual(list(Path(directory).iterdir()), [target])

    def test_successful_replacement_is_complete(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'faq.md'
            target.write_text('Old FAQ', encoding='utf-8')
            target.chmod(0o644)
            expected = crawl_docs.save_markdown_file(
                dict(title='FAQ', full_url='/faq', last_modified='', content='New FAQ'),
                target,
            )
            self.assertEqual(target.read_text(), expected)
            self.assertEqual(list(Path(directory).iterdir()), [target])
            if os.name == 'posix':
                self.assertEqual(target.stat().st_mode & 0o777, 0o644)

    @unittest.skipUnless(os.name == 'posix', 'Requires symlink support')
    def test_relative_symlink_targets_are_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'targets').mkdir()
            for name in ('faq.md', 'crawl_summary.json'):
                target = root / 'targets' / name
                target.write_text('Old complete data')
                link = root / name
                link.symlink_to(Path('targets') / name)
                crawl_docs._atomic_write_text(link, 'Updated complete data')
                self.assertTrue(link.is_symlink())
                self.assertEqual(target.read_text(), 'Updated complete data')
                program = '''
import errno, resource, signal, sys
import crawl_docs
signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
resource.setrlimit(resource.RLIMIT_FSIZE, (4096, 4096))
try:
    crawl_docs._atomic_write_text(sys.argv[1], 'x' * 65536)
except OSError as error:
    assert error.errno == errno.EFBIG, error
else:
    raise AssertionError('Expected a real file-size write failure')
'''
                child = subprocess.run([sys.executable, '-c', program, str(link)],
                                       cwd=Path(crawl_docs.__file__).parent,
                                       capture_output=True, text=True)
                self.assertEqual(child.returncode, 0, child.stderr)
                self.assertTrue(link.is_symlink())
                self.assertEqual(target.read_text(), 'Updated complete data')
                self.assertFalse(list((root / 'targets').glob('.*.tmp')))

    @unittest.skipUnless(os.name == 'posix', 'Requires a real OS file-size limit')
    def test_failed_publication_is_rejected_by_refresh_caller(self):
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'<div class="markdown-body">' + b'x' * 65536 + b'</div>')

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'scripts').mkdir()
                (root / 'venv' / 'bin').mkdir(parents=True)
                (root / 'venv' / 'bin' / 'python').symlink_to(sys.executable)
                # A local entrypoint configures the real crawler for the real
                # HTTP fixture; no network or filesystem operation is mocked.
                (root / 'scripts' / 'crawl_docs.py').write_text(
                    'import sys\n'
                    f'sys.path.insert(0, {str(Path(crawl_docs.__file__).parent)!r})\n'
                    'import crawl_docs\n'
                    f'crawl_docs.BASE_URL = "http://127.0.0.1:{server.server_port}"\n'
                    'crawl_docs.DOC_URLS = [("faq", "/faq")]\n'
                    'sys.exit(crawl_docs.main())\n', encoding='utf-8',
                )
                program = '''
import resource, signal, sys
from pathlib import Path
import check_docs
root = Path(sys.argv[1])
check_docs.VENV_DIR = root / 'venv'
check_docs.SCRIPT_DIR = root / 'scripts'
signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
resource.setrlimit(resource.RLIMIT_FSIZE, (8192, 8192))
assert check_docs.run_crawl(root / 'cache') is False
'''
                child = subprocess.run([sys.executable, '-c', program, str(root)],
                                       cwd=Path(crawl_docs.__file__).parent,
                                       env={**os.environ, 'PYTHONDONTWRITEBYTECODE': '1'},
                                       capture_output=True, text=True)
                self.assertEqual(child.returncode, 0, child.stdout + child.stderr)
                summary = json.loads((root / 'cache' / 'crawl_summary.json').read_text())
                self.assertEqual(summary['failed_count'], 1)
                self.assertEqual(summary['success_count'], 0)
                self.assertFalse((root / 'cache' / 'faq.md').exists())
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    unittest.main()
