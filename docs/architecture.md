# Prism Studio — Architecture & Roadmap

## Design principles

1. **Originals are read-only.** All state lives in the SQLite catalog and
   is mirrored to XMP sidecars, so the library survives losing the
   catalog.
2. **One engine, thin shell.** All logic and tests live in `prism-core`.
   The Tauri app only wires that logic to the UI, so a CLI or a different UI
   can reuse the engine as-is.
3. **Typed, versioned develop settings.** These are serde structs with
   `#[serde(default)]` everywhere, so old or partial settings always load.
   This replaces the flat `Group.Key` bag found in the reference product.
4. **Masks are separate from adjustments.** A mask provider decides *where*
   and the shared local-adjustment engine decides *what*. The reference
   product works the same way: its "AI" looks are ordinary presets with an
   ML-generated mask.
5. **Fast browsing path, accurate editing path.** Thumbnails and the viewer
   use embedded RAW previews. Develop and Export decode sensor data.

## Data flow

```
Add folder ─► indexer thread ─► scan (rayon, EXIF) ─► catalog upsert (1 txn)
                                     │                  └─ import XMP of new files
                                     └─► thumbnails + dHash (rayon) ─► catalog
UI grid ◄── prism://thumb/<id>  (cached JPEG, developed variant if edited)
Develop ─► develop_preview(id, settings, max) ─► cached linear source (LRU 4)
                                              └─► pipeline ─► JPEG data URL + histogram
          autosave (600 ms debounce) ─► catalog + sidecar
```

## Roadmap (priority order)

| # | Item | Notes |
|---|---|---|
| ✓ | Full-resolution demosaic (Malvar–He–Cutler) | done; a GPU path remains an option |
| ✓ | Lens correction via lensfun DB | done (downloaded on request) |
| ✓ | ML masks (subject, sky) | U²-Netp via tract, stored as `Mask::Bitmap` |
| ✓ | Face detection/recognition + People view | YuNet + SFace, greedy clustering at cosine 0.40 |
| ✓ | Embedding similarity + text search | TinyCLIP; brute-force cosine (ANN index when libraries exceed ~100k) |
| ✓ | AI keywording | CLIP zero-shot over a fixed vocabulary |
| 7 | Interactive crop handles, brush masks, heal/clone | Edit mode |
| ✓ | Presets (save/apply develop looks), batch rename/resize | `workflow.rs` |
| 9 | Export: AVIF, 16-bit TIFF, metadata embedding, watermark | JPEG/PNG/TIFF/WebP done |
| 10 | Catalog: folder watch (notify), offline volumes | folder tree, calendar and map views done |
