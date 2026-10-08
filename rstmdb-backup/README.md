# rstmdb-backup

Physical backup/restore archive format (`.rstmbak`) for
[rstmdb](https://github.com/rstmdb/rstmdb).

A `.rstmbak` file is a self-describing container — magic header + compression
byte + a (gzip) tar of `manifest.json`, the `snapshots/`, and the append-only
`wal/` segments that make up a rstmdb `data_dir`. Restoring extracts those files
into a fresh `data_dir`; the server's normal WAL replay reconstructs all state on
the next start.

Used by the `rstmdb backup` / `rstmdb restore` / `rstmdb verify` subcommands.
