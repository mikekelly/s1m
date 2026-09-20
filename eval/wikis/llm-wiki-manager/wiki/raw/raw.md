---
type: hub
title: Raw Sources
last_updated: 2026-07-01T01:56:22Z
tags: [raw]
related: []
status: active
summary: Hub for immutable ingested artifacts under raw/.
---

# Raw Sources

Immutable ingested artifacts live here. **Never edit** files after ingestion.

| Folder                         | Use                                           |
| ------------------------------ | --------------------------------------------- |
| [articles/](articles/)         | Long-form articles, blog posts, external docs |
| [prs/](prs/)                   | Pull request exports, review threads          |
| [tickets/](tickets/)           | Issue tracker dumps                           |
| [design-notes/](design-notes/) | Informal design notes                         |
| [transcripts/](transcripts/)   | Meeting / chat transcripts                    |
| [assets/](assets/)             | Images and diagrams (Obsidian attachments)    |

After adding a raw file, create a matching summary in `sources/` and log an ingest operation.
