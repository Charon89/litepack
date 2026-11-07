# Introduction

LitePack is an open-core, Windows-first archiver. Its `.lpk` format aims to do better than the
common archive formats on real user data by recompressing already-compressed files bit-exactly (JPEG,
and Deflate streams inside PDF, Office, PNG and APK files), by deduplicating and delta-coding across
files, and by using a modern container with recovery records, authenticated encryption, Merkle
integrity, random access and journaling.

LitePack is in development and Phase 1 is under way. These pages grow as the work lands; figures
appear only on the Benchmarks pages, generated from committed result files.

{{#include footer.md}}
