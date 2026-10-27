"""Policy checks for platform builds, release gates and seed ownership."""
from pathlib import Path
import re
import os
import subprocess
import tempfile
import textwrap
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


def read(name):
    return (ROOT / name).read_text()


class PlatformWorkflowPolicy(unittest.TestCase):
    def test_littlefs_configuration_is_owned_by_core(self):
        self.assertFalse((ROOT / 'config/littlefs/little_fs_config.h').exists())
        self.assertNotIn('LITTLEFS_CONFIG', read('.cargo/config.toml'))
        binding = tomllib.loads(read('Cargo.toml'))['patch']['crates-io']['littlefs2-sys']
        self.assertEqual(binding['git'], 'https://github.com/xila-project/core')
        self.assertRegex(binding['rev'], r'^[0-9a-f]{40}$')

    def test_runtime_pr_has_no_publish_permissions_and_runs_smoke(self):
        workflow = read('.github/workflows/runtime.yml')
        pr = workflow.split('  runtime-pr:', 1)[1].split('  runtime:', 1)[0]
        self.assertNotIn('packages: write', pr)
        self.assertNotIn('docker/login-action', pr)
        for check in ('Check image user and entrypoint', 'Build runtime smoke-test guest',
                      'Test read-only root, host mounts, state persistence, and exit status',
                      'Exercise Xila CLI shell over stdin'):
            self.assertIn(check, pr)
        self.assertIn('cache-from: type=gha', pr)
        self.assertNotIn('cache-to:', pr)
        self.assertIn('load: true', pr)  # Needed to execute the smoke suite.

    def test_runtime_release_is_gated_and_preserves_image_name(self):
        workflow = read('.github/workflows/runtime.yml')
        release = workflow.split('  runtime:', 1)[1]
        self.assertIn('packages: write', release)
        self.assertIn("github.event_name == 'workflow_run'", release)
        self.assertIn('needs.primary.result == \'success\'', release)
        self.assertIn('needs.deep.result == \'success\'', release)
        self.assertIn('needs.deep.outputs.validated-sha', release)
        self.assertIn('ghcr.io/${{ github.repository_owner }}/xila-runtime', release)
        self.assertNotIn("github.event_name == 'pull_request' ||", release)

    def test_docker_consumes_separately_built_guests(self):
        dockerfile = read('Dockerfile.runtime')
        self.assertIn('COPY --from=applications / /applications/', dockerfile)
        self.assertIn('--applications-dir /applications', dockerfile)
        self.assertNotIn('--package xila-calculator', dockerfile)
        self.assertIn('USER 10001:10001', dockerfile)
        self.assertIn('ENTRYPOINT ["/usr/bin/xila-runtime"]', dockerfile)

    def test_browser_demo_profiles_and_images_are_reproducible(self):
        tasks = tomllib.loads(read('mise.ci.toml'))['tasks']
        script = tasks['pages:build']['run']
        for locale in ('en', 'fr', 'es', 'de', 'pt-BR', 'it', 'pl'):
            self.assertRegex(script, rf'\b{re.escape(locale)}\b')
        self.assertIn('cmp "$CARGO_TARGET_DIR/seed/wasm/seed.img"', script)
        self.assertIn('cmp "$CARGO_TARGET_DIR/seed/wasm/native.img"', script)
        self.assertIn('mise run pages:demo', script)
        demo = read('.github/workflows/demo.yml')
        self.assertIn('platform-demonstrations', demo)
        self.assertIn('reference-filesystem-images', demo)
        self.assertIn('XILA_FONT_CATALOG_ROOTS', demo)

    def test_demo_images_follow_each_locale_target(self):
        script = tomllib.loads(read('mise.ci.toml'))['tasks']['pages:build']['run']
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            cargo = bin_dir / 'cargo'
            cargo.write_text(textwrap.dedent("""\
                #!/usr/bin/env python3
                import os, pathlib, sys
                args = sys.argv[1:]
                output = pathlib.Path(args[args.index('--') + 1])
                apps = pathlib.Path(args[args.index('--applications-dir') + 1])
                locale = os.environ['EMBEDDED_I18N_LOCALE']
                assert (apps / 'calculator.wasm').read_text() == locale
                bundle = args[args.index('--bundle') + 1]
                with output.open('x') as image:
                    image.write(locale + ':' + bundle)
                """))
            cargo.chmod(0o755)
            mise = bin_dir / 'mise'
            mise.write_text(textwrap.dedent("""\
                #!/bin/sh
                set -eu
                case "$2" in
                  ci:build-applications)
                    mkdir -p "$CARGO_TARGET_DIR/wasm32-wasip1/release-wasm"
                    printf '%s' "$EMBEDDED_I18N_LOCALE" > "$CARGO_TARGET_DIR/wasm32-wasip1/release-wasm/calculator.wasm"
                    ;;
                  pages:demo)
                    test "$(cat target/seed/wasm/seed.img)" = "$EMBEDDED_I18N_LOCALE:wasm"
                    mkdir -p examples/wasm/dist
                    cp target/seed/wasm/seed.img examples/wasm/dist/seed.img
                    ;;
                  *) exit 1 ;;
                esac
                """))
            mise.chmod(0o755)
            env = os.environ | {'PATH': f"{bin_dir}:{os.environ['PATH']}", 'CARGO_TARGET_ROOT': str(root / 'locale targets')}
            result = subprocess.run(['sh', '-c', script], cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            for locale in ('en', 'fr', 'es', 'de', 'pt-BR', 'it', 'pl'):
                for bundle in ('wasm', 'native'):
                    self.assertEqual((root / 'target/reference-filesystem-images' / locale / f'{bundle}.img').read_text(), f'{locale}:{bundle}')
                self.assertEqual((root / 'pages/demonstrations/wasm' / locale / 'seed.img').read_text(), f'{locale}:wasm')

    def test_driver_and_seed_checks_are_required(self):
        ci = read('.github/workflows/ci.yml')
        for task in ('clippy:drivers', 'test:drivers', 'test:seed', 'firmware:test',
                     'ci:profiling', 'ci:profiling-runtime-tests', 'ci:profiling-native-tests',
                     'clippy:browser', 'check:browser'):
            self.assertIn(f'mise run {task}', ci)
        self.assertIn('drivers/wasm/tests/seed.test.mjs', ci)
        self.assertIn('examples/wasm/tests/disk_manager.test.mjs', ci)
        self.assertIn('needs: [quality, native, browser]', ci)

    def test_ci_cache_and_toolchain_wrapping_are_scoped(self):
        tasks = tomllib.loads(read('mise.ci.toml'))
        wrapper = tasks['wrappers']['cargo']
        self.assertEqual(wrapper['env']['MBX_CARGO_SHIM_MODE'], '1')
        self.assertNotIn('mr-boxington', str(tasks.get('task_templates', {})))
        ci = read('.github/workflows/ci.yml')
        self.assertIn('v1-platforms-host', ci)
        self.assertIn('v1-platforms-browser', ci)
        self.assertIn('github-cache-mode: target', ci)
        self.assertIn('github-cache-mode: objects', read('.github/workflows/demo.yml'))

    def test_cross_repository_workflow_uses_explicit_refs_and_checks_identity(self):
        workflow = read('.github/workflows/integration.yml')
        for token in ('core_ref:', 'applications_ref:', 'local_overrides.py',
                      'cargo metadata', 'cargo tree', 'package_seed_check.py'):
            self.assertIn(token, workflow)
        self.assertIn('assert not bad', workflow)
        self.assertIn('--profile release-wasm', workflow)
        self.assertIn('wasm32-wasip1/release-wasm', workflow)

    def test_profiles_are_explicit_and_browser_does_not_gain_weather(self):
        profiles = ROOT / 'tools/fs_image/profiles'
        native = (profiles / 'native.txt').read_text().splitlines()
        browser = (profiles / 'browser.txt').read_text().splitlines()
        self.assertEqual(native, ['calculator', 'clock', 'pixel_painter', 'synth', 'text_editor', 'weather'])
        self.assertEqual(browser, native[:-1])


if __name__ == '__main__':
    unittest.main()
