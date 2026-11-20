"""REQ-005 / AC-005-06: exercise the packer with a tiny real Cargo binary.

These are script regressions, not HoH product acceptance. Cargo actually builds
the old and new executables; malformed --version responses use explicit fixtures.
"""
import hashlib
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
        self.root = Path(self.temp.name)
        (self.root / "scripts/product").mkdir(parents=True)
        shutil.copy(ROOT / "scripts/product/bundle.sh", self.root / "scripts/product/bundle.sh")
        (self.root / "src").mkdir()
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "hoh"\nversion = "0.0.0"\nedition = "2024"\n'
        )
        (self.root / "src/main.rs").write_text(
            'fn main() { println!("hoh 0.0.0\\ncommit {}\\ncore 0.0.0, agents 0.0.0", '
            'option_env!("HOH_BUILD_COMMIT").unwrap_or("unknown")); }\n'
        )
        for name in ["LICENSE", "THIRD_PARTY_NOTICES.md"]:
            (self.root / name).write_text(name + "\n")
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("HOH_", "CARGO_"))}
        self.env["CARGO_HOME"] = str(self.root / "cargo-home")
        self.out = self.root / "hoh-release.tar.gz"

    def build(self, commit=NEW_COMMIT, target=None):
        env = self.env.copy()
        if commit is not None:
            env["HOH_BUILD_COMMIT"] = commit
        if target is not None:
            env["CARGO_TARGET_DIR"] = str(target)
        p = subprocess.run(["cargo", "build", "--release", "--offline"], cwd=self.root,
                           env=env, capture_output=True, text=True, timeout=60)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        return env

    def bundle(self, env):
        return subprocess.run(["bash", "scripts/product/bundle.sh", str(self.out)],
                              cwd=self.root, env=env, capture_output=True, text=True, timeout=30)

    def assert_binary(self, expected):
        with tarfile.open(self.out) as archive:
            self.assertEqual(hashlib.sha256(archive.extractfile("hoh/bin/hoh").read()).hexdigest(),
                             hashlib.sha256(expected.read_bytes()).hexdigest(),
                             "the archive must contain the binary Cargo just built")
            self.assertEqual(archive.getmember("hoh/bin/hoh").mode, 0o755)
            self.assertIn("hoh/LICENSE", archive.getnames())

    def assert_refused(self, env, diagnostic):
        # A failed preflight neither creates a release nor overwrites an existing one.
        for previous in [None, b"previous release"]:
            if previous is not None:
                self.out.write_bytes(previous)
            p = self.bundle(env)
            self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)
            self.assertIn(diagnostic, p.stderr)
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
        self.assert_binary(target / "release/hoh")
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
        self.assert_binary(self.root / "config target/release/hoh")

    def test_relative_env_target_overrides_config(self):
        self.build(OLD_COMMIT)
        (self.root / ".cargo").mkdir()
        (self.root / ".cargo/config.toml").write_text('[build]\ntarget-dir = "unused"\n')
        env = self.build(target="relative target")
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assert_binary(self.root / "relative target/release/hoh")

    def test_default_target_directory(self):
        env = self.build()
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assert_binary(self.root / "target/release/hoh")

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
        self.assert_binary(self.root / "target/release/hoh")
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
        binary = self.root / "target/release/hoh"
        # Explicit executable fixtures for error paths, never product evidence.
        for output in ["hoh 0.0.0", "hoh 0.0.0\\ncommit " + NEW_COMMIT + "extra",
                       "hoh 0.0.0\\ncommit unknown\\ncommit " + NEW_COMMIT]:
            with self.subTest(output=output):
                binary.write_text("#!/bin/sh\nprintf '" + output + "\\n'\n")
                self.assert_refused(env, "commit mismatch")
                self.out.unlink()
        binary.write_text("#!/bin/sh\nexit 7\n")
        self.assert_refused(env, "--version failed")

    def test_metadata_error_never_falls_back_to_default_target(self):
        env = self.build()
        (self.root / "Cargo.toml").write_text("invalid manifest")
        self.assert_refused(env, "Cargo.toml")

    def test_missing_selected_binary_never_falls_back_to_stale_default(self):
        env = self.build(OLD_COMMIT)
        env["CARGO_TARGET_DIR"] = str(self.root / "missing target")
        env["HOH_BUILD_COMMIT"] = NEW_COMMIT
        self.assert_refused(env, "missing target")

    def test_a_build_target_triple_packs_cargos_binary_not_the_host_default(self):
        # BTD-01: the same commit built for the host default and for an explicit triple; the triple's bytes differ
        # (a marker in the source), and only Cargo's answer for this build may be packed.
        triple = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
        triple = next(l.split(": ", 1)[1] for l in triple.splitlines() if l.startswith("host: "))
        self.build()
        stale = self.root / "target/release/hoh"
        (self.root / "src/main.rs").write_text((self.root / "src/main.rs").read_text() + "// triple build\n"
                                               "#[used] static MARK: [u8; 5] = *b\"tripl\";\n")
        env = self.env.copy()
        env["HOH_BUILD_COMMIT"] = NEW_COMMIT
        env["CARGO_BUILD_TARGET"] = triple
        p = subprocess.run(["cargo", "build", "--release", "--offline"], cwd=self.root, env=env,
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        fresh = self.root / "target" / triple / "release/hoh"
        self.assertNotEqual(fresh.read_bytes(), stale.read_bytes())
        p = self.bundle(env)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assert_binary(fresh)

    def test_a_build_target_in_cargo_config_is_refused_not_guessed(self):
        env = self.build()
        (self.root / ".cargo").mkdir()
        (self.root / ".cargo/config.toml").write_text('[build]\ntarget = "x86_64-unknown-linux-gnu"\n')
        self.assert_refused(env, "build target in .cargo/config")

