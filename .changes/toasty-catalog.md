---
figstash-cli: "minor:refactor"
figstash-core: "minor:refactor"
figstash-figma: "patch:refactor"
figstash-query: "minor:refactor"
figstash-store: "minor:refactor"
---

Move the durable SQLite catalog to Toasty models and async repository APIs while preserving store-v1 compatibility and raw SQL for SQLite-specific features.

Raise the workspace MSRV to Rust 1.95, as required by Toasty.
