#!/usr/bin/env python3
from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    metadata = json.loads((ROOT / "project.json").read_text(encoding="utf-8"))
    required = [
        "README.md",
        "AGENTS.md",
        "project.json",
        ".zpkg.toml",
        "rust-toolchain.toml",
        "docs/architecture.md",
        *metadata.get("required_paths", []),
    ]
    missing = sorted({path for path in required if not (ROOT / path).exists()})
    if missing:
        raise SystemExit(f"missing required paths: {missing}")

    for path in ROOT.rglob("*"):
        if not path.is_file() or ".git" in path.parts or path.stat().st_size > 1_000_000:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        if any(marker in text for marker in ("<" * 7, "=" * 7, ">" * 7)):
            raise SystemExit(f"conflict marker in {path}")
        if re.search(
            r"gh[pousr]_[A-Za-z0-9]{20,}|lin_api_[A-Za-z0-9]{20,}|BEGIN [A-Z ]*PRIVATE KEY",
            text,
        ):
            raise SystemExit(f"credential-shaped content in {path}")

    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    for marker in (
        'edition = "2024"',
        'rust-version = "1.88"',
        'eal-semantic = { git = "https://github.com/embedded-alerts/eal-libs"',
        'reqwest = { version = "0.12"',
    ):
        if marker not in manifest:
            raise SystemExit(f"Cargo manifest is missing {marker!r}")

    toolchain = (ROOT / "rust-toolchain.toml").read_text(encoding="utf-8")
    for marker in ('channel = "1.88.0"', 'components = ["rustfmt", "clippy"]'):
        if marker not in toolchain:
            raise SystemExit(f"Rust toolchain is missing {marker!r}")

    ingestion = "\n".join(
        (ROOT / path).read_text(encoding="utf-8")
        for path in (
            "src/ingestion/network/fetcher.rs",
            "src/ingestion/network/policy.rs",
            "src/ingestion/network/response.rs",
            "src/ingestion/network/safety.rs",
            "src/ingestion/revision.rs",
        )
    )
    for marker in (
        "Policy::none()",
        "resolve_public_addresses",
        "is_public_ip",
        "max_response_bytes",
        "IF_NONE_MATCH",
        "IF_MODIFIED_SINCE",
        "content_sha256",
        "EmbeddingWorkItem",
        "decide_revision",
    ):
        if marker not in ingestion:
            raise SystemExit(f"ingestion safety contract is missing {marker!r}")

    crawl_once = (ROOT / "src/bin/crawl_once.rs").read_text(encoding="utf-8")
    if "EAL_CRAWL_URL" not in crawl_once or "content_preview" not in crawl_once:
        raise SystemExit("crawl_once must use explicit environment input and bounded output")

    print(
        f"validated {metadata['organization']}/{metadata['repository']} "
        "bounded ingestion contract v2"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
