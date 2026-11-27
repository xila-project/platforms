"""Policy checks for platform builds, release gates and seed ownership."""
from pathlib import Path
import re
import os
import subprocess
import tempfile
import textwrap
import tomllib
import unittest
import yaml

from locked_revision import locked_revision

ROOT = Path(__file__).resolve().parents[2]


def read(name):
    return (ROOT / name).read_text()


class PlatformWorkflowPolicy(unittest.TestCase):
    def test_littlefs_configuration_is_owned_by_core(self):
        self.assertFalse((ROOT / 'config/littlefs/little_fs_config.h').exists())
        self.assertNotIn('LITTLEFS_CONFIG', read('.cargo/config.toml'))
        binding = tomllib.loads(read('Cargo.toml'))['patch']['crates-io']['littlefs2-sys']
        self.assertEqual(binding['git'], 'https://github.com/xila-project/core')
        # Xila sources track main by branch; Cargo.lock holds the exact commit.
        self.assertEqual(binding.get('branch'), 'main')
        self.assertNotIn('rev', binding)

    def test_xila_git_sources_track_main(self):
        # Cargo.lock holds the exact commit; every Xila repository must use the same
        # `branch = "main"` reference so Cargo unifies Core, Applications and
        # package_manager instead of resolving duplicate copies.
        manifests = [tomllib.loads(read('Cargo.toml')), tomllib.loads(read('tools/artifacts/Cargo.toml'))]
        sections = []
        for cargo in manifests:
            sections.append(cargo.get('workspace', {}).get('dependencies', {}))
            sections.append(cargo.get('dependencies', {}))
            sections.extend(cargo.get('patch', {}).values())
        for dependencies in sections:
            for key, item in dependencies.items():
                if isinstance(item, dict) and item.get('git', '').startswith('https://github.com/xila-project/'):
                    self.assertEqual(item.get('branch'), 'main', key)
                    self.assertFalse({'rev', 'tag'} & item.keys(), key)
        locked = {}
        for package in tomllib.loads(read('Cargo.lock'))['package']:
            match = re.match(r'git\+https://github\.com/xila-project/([a-z_]+)\?(.*)#([0-9a-f]{40})$',
                             package.get('source', ''))
            if match:
                self.assertEqual(match.group(2), 'branch=main', package['name'])
                locked.setdefault(match.group(1), set()).add(match.group(3))
        for repository, revisions in locked.items():
            self.assertEqual(len(revisions), 1, repository)
        for repository in ('core', 'applications', 'package_manager'):
            self.assertEqual(locked_revision(repository), next(iter(locked[repository])))

    def test_runtime_pr_has_no_publish_permissions_and_runs_smoke(self):
        workflow = read('.github/workflows/runtime.yml')
        pr = workflow.split('  runtime-pr:', 1)[1].split('  runtime:', 1)[0]
        self.assertNotIn('packages: write', pr)
        self.assertNotIn('docker/login-action', pr)
        for check in ('Check image user and entrypoint', 'Build runtime smoke-test guest',
                      'Test read-only root, host mounts, state persistence, and exit status',
                      'Exercise Xila CLI shell over stdin'):
            self.assertIn(check, pr)
        # The shell smoke checks the home recorded in the seeded account, in
        # the PR build and in the release build alike.
        for home_check in (
            r"echo home=$HOME\ncd $HOME\npwd\n",
            "grep -Eq '(^|[[:space:]])home=/users/administrator[[:space:]]*$'",
            "grep -Eq '(^|[[:space:]])/users/administrator[[:space:]]*$'; then",
        ):
            self.assertEqual(workflow.count(home_check), 2, home_check)
            self.assertIn(home_check, pr)
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

    def test_runtime_core_deep_validation_matches_the_locked_core(self):
        # The release gate runs Core's deep validation at the Core revision the
        # firmware is locked to; the `uses:` SHA, `core_ref` and the
        # `validated-sha` comparison must all follow a Core relock.
        locked = {
            package['source'].rsplit('#', 1)[1]
            for package in tomllib.loads(read('Cargo.lock'))['package']
            if package.get('source', '').startswith('git+https://github.com/xila-project/core?')
        }
        self.assertEqual(len(locked), 1, locked)
        core = locked.pop()
        jobs = yaml.safe_load(read('.github/workflows/runtime.yml'))['jobs']
        workflow, _, uses_revision = jobs['deep']['uses'].partition('@')
        self.assertEqual(workflow, 'xila-project/core/.github/workflows/extended.yml')
        self.assertEqual(uses_revision, core, 'deep `uses:` revision')
        self.assertEqual(jobs['deep']['with']['core_ref'], core, 'deep `core_ref`')
        comparisons = re.findall(r"needs\.deep\.outputs\.validated-sha\s*==\s*'([0-9a-f]*)'",
                                 jobs['runtime']['if'])
        self.assertEqual(comparisons, [core], 'runtime `validated-sha` comparison')
        # No other copy of a Core revision may hide elsewhere in the workflow.
        workflow_text = read('.github/workflows/runtime.yml')
        self.assertEqual(
            set(re.findall(r'xila-project/core/[^@\s]*@([0-9a-f]{40})', workflow_text)), {core})

    def test_docker_consumes_separately_built_guests(self):
        dockerfile = read('Dockerfile.runtime')
        self.assertIn('COPY --from=applications / /bundles/', dockerfile)
        self.assertIn('--bundles-dir /bundles', dockerfile)
        runtime = read('.github/workflows/runtime.yml')
        self.assertEqual(runtime.count('XILA_BUNDLES_DIR=.application-artifacts mise run ci:bundle-applications'), 2)
        self.assertNotIn('--package xila-calculator', dockerfile)
        self.assertIn('USER 10001:10001', dockerfile)
        self.assertIn('ENTRYPOINT ["/usr/bin/xila-runtime"]', dockerfile)

    def test_docker_rust_version_matches_mise(self):
        # The runtime image pins its own toolchain; keep it equal to the mise
        # var so the one-version rule can't drift there unnoticed.
        pinned = tomllib.loads(read('mise.toml'))['vars']['rust_version']
        dockerfile = read('Dockerfile.runtime')
        self.assertIn(f'ARG RUST_VERSION={pinned}\n', dockerfile)

    def test_browser_demo_profiles_and_images_are_reproducible(self):
        tasks = tomllib.loads(read('mise.ci.toml'))['tasks']
        script = tasks['pages:build']['run']
        for locale in ('en', 'fr', 'es', 'de', 'pt-BR', 'it', 'pl', 'ja', 'zh-CN'):
            self.assertRegex(script, rf'\b{re.escape(locale)}\b')
        self.assertIn('cmp "$CARGO_TARGET_DIR/seed/wasm/seed.img"', script)
        self.assertIn('cmp "$CARGO_TARGET_DIR/seed/wasm/native.img"', script)
        self.assertIn('mise run pages:demo', script)

        demo = yaml.safe_load(read('.github/workflows/demo.yml'))
        build = demo['jobs']['build']
        locales_match = re.fullmatch(
            r"\$\{\{\s*github\.event_name == 'pull_request' && '([^']+)' \|\| '([^']+)'\s*\}\}",
            build['env']['XILA_DEMO_LOCALES'],
        )
        self.assertIsNotNone(locales_match, build['env']['XILA_DEMO_LOCALES'])
        self.assertEqual(locales_match.group(1), 'en')
        self.assertEqual(
            locales_match.group(2).split(),
            ['en', 'fr', 'es', 'de', 'pt-BR', 'it', 'pl', 'ja', 'zh-CN'],
        )

        steps = build['steps']
        locked_idx, locked_step = next(
            (idx, step)
            for idx, step in enumerate(steps)
            if step.get('run') == 'mise run ci:locked-revisions'
        )
        self.assertEqual(locked_step.get('id'), 'locked')

        apps_checkout = next(
            step for step in steps
            if step.get('with', {}).get('repository') == 'xila-project/applications'
        )
        self.assertEqual(
            build['env']['XILA_FONT_CATALOG_ROOTS'],
            f"${{{{ github.workspace }}}}/{apps_checkout['with']['path']}",
        )

        core_idx, core_checkout = next(
            (idx, step)
            for idx, step in enumerate(steps)
            if step.get('with', {}).get('repository') == 'xila-project/core'
        )
        pages_idx = next(
            idx for idx, step in enumerate(steps) if step.get('run') == 'mise run pages:build'
        )
        self.assertLess(locked_idx, core_idx)
        self.assertLess(core_idx, pages_idx)
        self.assertEqual(core_checkout.get('if'), "github.event_name != 'pull_request'")
        self.assertEqual(core_checkout['with'].get('ref'), '${{ steps.locked.outputs.core }}')
        self.assertIs(core_checkout['with'].get('persist-credentials'), False)

        core_path = core_checkout['with']['path']
        self.assertIn(f"{core_path}/tools/localization/fetch-japanese-font.sh", tasks['ci:font-japanese']['run'])
        self.assertIn('tools/ci/locked_revision.py core', tasks['ci:font-japanese']['run'])
        self.assertIn(f"{core_path}/subsystems/graphics/fonts/SourceHanSansJP-Regular.otf", script)

        pages_upload = next(
            step for step in steps
            if step.get('uses', '').startswith('actions/upload-pages-artifact@')
        )
        self.assertEqual(pages_upload['with']['path'], 'pages/')
        artifacts = {
            step['with']['name']: step['with']['path']
            for step in steps
            if step.get('uses', '').startswith('actions/upload-artifact@')
        }
        self.assertEqual(artifacts['platform-demonstrations'], 'pages/demonstrations/')
        self.assertEqual(artifacts['reference-filesystem-images'], 'target/reference-filesystem-images/')

        deploy = demo['jobs']['deploy']
        self.assertEqual(deploy['needs'], 'build')
        self.assertEqual(deploy['permissions']['pages'], 'write')
        self.assertEqual(deploy['environment']['name'], 'github-pages')
        deploy_if = deploy['if']
        self.assertNotIn("github.event_name == 'pull_request'", deploy_if)
        for token in (
            "github.event_name == 'workflow_run'",
            "github.event.workflow_run.conclusion == 'success'",
            "github.event.workflow_run.event == 'push'",
            "github.event.workflow_run.head_branch == 'main'",
            "github.ref == 'refs/heads/main'",
            "inputs.platform_ref == '' || inputs.platform_ref == github.sha",
        ):
            self.assertIn(token, deploy_if)
        self.assertIn('github.event.workflow_run.head_sha', build['steps'][0]['with']['ref'])

        landing = read('examples/wasm/pages-index.html')
        self.assertIn('url=demonstrations/wasm/en/', landing)
        self.assertIn('href="demonstrations/wasm/en/"', landing)

        index_html = read('examples/wasm/index.html')
        self.assertIn('href="licenses/"', index_html)
        self.assertIn('href="licenses/THIRD_PARTY_NOTICES.txt"', index_html)
        notices = read('examples/wasm/licenses/THIRD_PARTY_NOTICES.txt')
        for token in (
            'Montserrat',
            'Source Han Sans',
            'Copyright 2014-2025 Adobe',
            'Font Awesome',
            'Fonticons, Inc.',
            'unscii-16.woff',
            'Fixedsys Excelsior',
            'SIL OPEN FONT LICENSE',
        ):
            self.assertIn(token, notices)
        self.assertNotIn('GNU GENERAL PUBLIC LICENSE', notices)

    def test_demo_images_follow_each_locale_target(self):
        script = tomllib.loads(read('mise.ci.toml'))['tasks']['pages:build']['run']
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            (root / 'examples/wasm/licenses').mkdir(parents=True)
            (root / 'examples/wasm/pages-index.html').write_text('landing')
            jp_font = root / '.ci-core-fonts/subsystems/graphics/fonts/SourceHanSansJP-Regular.otf'
            jp_font.parent.mkdir(parents=True)
            jp_font.write_text('stale')
            (root / 'examples/wasm/licenses/THIRD_PARTY_NOTICES.txt').write_text('notices')
            cargo = bin_dir / 'cargo'
            cargo.write_text(textwrap.dedent("""\
                #!/usr/bin/env python3
                import os, pathlib, sys
                args = sys.argv[1:]
                output = pathlib.Path(args[args.index('--') + 1])
                bundles = pathlib.Path(args[args.index('--bundles-dir') + 1])
                locale = os.environ['EMBEDDED_I18N_LOCALE']
                assert (bundles / 'dev.xila.calculator/executables/calculator.wasm').read_text() == locale
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
                  ci:font-japanese)
                    test -z "${FAIL_FONT_SETUP:-}" || exit 42
                    mkdir -p .ci-core-fonts/subsystems/graphics/fonts
                    printf 'jp-font' > .ci-core-fonts/subsystems/graphics/fonts/SourceHanSansJP-Regular.otf
                    printf 'fetched\n' >> jp_fetch.log
                    ;;
                  ci:build-applications)
                    mkdir -p "$CARGO_TARGET_DIR/wasm32-wasip1/release-wasm"
                    printf '%s' "$EMBEDDED_I18N_LOCALE" > "$CARGO_TARGET_DIR/wasm32-wasip1/release-wasm/calculator.wasm"
                    ;;
                  ci:bundle-applications)
                    mkdir "$XILA_BUNDLES_DIR"
                    mkdir -p "$XILA_BUNDLES_DIR/dev.xila.calculator/executables"
                    cp "$CARGO_TARGET_DIR/wasm32-wasip1/release-wasm/calculator.wasm" "$XILA_BUNDLES_DIR/dev.xila.calculator/executables/"
                    ;;
                  pages:demo)
                    test "$(cat target/seed/wasm/seed.img)" = "$EMBEDDED_I18N_LOCALE:wasm"
                    if [ "$EMBEDDED_I18N_LOCALE" = ja ]; then
                      test "$(cat "$XILA_JAPANESE_FONT_PATH")" = jp-font
                    fi
                    mkdir -p examples/wasm/dist
                    cp target/seed/wasm/seed.img examples/wasm/dist/seed.img
                    ;;
                  *) exit 1 ;;
                esac
                """))
            mise.chmod(0o755)
            base_env = {k: v for k, v in os.environ.items() if k != 'XILA_JAPANESE_FONT_PATH'}
            env = base_env | {'PATH': f"{bin_dir}:{os.environ['PATH']}", 'CARGO_TARGET_ROOT': str(root / 'locale targets')}
            result = subprocess.run(['sh', '-c', script], cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((root / 'jp_fetch.log').read_text().splitlines(), ['fetched'])
            self.assertEqual((root / 'pages/index.html').read_text(), 'landing')
            for locale in ('en', 'fr', 'es', 'de', 'pt-BR', 'it', 'pl', 'ja', 'zh-CN'):
                for bundle in ('wasm', 'native'):
                    self.assertEqual((root / 'target/reference-filesystem-images' / locale / f'{bundle}.img').read_text(), f'{locale}:{bundle}')
                self.assertEqual((root / 'pages/demonstrations/wasm' / locale / 'seed.img').read_text(), f'{locale}:wasm')
                self.assertEqual((root / 'pages/demonstrations/wasm' / locale / 'licenses/THIRD_PARTY_NOTICES.txt').read_text(), 'notices')

            # PR builds (en only) must not invoke ci:font-japanese, and bad explicit paths must fail early.
            (root / 'jp_fetch.log').unlink()
            pr_result = subprocess.run(
                ['sh', '-c', script],
                cwd=root,
                env=env | {'XILA_DEMO_LOCALES': 'en', 'CARGO_TARGET_ROOT': str(root / 'pr targets')},
                capture_output=True,
                text=True,
            )
            self.assertEqual(pr_result.returncode, 0, pr_result.stdout + pr_result.stderr)
            self.assertFalse((root / 'jp_fetch.log').exists())

            bad_result = subprocess.run(
                ['sh', '-c', script],
                cwd=root,
                env=env | {'XILA_DEMO_LOCALES': 'ja', 'XILA_JAPANESE_FONT_PATH': str(root / 'missing.otf')},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(bad_result.returncode, 0)
            self.assertIn('ci:font-japanese', bad_result.stderr)

            failed_setup = subprocess.run(
                ['sh', '-c', script],
                cwd=root,
                env=env | {
                    'XILA_DEMO_LOCALES': 'ja',
                    'XILA_JAPANESE_FONT_PATH': str(jp_font),
                    'FAIL_FONT_SETUP': '1',
                },
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(failed_setup.returncode, 0)
            self.assertIn('mise -E ci run ci:font-japanese', failed_setup.stderr)

    def test_japanese_font_setup_recovers_from_interrupted_checkout(self):
        script = tomllib.loads(read('mise.ci.toml'))['tasks']['ci:font-japanese']['run']
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            upstream = root / 'upstream'
            checkout = root / '.ci-core-fonts'

            def git(*args, cwd=root):
                return subprocess.check_output(['git', *args], cwd=cwd, text=True).strip()

            git('init', '-q', str(upstream))
            fetcher = upstream / 'tools/localization/fetch-japanese-font.sh'
            fetcher.parent.mkdir(parents=True)
            fetcher.write_text("echo verified\n")
            git('add', '.', cwd=upstream)
            git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                '-c', 'commit.gpgsign=false', 'commit', '-qm', 'Fixture', cwd=upstream)
            revision = git('rev-parse', 'HEAD', cwd=upstream)
            git('init', '-q', str(checkout))
            self.assertEqual(git('remote', cwd=checkout), '')

            tools = root / 'tools/ci'
            tools.mkdir(parents=True)
            (tools / 'locked_revision.py').write_text(read('tools/ci/locked_revision.py'))
            (root / 'Cargo.lock').write_text(
                f'[[package]]\nsource = "git+https://github.com/xila-project/core?branch=main#{revision}"\n'
            )
            env = os.environ | {
                'GIT_CONFIG_COUNT': '1',
                'GIT_CONFIG_KEY_0': f'url.file://{upstream}.insteadOf',
                'GIT_CONFIG_VALUE_0': 'https://github.com/xila-project/core.git',
            }

            def assert_setup(env):
                result = subprocess.run(
                    ['sh', '-c', script], cwd=root, env=env, capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(git('rev-parse', 'HEAD', cwd=checkout), revision)
                self.assertEqual(result.stdout.strip(), 'verified')

            assert_setup(env)
            assert_setup(env | {'GIT_ALLOW_PROTOCOL': ''})

    def test_driver_and_seed_checks_are_required(self):
        ci = read('.github/workflows/ci.yml')
        for task in ('clippy:drivers', 'test:drivers', 'test:seed', 'firmware:test',
                     'ci:profiling', 'ci:profiling-runtime-tests', 'ci:profiling-native-tests',
                     'clippy:browser', 'check:browser', 'test:browser-http', 'test:browser-drivers',
                     'build:nano-33-ble'):
            self.assertIn(f'mise run {task}', ci)
        browser_tests = tomllib.loads(read('mise.ci.toml'))['tasks']['ci:browser-node-tests']['run']
        self.assertIn('drivers/wasm/tests/seed.test.mjs', browser_tests)
        self.assertIn('examples/wasm/tests/disk_manager.test.mjs', browser_tests)
        mise_vars = tomllib.loads(read('mise.toml'))['vars']
        wasm_bindgen_versions = {
            package['version']
            for package in tomllib.loads(read('Cargo.lock'))['package']
            if package['name'] == 'wasm-bindgen'
        }
        self.assertEqual(wasm_bindgen_versions, {mise_vars['wasm_bindgen_version']})
        self.assertIn('needs: [quality, native, nano-33-ble, browser]', ci)

    def test_host_test_suites_use_pinned_nextest_and_run_doctests(self):
        self.assertRegex(tomllib.loads(read('mise.toml'))['vars']['nextest_version'], r'^\d+\.\d+\.\d+$')
        expected = 'cargo-nextest-{{ vars.nextest_version }}'
        tasks = {}
        for name in ('mise.toml', 'mise.ci.toml'):
            for task, configuration in tomllib.loads(read(name)).get('tasks', {}).items():
                merged = tasks.setdefault(task, {})
                # mise merges the tools of a task across configuration files.
                merged['tools'] = merged.get('tools', {}) | configuration.get('tools', {})
                merged.update({key: value for key, value in configuration.items() if key != 'tools'})
        for task, requires_doctests in (
            ('test:seed', True),
            ('test:drivers', True),
            ('firmware:test', True),
            ('ci:profiling', True),
            ('ci:profiling-runtime-tests', False),
            ('ci:profiling-native-tests', False),
        ):
            with self.subTest(task=task):
                configuration = tasks[task]
                self.assertEqual(configuration['tools']['github:nextest-rs/nextest'], expected)
                self.assertIn('cargo nextest run --locked', configuration['run'])
                if requires_doctests:
                    self.assertRegex(configuration['run'], r'cargo test [^\n]*--doc')

    def test_workspace_actions_share_one_pinned_revision(self):
        revisions = set()
        for workflow in sorted((ROOT / '.github/workflows').glob('*.yml')):
            revisions.update(re.findall(
                r'xila-project/workspace/\.github/(?:actions/setup-rust|workflows/quality\.yml)@([0-9a-f]{40})',
                workflow.read_text()))
        self.assertEqual(len(revisions), 1, revisions)

    def test_nano_build_installs_the_linker_package_set(self):
        job = yaml.safe_load(read('.github/workflows/ci.yml'))['jobs']['nano-33-ble']
        tasks = [step['run'] for step in job['steps'] if 'run' in step]
        self.assertEqual(tasks, [
            'mise run ci:packages-linker',
            'mise run build:nano-33-ble',
            'mise run release:nano-33-ble-image',
        ])

    def test_quality_reuses_shared_job_without_weakening_gate(self):
        ci = read('.github/workflows/ci.yml')
        self.assertIn('  quality:', ci)
        self.assertIn('xila-project/workspace/.github/workflows/quality.yml@', ci)
        self.assertIn('needs: [quality, native, nano-33-ble, browser]', ci)
        quality = yaml.safe_load(ci)['jobs']['quality']
        self.assertRegex(quality['uses'], r'/quality\.yml@[0-9a-f]{40}$')
        self.assertEqual(quality['with']['extra-task'], 'ci:quality-extra')
        for line in ci.splitlines():
            self.assertNotRegex(line, r'^      - (uses|run|env):')
        self.assertIn('name: "Gate | Verify mandatory checks"', ci)

    def test_ci_cache_and_toolchain_wrapping_are_scoped(self):
        tasks = tomllib.loads(read('mise.ci.toml'))
        wrapper = tasks['wrappers']['cargo']
        self.assertEqual(wrapper['env']['MBX_CARGO_SHIM_MODE'], '1')
        self.assertNotIn('mr-boxington', str(tasks.get('task_templates', {})))
        ci = read('.github/workflows/ci.yml')
        self.assertIn('mise run ci:packages-host', ci)
        self.assertIn('mise run ci:packages-linker', ci)
        self.assertNotIn('apt-get', ci)
        self.assertIn('v1-platforms-host', ci)
        self.assertIn('v1-platforms-browser', ci)
        self.assertIn('cache-mode: target', ci)
        self.assertIn('cache-mode: objects', read('.github/workflows/demo.yml'))

    def test_espressif_emulator_job_is_advisory_and_uses_pinned_tasks(self):
        ci = yaml.safe_load(read('.github/workflows/ci.yml'))
        job = ci['jobs']['espressif-emulator']
        matrix = job['strategy']['matrix']['include']
        self.assertEqual({row['chip']: row['task'] for row in matrix}, {
            'esp32s3': 'test:esp32-s3-qemu',
            'esp32c6': 'test:esp32-c6-qemu',
        })
        # Advisory while the emulator check settles: it is not part of the gate.
        self.assertNotIn('espressif-emulator', ci['jobs']['required']['needs'])
        self.assertIn('timeout-minutes', job)
        for step in job['steps']:
            self.assertRegex(step['name'], r'^(Setup|Test|Report) \| ')
            self.assertNotIn('apt-get', step.get('run', ''))
            if 'run' in step:
                self.assertTrue(step['run'].startswith('mise run '), step['name'])
        mise = tomllib.loads(read('mise.toml'))
        for task in ('setup:espressif-rust', 'setup:espressif-emulator',
                     'build:esp32-s3-qemu', 'setup:esp32c6-emulator',
                     'build:esp32-c6-qemu', *(row['task'] for row in matrix)):
            self.assertIn(task, mise['tasks'])
        for pin in ('esp_qemu_version', 'espflash_version', 'espup_version'):
            self.assertNotIn('latest', mise['vars'][pin])

    def test_every_espressif_board_has_a_build_task_and_a_ci_job(self):
        boards = {path.parent.name for path in (ROOT / 'examples/boards').glob('*/Cargo.toml')}
        mise = tomllib.loads(read('mise.toml'))
        for board in boards:
            self.assertIn(f'build:{board}', mise['tasks'])
        ci = yaml.safe_load(read('.github/workflows/ci.yml'))
        emulated = {row['task'].removeprefix('test:')
                    for row in ci['jobs']['espressif-emulator']['strategy']['matrix']['include']}
        built = {row['board'] for row in ci['jobs']['espressif-boards']['strategy']['matrix']['include']}
        # Each board is checked in CI once: booted in QEMU or built.
        self.assertEqual(emulated | built, boards)
        self.assertFalse(emulated & built)
        self.assertNotIn('espressif-boards', ci['jobs']['required']['needs'])

    def test_xila_checkouts_use_locked_revisions(self):
        # Checkouts of Core, Applications and package_manager follow Cargo.lock
        # (`ci:locked-revisions`) instead of hard-coding commits that drift.
        checkout = re.compile(
            r'repository: xila-project/(core|applications|package_manager)\n(?:\s+[a-z-]+: .*\n)*?\s+ref: (.*)')
        for path in sorted((ROOT / '.github/workflows').glob('*.yml')):
            workflow = path.read_text()
            triggers = workflow.split('\njobs:', 1)[0]
            if not any(event in triggers for event in ('pull_request', 'push:', 'workflow_dispatch', 'workflow_run', 'schedule')):
                continue  # Reusable workflow: the trusted caller supplies its refs.
            for repository, ref in checkout.findall(workflow):
                self.assertEqual(ref.strip(), '${{ steps.locked.outputs.%s }}' % repository, f'{path.name}: {repository}')
            if 'steps.locked.outputs.' in workflow:
                self.assertIn('run: mise run ci:locked-revisions', workflow, path.name)

    def test_cross_repository_workflow_uses_explicit_refs_and_checks_identity(self):
        workflow = read('.github/workflows/integration.yml')
        for token in ('local_overrides.py',
                      'cargo metadata', 'cargo tree', 'package_seed_check.py'):
            self.assertIn(token, workflow)
        self.assertIn('assert not bad', workflow)
        # Default checkouts follow the commits Platforms locks, not hard-coded SHAs.
        self.assertIn('run: mise run ci:locked-revisions', workflow)
        task = tomllib.loads(read('mise.ci.toml'))['tasks']['ci:locked-revisions']['run']
        self.assertIn('locked_revision.py "$repository" "$repository"', task)
        # No caller-supplied refs: checked-out code always comes from the locked commits.
        self.assertNotIn('inputs.', workflow)
        for repository, ref in (('core', 'steps.locked.outputs.core'),
                                ('applications', 'steps.locked.outputs.applications'),
                                ('package_manager', 'steps.locked.outputs.package_manager')):
            self.assertIn('ref: ${{ ' + ref + ' }}', workflow, repository)
        self.assertLess(workflow.index('Resolve locked Xila revisions'), workflow.index('repository: xila-project/core'))
        self.assertIn('mise run ci:integration-build-applications', workflow)
        tasks = tomllib.loads(read('mise.ci.toml'))
        build = tasks['tasks']['ci:integration-build-applications']
        self.assertIn('wasm32-wasip1', build['tools']['rust']['targets'])
        self.assertIn('--profile release-wasm', build['run'])
        self.assertIn('wasm32-wasip1/release-wasm', workflow)

    def test_guest_builds_are_size_optimized_like_released_bundles(self):
        tasks = tomllib.loads(read('mise.ci.toml'))
        for name in ('ci:build-applications', 'ci:integration-build-applications'):
            task = tasks['tasks'][name]
            self.assertEqual(task['tools']['aqua:WebAssembly/binaryen'], '{{ vars.binaryen_version }}', name)
            self.assertIn('bash tools/optimize_wasm.sh', task['run'], name)
            self.assertLess(task['run'].index('cargo build'), task['run'].index('optimize_wasm.sh'), name)
        script = read('tools/optimize_wasm.sh')
        proposals = sorted(flag for flag in script.split() if flag.startswith('--enable-'))
        # Exactly the proposals Applications' host engine enables.
        self.assertEqual(proposals, sorted([
            '--enable-bulk-memory', '--enable-custom-page-sizes', '--enable-extended-const',
            '--enable-multivalue', '--enable-mutable-globals', '--enable-nontrapping-float-to-int',
            '--enable-reference-types', '--enable-sign-ext', '--enable-tail-call']))
        self.assertIn('-Oz', script.split())

    def test_profiles_are_explicit_and_both_seed_weather(self):
        profiles = ROOT / 'tools/fs_image/profiles'
        native = (profiles / 'native.txt').read_text().splitlines()
        browser = (profiles / 'browser.txt').read_text().splitlines()
        self.assertEqual(native, ['calculator', 'clock', 'digits', 'pixel_painter', 'synth', 'text_editor', 'weather'])
        self.assertEqual(browser, native)


if __name__ == '__main__':
    unittest.main()
