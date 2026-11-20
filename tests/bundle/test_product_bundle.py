"""REQ-005 / AC-005-06: exercise the packer with a tiny real Cargo binary.

These are script regressions, not HoH product acceptance. Cargo actually builds
the old and new executables; malformed --version responses use explicit fixtures.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
OLD_COMMIT = "1" * 40
NEW_COMMIT = "2" * 40


class ProductBundle(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hoh-bundle-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "project"
        (self.root / "scripts/product").mkdir(parents=True)
        for script in ["bundle.sh", "release-exe.sh", "build.sh", "build-commit.sh"]:
            shutil.copy(ROOT / "scripts/product" / script, self.root / "scripts/product" / script)
        (self.root / "src").mkdir()
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "hoh"\nversion = "0.0.0"\nedition = "2024"\n'
        )
        (self.root / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "hoh"\nversion = "0.0.0"\n'
        )
        (self.root / "src/main.rs").write_text(
            'fn main() { println!("hoh 0.0.0\\ncommit {}\\ncore 0.0.0, agents 0.0.0", '
            'option_env!("HOH_BUILD_COMMIT").unwrap_or("unknown")); }\n'
        )
        for name in ["LICENSE", "THIRD_PARTY_NOTICES.md"]:
            (self.root / name).write_text(name + "\n")
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("HOH_", "CARGO_"))}
        self.env["CARGO_HOME"] = str(self.base / "cargo-home")
        self.out = self.root / "hoh-release.tar.gz"

    def build(self, commit=NEW_COMMIT, target=None, extra_env=None):
        env = self.env.copy()
        env.update(extra_env or {})
        if commit is not None:
            env["HOH_BUILD_COMMIT"] = commit
        if target is not None:
            env["CARGO_TARGET_DIR"] = str(target)
        p = subprocess.run(["bash", "scripts/product/release-exe.sh"], cwd=self.root,
                           env=env, capture_output=True, text=True, timeout=60)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.executable = Path(p.stdout.rstrip("\n"))
        self.assertTrue(self.executable.is_absolute() and self.executable.is_file())
        self.build_diagnostics = p.stderr
        return env

    def bundle(self, env, args=None):
        if args is None:
            args = [str(self.out), str(self.executable)]
        return subprocess.run(["bash", "scripts/product/bundle.sh", *args],
                              cwd=self.root, env=env, capture_output=True, text=True, timeout=30)

    def assert_binary(self, expected):
        with tarfile.open(self.out) as archive:
            self.assertEqual(hashlib.sha256(archive.extractfile("hoh/bin/hoh").read()).hexdigest(),
                             hashlib.sha256(expected.read_bytes()).hexdigest(),
                             "the archive must contain the binary Cargo just built")
            self.assertEqual(archive.getmember("hoh/bin/hoh").mode, 0o755)
            self.assertIn("hoh/LICENSE", archive.getnames())

    def assert_refused(self, env, diagnostic, args=None):
        # A failed preflight neither creates a release nor overwrites an existing one.
        for previous in [None, b"previous release"]:
            if previous is not None:
                self.out.write_bytes(previous)
            p = self.bundle(env, args)
            self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)
            self.assertIn(diagnostic, p.stderr)
            self.assertNotIn("Traceback", p.stderr)
            self.assertEqual(sum(line.startswith("bundle:") for line in p.stderr.splitlines()), 1, p.stderr)
            if previous is None:
                self.assertFalse(self.out.exists())
            else:
                self.assertEqual(self.out.read_bytes(), previous)

    def test_env_target_packs_fresh_binary_instead_of_stale_default(self):
        self.build(OLD_COMMIT)
        target = self.root / "fresh target 中文"
        env = self.build(target=target)
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assert_binary(self.executable)
        first = self.out.read_bytes()
        self.assertEqual(self.bundle(env).returncode, 0)
        self.assertEqual(self.out.read_bytes(), first, "the release tar must stay reproducible")

    def test_config_target_directory(self):
        self.build(OLD_COMMIT)
        (self.root / ".cargo").mkdir()
        (self.root / ".cargo/config.toml").write_text('[build]\ntarget-dir = "config target"\n')
        env = self.build()
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assert_binary(self.executable)

    def test_relative_env_target_overrides_config(self):
        self.build(OLD_COMMIT)
        (self.root / ".cargo").mkdir()
        (self.root / ".cargo/config.toml").write_text('[build]\ntarget-dir = "unused"\n')
        env = self.build(target="relative target")
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assert_binary(self.executable)

    def test_default_target_directory(self):
        env = self.build()
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assert_binary(self.executable)

    def test_build_diagnostics_remain_visible_and_fresh_artifacts_are_usable(self):
        source = self.root / "src/main.rs"
        source.write_text(source.read_text() + '\nfn unused_diagnostic_marker() {}\n')
        self.build()
        self.assertIn("unused_diagnostic_marker", self.build_diagnostics)
        executable = self.executable
        # A cached successful build still reports the executable in compiler-artifact.
        env = self.build()
        self.assertEqual(self.executable, executable)
        self.assertEqual(self.bundle(env).returncode, 0)
        self.assert_binary(executable)

    def test_btd51_failed_build_keeps_existing_bundle(self):
        self.build()
        (self.root / "Cargo.toml").write_text("invalid manifest")
        self.assert_build_refused(self.env)

    def assert_build_refused(self, env):
        env = dict(env, HOH_EVIDENCE_DIR=str(self.root))
        for previous in [None, b"previous release"]:
            if previous is not None:
                self.out.write_bytes(previous)
            p = subprocess.run(["bash", "scripts/product/build.sh"], cwd=self.root,
                               env=env, capture_output=True, text=True, timeout=60)
            self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)
            self.assertNotIn("Traceback", p.stderr)
            self.assertEqual(sum(line.startswith("bundle:") for line in p.stderr.splitlines()), 1, p.stderr)
            if previous is None:
                self.assertFalse(self.out.exists())
            else:
                self.assertEqual(self.out.read_bytes(), previous)

    def cargo_fixture(self, env, messages, exit_code=0):
        # Explicit Cargo protocol fixture: real Cargo configuration is covered above.
        commands = self.base / "commands"
        commands.mkdir(exist_ok=True)
        (commands / "messages").write_text(messages)
        cargo = commands / "cargo"
        cargo.write_text(
            '#!/bin/sh\nprintf "%s\\n" "$*" >>"$(dirname "$0")/calls"\n'
            'cat "$(dirname "$0")/messages"\necho "fixture diagnostic" >&2\n'
            f'exit {exit_code}\n'
        )
        cargo.chmod(0o755)
        return dict(env, PATH=str(commands) + os.pathsep + env["PATH"])

    def artifact(self, executable, name="hoh", kind="bin"):
        return json.dumps({"reason": "compiler-artifact", "target": {"name": name, "kind": [kind]},
                           "executable": executable}) + "\n"

    def test_btd51_malformed_missing_or_ambiguous_artifact_is_refused(self):
        env = self.build()
        valid = self.artifact(str(self.executable))
        for messages in ["not JSON", "null\n", "{}\n", valid + valid,
                         self.artifact(None), self.artifact("target/release/hoh"),
                         self.artifact(str(self.root / "missing")), self.artifact(str(self.root)),
                         self.artifact(str(self.executable), name="another"),
                         self.artifact(str(self.executable), kind="example")]:
            with self.subTest(messages=messages):
                self.assert_build_refused(self.cargo_fixture(env, messages))
                self.out.unlink()
        # A plausible artifact from a failing build is not success.
        self.assert_build_refused(self.cargo_fixture(env, valid, exit_code=101))

    def test_release_helper_selects_only_bin_hoh_in_one_build(self):
        env = self.build()
        messages = (self.artifact(str(self.executable), name="another")
                    + self.artifact(None, kind="lib") + self.artifact(str(self.executable)))
        env = self.cargo_fixture(env, messages)
        p = subprocess.run(["bash", "scripts/product/release-exe.sh"], cwd=self.root,
                           env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout, str(self.executable) + "\n")
        self.assertIn("fixture diagnostic", p.stderr)
        self.assertEqual((self.base / "commands/calls").read_text().splitlines(),
                         ["build --workspace --release --locked --message-format=json-render-diagnostics"])

    def test_wrong_commit_is_rejected_before_writing_archive(self):
        env = self.build(OLD_COMMIT)
        env["HOH_BUILD_COMMIT"] = NEW_COMMIT
        self.assert_refused(env, "commit mismatch")

    def test_unset_commit_requires_unknown_and_reports_it(self):
        env = self.build(commit=None)
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("HOH_BUILD_COMMIT is unset", p.stderr)
        self.assertIn("commit unknown", p.stderr)
        self.assert_binary(self.executable)
        self.out.unlink()
        env = self.build()
        del env["HOH_BUILD_COMMIT"]
        self.assert_refused(env, "commit mismatch")

    def test_dirty_commit_is_explicit_and_must_match_exactly(self):
        env = self.build(NEW_COMMIT + "-dirty")
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn(NEW_COMMIT + "-dirty", p.stderr)
        self.out.unlink()
        env["HOH_BUILD_COMMIT"] = NEW_COMMIT
        self.assert_refused(env, "commit mismatch")

    def test_invalid_expected_commit_is_rejected(self):
        env = self.build()
        env["HOH_BUILD_COMMIT"] = "invalid"
        self.assert_refused(env, "invalid HOH_BUILD_COMMIT")

    def test_missing_or_malformed_version_is_rejected(self):
        env = self.build()
        binary = self.executable
        # Explicit executable fixtures for error paths, never product evidence.
        for output in ["hoh 0.0.0", "hoh 0.0.0\\ncommit " + NEW_COMMIT + "extra",
                       "hoh 0.0.0\\ncommit unknown\\ncommit " + NEW_COMMIT]:
            with self.subTest(output=output):
                binary.write_text("#!/bin/sh\nprintf '" + output + "\\n'\n")
                self.assert_refused(env, "commit mismatch")
                self.out.unlink()
        binary.write_text("#!/bin/sh\nexit 7\n")
        self.assert_refused(env, "--version failed")

    def test_packer_needs_no_cargo_metadata_or_manifest(self):
        env = self.build()
        (self.root / "Cargo.toml").write_text("invalid manifest")
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assert_binary(self.executable)

    def test_missing_selected_binary_never_falls_back_to_stale_default(self):
        env = self.build(OLD_COMMIT)
        self.executable = self.root / "missing target/hoh"
        env["HOH_BUILD_COMMIT"] = NEW_COMMIT
        self.assert_refused(env, "executable")

    def test_btd51_missing_extra_or_empty_arguments_are_refused(self):
        env = self.build()
        for args in [[], [str(self.out)], [str(self.out), str(self.executable), "extra"],
                     ["", str(self.executable)], [str(self.out), ""]]:
            with self.subTest(args=args):
                self.assert_refused(env, "usage:", args)
                self.out.unlink()

    def test_btd51_relative_or_non_file_executable_is_refused(self):
        env = self.build()
        for executable in [self.executable.relative_to(self.root), self.executable.parent]:
            with self.subTest(executable=executable):
                self.assert_refused(env, "executable", [str(self.out), str(executable)])
                self.out.unlink()

    def test_archive_failure_keeps_existing_bundle(self):
        env = self.build()
        # Explicit compressor failure fixture, not a real Cargo/product failure.
        commands = self.base / "commands"
        commands.mkdir()
        gzip = commands / "gzip"
        gzip.write_text("#!/bin/sh\ncat >/dev/null\nprintf 'partial archive'\nexit 7\n")
        gzip.chmod(0o755)
        env["PATH"] = str(commands) + os.pathsep + env["PATH"]
        for previous in [None, b"previous release"]:
            if previous is not None:
                self.out.write_bytes(previous)
            p = self.bundle(env)
            self.assertNotEqual(p.returncode, 0)
            if previous is None:
                self.assertFalse(self.out.exists())
            else:
                self.assertEqual(self.out.read_bytes(), previous)
            self.assertEqual(list(self.root.glob("hoh-release.tar.gz.*")), [])

    def assert_target_build(self, location=None, config=None):
        # BTD-01 / BTD-50: equal commits cannot distinguish stale and fresh bytes.
        triple = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
        triple = next(l.split(": ", 1)[1] for l in triple.splitlines() if l.startswith("host: "))
        self.build()
        stale = self.executable
        (self.root / "src/main.rs").write_text((self.root / "src/main.rs").read_text() + "// triple build\n"
                                               "#[used] static MARK: [u8; 5] = *b\"tripl\";\n")
        if location is None:
            env = self.build(extra_env={"CARGO_BUILD_TARGET": triple})
        else:
            location.parent.mkdir(parents=True, exist_ok=True)
            location.write_text(config.replace("TRIPLE", triple))
            env = self.build()
        fresh = self.executable
        self.assertNotEqual(fresh.read_bytes(), stale.read_bytes())
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assert_binary(fresh)

    def test_a_build_target_triple_packs_cargos_binary_not_the_host_default(self):
        self.assert_target_build()

    def test_btd50_build_target_in_local_cargo_config(self):
        self.assert_target_build(self.root / ".cargo/config.toml", '[build]\ntarget = "TRIPLE"\n')

    def test_btd50_build_target_in_cargo_home(self):
        self.assert_target_build(Path(self.env["CARGO_HOME"]) / "config.toml", '[build]\ntarget = "TRIPLE"\n')

    def test_btd50_build_target_in_parent_config(self):
        self.assert_target_build(self.base / ".cargo/config.toml", '[build]\ntarget = "TRIPLE"\n')

    def test_btd50_dotted_build_target(self):
        self.assert_target_build(self.root / ".cargo/config.toml", 'build.target = "TRIPLE"\n')

    def test_btd50_inline_build_target(self):
        self.assert_target_build(self.root / ".cargo/config.toml", 'build = { target = "TRIPLE" }\n')

    def test_btd50_quoted_build_target(self):
        self.assert_target_build(self.root / ".cargo/config.toml", '[build]\n"target" = "TRIPLE"\n')

    def test_the_bundle_has_the_umask_mode_not_mktemps_0600(self):
        # BTD-100: the archive is written to a mktemp file (0600) and renamed; the bundle must be readable as a plain
        # redirect would have made it under the caller's umask.
        env = self.build()
        exe = self.executable
        p = subprocess.run(["bash", "-c", "umask 0022 && exec bash scripts/product/bundle.sh \"$0\" \"$1\"",
                            str(self.out), str(exe)], cwd=self.root, env=env, capture_output=True, text=True,
                           timeout=30)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(self.out.stat().st_mode & 0o777, 0o644)

