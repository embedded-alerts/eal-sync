"""Regression tests for the existing repository guard, not Rust runtime tests.

The temporary tree contains only the markers required by verify_repo.main().
Credential-shaped negative inputs are synthetic and constructed at runtime so
this test source does not itself contain credentials or trip the same guard.
"""
from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import verify_repo


class VerifyRepoTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="eal-guard-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.write("project.json", json.dumps({"organization": "fixture", "repository": "guard"}))
        for path in ("README.md", "AGENTS.md", ".zpkg.toml", "docs/architecture.md"):
            self.write(path, "fixture\n")
        self.write("Cargo.toml", '\n'.join((
            'edition = "2024"',
            'rust-version = "1.88"',
            'eal-semantic = { git = "https://github.com/embedded-alerts/eal-libs" }',
            'reqwest = { version = "0.12" }',
        )))
        self.write("rust-toolchain.toml", 'channel = "1.88.0"\ncomponents = ["rustfmt", "clippy"]\n')
        self.write("src/ingestion/network/fetcher.rs", '\n'.join((
            "Policy::none()", ".no_proxy()", "resolve_public_addresses", "is_public_ip",
            "allowed_path_prefixes", "require_default_port", "max_response_bytes",
            "IF_NONE_MATCH", "IF_MODIFIED_SINCE", "content_sha256", "EmbeddingWorkItem",
            "decide_revision",
        )))
        for path in (
            "src/ingestion/network/policy.rs", "src/ingestion/network/response.rs",
            "src/ingestion/network/safety.rs", "src/ingestion/revision.rs",
        ):
            self.write(path, "fixture\n")
        self.write("src/bin/crawl_once.rs", "EAL_CRAWL_URL EAL_CRAWL_ALLOWED_PATH_PREFIXES content_preview\n")

    def write(self, relative: str, text: str) -> None:
        destination = self.root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text, encoding="utf-8")

    def run_guard(self) -> int:
        with patch.object(verify_repo, "ROOT", self.root), contextlib.redirect_stdout(io.StringIO()):
            return verify_repo.main()

    def test_clean_control_passes(self) -> None:
        self.assertEqual(self.run_guard(), 0)

    def test_actual_environment_documentation_passes(self) -> None:
        documentation = verify_repo.ROOT / "env/README.md"
        self.write("env/README.md", documentation.read_text(encoding="utf-8"))
        self.assertEqual(self.run_guard(), 0)

    def test_private_key_headers_remain_rejected_in_documentation(self) -> None:
        for algorithm in ("", "RSA", "EC", "OPENSSH"):
            with self.subTest(algorithm=algorithm or "PKCS8"):
                header = " ".join(part for part in ("BEGIN", algorithm, "PRIVATE", "KEY") if part)
                self.write("env/README.md", "-----" + header + "-----\nsynthetic-only\n")
                with self.assertRaisesRegex(SystemExit, "credential-shaped content"):
                    self.run_guard()

    def test_github_token_shapes_remain_rejected(self) -> None:
        for kind in "pousr":
            with self.subTest(kind=kind):
                self.write("docs/example.md", "gh" + kind + "_" + "A" * 24)
                with self.assertRaisesRegex(SystemExit, "credential-shaped content"):
                    self.run_guard()

    def test_linear_token_shape_remains_rejected(self) -> None:
        self.write("docs/example.md", "_".join(("lin", "api", "A" * 24)))
        with self.assertRaisesRegex(SystemExit, "credential-shaped content"):
            self.run_guard()

    def test_conflict_markers_remain_rejected(self) -> None:
        for character in "<=>":
            with self.subTest(character=character):
                self.write("docs/example.md", character * 7)
                with self.assertRaisesRegex(SystemExit, "conflict marker"):
                    self.run_guard()

    def test_metadata_required_paths_remain_enforced(self) -> None:
        self.write("project.json", json.dumps({"required_paths": ["does-not-exist.md"]}))
        with self.assertRaisesRegex(SystemExit, "missing required paths"):
            self.run_guard()

    def test_ingestion_safety_markers_remain_enforced(self) -> None:
        self.write("src/ingestion/network/fetcher.rs", "fixture without required safety markers\n")
        with self.assertRaisesRegex(SystemExit, "ingestion safety contract is missing"):
            self.run_guard()


if __name__ == "__main__":
    unittest.main()
