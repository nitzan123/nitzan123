# ACDSee Photo Studio Ultimate 2027 — Reverse-Engineering Findings

Target: `acdsee-photo-studio-ultimate-2027-0-0-4660-win-en.exe` (public trial
installer from acdsee.com, 815,965,744 bytes,
SHA-256 `026e04d9563634cf64bbc328a56c00025538cbbac8a9ea8f63ce41193b91c144`).

Method: static analysis only. Nothing was executed. Tools used: 7-Zip (NSIS
unpack), msitools (MSI `File` table), `pefile` (imports, exports, resources),
`strings` (ASCII and UTF-16LE), REA CLI 6.7.0 (`inspect-pe-resources`; the
main resource DLL exceeded REA's 8 MiB result budget, so its resource census
used `pefile`). No function-level decompilation was performed — see
*Unknowns*.

## Boundary used for the clean-room build

This document records **behaviour, architecture, data models and formats**.
No ACDSee code, decompiled logic, image assets, presets, themes or ML models
were copied into Prism Studio. Encrypted model files (`*.enc`) were **not**
decrypted. Prism Studio uses its own names, its own algorithms and its own
file formats; ACDSee formats are only described here.

Legend: **[O]** observed directly in the artifact · **[I]** inference from
observations · **[U]** unknown.

---

## 1. Packaging and process architecture

| Finding | Kind | Evidence |
|---|---|---|
| NSIS bootstrapper → `ACDSeeUltimateInstaller.exe` → 791 MB MSI with 1,251 files | O | `7z x`, MSI `File` table |
| Main app `ACDSeeUltimate2027.exe` (63 MB, x64, v20.0.0.4660) | O | PE header, `File.Version` |
| `ACDSeeUltimate2027.dll` (120 MB) has **no imports** → resource-only DLL (305 bitmaps, 235 PNG, 68 SVG, 174 cursors, theme, OpenCL source, JSON) | O | pefile resource census |
| Separate background indexer `ACDSeeIndexerUltimate20.exe` linking onnxruntime + OpenCV (does AI tagging/face work off the UI process) | O/I | import table |
| Separate Quick View (`ACDSeeQVUltimate20.exe`), Commander, slideshow (`D3DBaseSlideShow.exe`), screen recorder (`ACDSeeSRUltimate.exe`) | O | file list |
| UI stack: MFC (`mfc140u.dll`) + BCGControlBar Pro (`CBCGPBaseInfo` RTTI) + WebView2 for web panels + Direct2D/DirectWrite + D3D9/11/12 | O | imports, RTTI strings |
| Bundled 3rd-party: LibRaw, lensfun, OpenCV 4.11, ONNX Runtime 1.21 + DirectML 1.15, FAISS, OpenBLAS, MediaInfo, Xerces/Xalan (XML), zlib, IPWorks SSL, GLib | O | file list + versions |

## 2. Plugin architecture (format and connectivity extensibility)

`.apl` files are DLLs with a C ABI, discovered by prefix [O]:

| Prefix | Role | Exported ABI (sample) |
|---|---|---|
| `ID_*` | image **decoder** | `IDP_Init`, `IDP_OpenImageW`, `IDP_GetImageInfo`, `IDP_PageDecode`, `IDP_GetMetadata`, `IDP_GetEmbeddeddata`, `IDP_GetRAWRotateInfo`, `IDP_CloseImage` |
| `IE_*` | image **encoder** | `IEP_Init`, `IEP_ImageEncode`, `IEP_GetValidPixelFormats`, `IEP_SetMetadataListW` |
| `IDE_*` | decoder+encoder (JPEG/PNG/TIFF "ACDStd", AVIF, JXL, WebP, PSD, JP2) | both sets + progressive `IDP_PageDecodeStart/Step/Stop`, `JPEGTrans` (lossless JPEG transforms) |
| `CX_*` | **connectivity/export** (Flickr, SmugMug, Zenfolio, FTP, Email, Archive) | `CXP_Init`, `CXP_InvokeCommand`, `CXP_ShowPlugInDialog`, `CXP_Free` |
| `AM_*` / `AX_*` | archive formats (ZIP, TGZ, CAB, RAR, ARJ, encryption) | — |

RAW decoding = `ID_DCRaw.apl` wrapping `libraw.dll` [O]. Paged decoding
(`IDP_GetPageInfo`/`IDP_PageDecode`) supports multi-page formats [O].

**Design takeaway [I]:** codec and export targets are isolated behind a small
stable ABI; the host never links codecs directly.

## 3. Catalog (database)

| Finding | Kind |
|---|---|
| Storage engine is **CodeBase** (Sequiter xBase; `CODE4.memSizeBlock` error string), tables are `.dbf` files | O |
| Location pattern `%LOCALAPPDATA%\ACD Systems\Catalogs\<ver><edition>\Default\*.dbf` plus a `Default.dbin` marker; ~48 historical version/edition paths kept for migration | O |
| Tables: `Asset`, `AssetType`, `FileType`, `JoinAssetTypeFileType`, `Folder`, `FolderRoot`, `Category`, `CategoryRoot`, `JoinCategoryAsset`, `Collection`, `DateInfo`, `Thumb`, `Thumb1`, `LookupListItem`, `LookupValueItem`, `JoinFieldSetFileType`, `SyncDevice`, `JoinSyncDeviceAsset`, `Config`, `sAsset`, `sConfig` | O |
| Thumbnails are stored **inside** the catalog (`Thumb*.dbf`) | I |
| Face recognition has its own DB interface (`CDBFaceRecognitionInterface::…PersonThumbnail…`) | O |

## 4. Metadata and sidecar model (XMP)

Custom namespaces [O]:

* `http://ns.acdsee.com/iptc/1.0/`, `http://ns.acdsee.com/regions/`,
  `http://ns.acdsee.com/MetadataPreset/1.0/`, `…/sType/Area#`, `…/sType/Dimensions#`
* `acdsee:` fields — `rating`, `tagged`, `categories`, `collections`,
  `keywords`, `caption`, `author`, `notes`, `datetime`, `EditStatus`,
  `OriginatingProgram`, `Snapshots`, `dpp`, `rpp`, `rawrppused`,
  `metadataversion`, `GPSLatitudeExist`/`GPSLongitudeExist`
* `acdsee-rs:` face/region fields — `RegionList`, `Name`, `NameAssignType`,
  `Type`, `ALGArea`/`DLYArea` (algorithm vs. display rectangles), `Distance`

`dpp`/`rpp` hold develop (non-destructive) parameters in XMP [I: name +
co-location with `rawrppused`].

## 5. Develop (non-destructive) parameter model

Develop settings are a flat key/value bag `Group.Key` serialized as XML
`<command><name>Develop</name><parameters><int|float|string id="…">…` [O,
from shipped preset files]. Every group has `.Active` and `.Version` [O].
35 groups / 502 keys recovered [O]:

| Group | Keys (abridged) |
|---|---|
| WhiteBalance | AsShot, Temperature, Tint, Strength |
| Exposure | Exposure, Contrast, FillLight, HighlightRecovery, ClarityStrength, Dehaze, Vibrance, Saturation, Treatment |
| Lighting ("Light EQ") | Mode, Simple.{Shadows,Midtones,Highlights}, LightEQ.{Brightening,Darkening,NumEQPoints,…EQPoints}, Advanced.* |
| AdvancedHSL ("Color EQ") | Hue/Saturation/Brightness sliders **and** curves (node X/Y arrays), Mode |
| ToneCurves / BrightnessContrastCurves / CameraCurves | per channel R/G/B/RGB: NodeCount, NodesX/Y, BlackPoint, WhitePoint, Gamma |
| ToneWheel / ColorWheel | per-selection Hue, Saturation, Brightness, Contrast, HueRange, SatRange, Smoothness |
| SplitTone | HighlightsHue/Saturation, ShadowsHue/Saturation, Balance |
| AdvancedBlackAndWhite | per-band Brightness/Contrast, TintHue/Amount |
| Sharpening | Amount, Radius, Detail, MaskAmount, Threshold |
| NoiseReduction | Luma, LumaStrength, Color, PreserveDetail, FrequencyRange, Quality |
| ChromaticAberration | RedCyan, BlueYellow, Defringe{Color,Radius,Strength} |
| LensDistortion | lensfun-style `AutoLensCorrectionParams` (aperture, focal length/distance, 3 distortion + 6 TCA terms) |
| Perspective | Horizontal, Vertical, TrapezoidH/V |
| Rotation / Crop | StraightenAngle, Rotate90; Left/Top/Right/Bottom, proportion lock |
| Vignette / PostCropVignette | Strength, Radius, Roundness, Feathering |
| SkinTune, SoftFocus, Style (grain, gradient map, cross-process, photo effect), LUT (Data, Opacity, BlendMode), ColorProfile, RedEye, CloneHeal | — |
| **Local**: TuningBrushes, TuningGradients, TuningRadialGradients, DetailBrushes/Gradients/RadialGradients, TuningLumaMask | per mask: Exposure, FillLight, Contrast, Clarity, Saturation, Vibrance, Temp, Tint, **8-band** Hue0..7/Sat0..7/Bri0..7, Feathering, plus **Pixel Targeting** (luma range + colour-wheel range + skin) |

The **8-band** colour model (indices 0–7) is used everywhere colour is
targeted [O].

## 6. AI subsystem

| Finding | Kind |
|---|---|
| Runtime: ONNX Runtime + DirectML (GPU), OpenCV, OpenBLAS | O |
| Models shipped **encrypted** (`*.enc`): `Image_Tagging_v1` (228 MB), `Similarity_v1` (198 MB), `Face_Detection_v1`, `Face_Detection_Desc_v1`, `Face_Alignment_v1`, `Face_Recognition_v1`, `AIKeywordsMain`, `AIKeywordsTranslator` | O |
| Similarity search = embeddings indexed with **FAISS** (`IndexFlat`, `Clustering` RTTI) | O |
| Face pipeline: detect → align → embed → cluster (`FaceClustering`) → auto-name by distance (`Face assignment-- Name: %s distance: %f`) | O (log strings) |
| "AI" one-click looks (Sky, Subject, Skin, Background × Brighten/Darken/Blur/Pop…) are **ordinary develop presets** (`Type: PRESET_DEVELOP_GLOBAL`) whose TuningBrush/DetailBrush has `Type=2` and a mask payload `<command><name>RLE</name><int id="AIType">…` (sky = 3) | O |
| So: AI only **produces masks**; the edit itself is the standard local-adjustment engine | I |
| "AI Denoise" exists as a separate feature | O |

## 7. GPU rendering

An embedded OpenCL program (`OPENCL_SOURCE` 8204) holds RAW demosaic kernels:
`InterpolateGreen`, `InterpolateGreenLM`, `InterpolateRedAndBlueAtGreen`,
`InterpolateRedAtBlueBlueAtRed`, `EnhanceGreen`, `Median`, `MedianLM`,
`InterpolateBorder`, `ColorBalance` [O]. → Green-first demosaic with median
artifact suppression, GPU-accelerated [I].

## 8. Search, UI and modes

* Saved/advanced search = JSON `{AdvancedSearch:{Criteria:{Groups:[{Elements:[{DateTime:{Comparison:"WithinLastNWeeks", CriteriaName:"EXIF_DATETIMEORIGINAL", Val1:1}}]}]}}}` — groups of typed criteria with comparison operators [O].
* Modes: Manage, Media, View, Develop, Edit (+ People, Dashboard, 365 cloud) [O/I].
* Panes: Folders, Catalog (categories/keywords/ratings/labels), Calendar, Shortcuts, Selective Browsing, Search, Properties, Preview, Filter/Group/Sort [O].
* Theme = JSON palette keyed by widget (`Dark.Colors.Breadcrumb.Background = #2D2D2D`, selection `#56749B`) [O].

## 9. Unknowns

* [U] Develop pipeline **execution order** and the exact maths of each
  operator (needs function-level decompilation of the 63 MB exe; not done —
  Ghidra is not installed here and a full auto-analysis of this binary is
  multi-hour).
* [U] Field layouts of each `.dbf` table (needs a real catalog or the
  CodeBase field definitions in code).
* [U] Exact XMP serialization of `acdsee:dpp`.
* [U] Model architectures/inputs (models are encrypted; not decrypted by
  design).

---

## 10. What Prism Studio adopts (as ideas, re-implemented)

| ACDSee idea | Prism Studio implementation |
|---|---|
| Catalog + sidecars | SQLite catalog (`rusqlite`) + standard XMP sidecars (`xmp:Rating`, `xmp:Label`, `dc:subject`, own `prism:` namespace) |
| Thumbnails in catalog | Disk thumbnail cache keyed by BLAKE3(path, mtime, size) |
| Plugin decoders | Rust `image` crate + `rawloader` behind one `imaging::load` entry point |
| Flat versioned develop bag | Typed, versioned `DevelopSettings` (serde JSON), every section has `enabled` |
| 8-band colour model | 8-band HSL "Color Mixer" with smooth band weights |
| Local adjustments + pixel targeting | `LocalAdjustment` masks: linear, radial, sky (heuristic) with luma range targeting |
| AI looks = mask + normal adjustments | Same separation: mask providers are pluggable, edit engine is shared |
| FAISS similarity | 64-bit perceptual dHash + Hamming distance (v1); embedding index is a v2 slot |
| Advanced search groups | `SearchQuery` struct (rating, label, keyword, date, camera, text) + saved searches |
