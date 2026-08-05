# Architecture

Offline-first rule, alert, acknowledgement, and delivery synchronization built with opto-sync contracts.

## Fleet

- `eal-interfaces`
- `eal-api`
- `eal-mash-web`
- `eal-leptos-web`
- `eal-dioxus-web`
- `eal-sync`
- `eal-cli`
- `eal-infra`
- `embedded-alerts-clients`
- `embedded-alerts-libs`
- `embedded-alerts.github.io`
- `embedded-alerts-monorepo`

Interfaces own wire formats; libraries own reusable domain behavior; clients consume versioned contracts; runtimes own deployment behavior; monorepos coordinate pinned revisions. Edge code is allowlisted and never a generic proxy.
