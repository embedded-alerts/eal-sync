#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
queue = (ROOT / "src/queue.rs").read_text()
runner = (ROOT / "src/runner.rs").read_text()
api = (ROOT / "src/api.rs").read_text()
main = (ROOT / "src/main.rs").read_text()
migration = (ROOT / "migrations/001_runtime.sql").read_text()

required = {
    "transactional lease": "FOR UPDATE SKIP LOCKED",
    "lease token": "lease_token",
    "attempt receipt": "eal_crawl_attempts",
    "expired lease recovery": "lease_expired",
    "bounded retry": "retry_backoff_seconds",
    "no shell process": "Command::new(&config.executable)",
    "redirect blocking": "Policy::none()",
    "proxy bypass": ".no_proxy()",
    "tenant API boundary": "x-eal-tenant-id",
    "canonical page route": "v1/sources/{source_id}/pages",
}
text = "\n".join([queue, runner, api, main, migration])
for name, needle in required.items():
    if needle not in text:
        raise SystemExit(f"missing {name}: {needle}")

for forbidden in ["sh -c", "bash -c", "Command::new(\"sh\")", "/send", "/notify"]:
    if forbidden.lower() in text.lower():
        raise SystemExit(f"forbidden crawl-runtime behavior: {forbidden}")

if "CHECK (start_url ~ '^https://')" not in migration:
    raise SystemExit("crawl queue must be HTTPS-only")
if "ENABLE ROW LEVEL SECURITY" not in migration:
    raise SystemExit("internal crawl tables must enable row-level security")
if "notification" not in migration.lower():
    raise SystemExit("schema boundary must state that notification delivery is absent")

print("durable crawl runtime contract verified")
