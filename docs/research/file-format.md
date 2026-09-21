<!-- Asset file format exploration — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../PLAN.md -->

Empirical results are in. Composing the digest.

**FORMAT DECISION DIGEST — sleepytime precomputed asset ("SLPY" container)**

**Recommendation: (1) custom RIFF-style chunked container, temporal byte-delta pre-pass + per-frame zstd blocks + explicit frame index. Fallback: (5) SQLite as container (same blob payloads inside).**

---

**Empirical basis** (measured on this machine: synthetic-but-plausible 480x270 planes — smooth luma+blob w/ noise, 4.1%-dense edge plane, sparse highlight, mid-freq detail; per-frame *independent* compression, CLI zstd/lz4):

| filter | codec | avg B/frame | ratio | decode speed |
|---|---|---|---|---|
| intra | zstd -3 | 166,540 | 3.1x | ~1.0 GB/s |
| intra | zstd -19 | 126,898 | 4.1x | ~1.1 GB/s |
| intra | lz4 -1 | 247,646 | 2.1x | ~5.1 GB/s |
| temporal delta | zstd -3 | 36,466 | 14.2x | ~3.0 GB/s |
| temporal delta | zstd -19 | 20,897 | **24.8x** | ~3.6 GB/s |
| temporal delta | lz4 -9 | 33,072 | 15.7x | ~5 GB/s |

- Temporal byte-delta (`cur[i]-prev[i] mod 256`) multiplies zstd ratio ~4-6x. It is the single highest-leverage design choice.
- Decode budget: 518,400 B frame at 3.0-3.6 GB/s = **0.15-0.17 ms** + un-delta add (memory-bound, ~0.05 ms SIMD) → ~0.2 ms/frame, 5x under the 1 ms budget. Even intra zstd = ~0.5 ms. LZ4 unnecessary; zstd wins on size at equal budget-compliance. Keep a codec-ID byte anyway.
- zstd level: compress at -19 in the factory (offline, decode speed unaffected — actually faster on smaller input). Real-world planes are noisier than synthetic; assume 8-15x realistic.

**Size arithmetic (3 min, 480x270, 4x 8-bit planes, 30 fps):**
- Frame raw: 480 x 270 x 4 = 518,400 B. Frames: 180 x 30 = 5,400. Raw total: 518,400 x 5,400 = **2.80 GB**.
- Keyframe every 60 frames (2 s): 90 intra x 126,898 + 5,310 delta x 20,897 = 11.4 MB + 111.0 MB ≈ **122 MB** (measured-synthetic); conservative real-world ≈ **190-350 MB** (8-15x). At 24 fps: x0.8 → ~98 MB synthetic / 150-280 MB real.
- Frame index: 5,400 x 16 B = 86 KB (negligible). If size matters more, keyframe interval 120 or per-scene keyframes changes little (deltas dominate).

---

**Requirements vs candidates** (✓ good / ~ workable / ✗ fails):

| Candidate | <1ms O(frame) decode | seek | mmap | dep footprint | versioning | compression | metadata | streaming factory write | verdict |
|---|---|---|---|---|---|---|---|---|---|
| (1) custom chunked + zstd + index | ✓ 0.2ms | ✓ O(1) via index | ✓ trivial | ✓ libzstd only (~300KB, BSD, vendorable) | ✓ you own it | ✓ 14-25x | ✓ META chunk | ✓ index-at-end | **RECOMMEND** |
| (2) mp4/mkv + ffmpeg | ~ | ~ frame-accurate seek fiddly | ✗ | ✗ ffmpeg = tens of MB, ABI churn | ~ | ✗ lossy codecs corrupt feature planes; chroma subsampling wrecks planes packed as U/V; lossless (FFV1) heavy | ~ side-data hacks | ✓ | reject: violates tiny-dep + fidelity |
| (3) npz | ✗ whole-array or per-frame zip entries w/ DEFLATE (~2-3x, slow) | ~ | ✗ compressed entries | ✗ numpy semantics in C/Rust runtime | ✗ | ✗ | ✗ | ~ | factory-internal debug only |
| (4) flatbuffers/capnproto | ~ | ~ | ✓ | ~ codegen + lib for what is ~40 lines of struct packing | ✓ schema evolution | ✗ still need zstd+index yourself | ✓ | ~ | reject: solves the wrong problem |
| (5) sqlite (frames table, blob/frame) | ✓ SELECT+zstd well under 1ms | ✓ PK index free | ~ can mmap DB, blobs not contiguous | ~ amalgamation ~250-800KB, zero-install, ubiquitous | ✓ user_version pragma | ✓ same blobs | ✓ meta table | ✓ transactional append | **FALLBACK** |
| (6) msgpack/CBOR stream | ~ | ✗ no random access w/o bolt-on index | ✗ varint layout | ✓ | ~ | ✗ bolt-on | ✓ | ✓ | reject as container; use CBOR *inside* META |
| (7) QOI-style delta/RLE pre-pass | — filter, not container — | | | | | ✓ adopt temporal delta; skip spatial RLE (zstd already catches runs) | | | **adopt delta into (1)** |

**Rationale tied to brief:** (a) tiny deps → only libzstd, statically linkable, exists on every target incl. musl/BSD/Windows; (b) O(frame) decode → per-frame blocks, playback decodes exactly one block + one memadd; (c) seek → index gives O(1) block lookup; worst case decode keyframe + ≤59 deltas ≈ 12 ms — fine for interactive scrub, playback unaffected; (d) mmap → file is offset-addressable, decompress straight out of mapped pages, index itself is a flat mmap-able array; (e) resolution independence → planes stored at base analysis resolution, runtime samples; (f) 80/20 → whole reader is ~300 lines of C/Rust, no codegen, no schema compiler; (g) factory iteration → deterministic bytes, per-chunk CRC32 for golden tests, trailer magic detects truncation.

---

**Byte layout sketch ("SLPY" v1, all integers little-endian):**

```
HEADER (64 B, fixed):
  0  magic          "SLPY"            4B
  4  version_major  u16               reader MUST reject if > supported
  6  version_minor  u16               reader MUST accept any (additive changes only)
  8  header_size    u32               = 64; future minors may grow, skip to this offset
 12  flags          u32               bit0: index present; bit1: CRCs present
 16  fps_num/fps_den u16/u16          e.g. 30000/1001
 20  base_w, base_h u16/u16           analysis resolution, e.g. 480,270
 24  aspect_num/den u16/u16           display aspect, e.g. 16,9
 28  frame_count    u32
 32  plane_count    u8                1..8
 33  codec          u8                0=raw 1=lz4 2=zstd
 34  filter         u8                0=intra-only 1=temporal-delta
 35  keyframe_ivl   u8                frames between keyframes (0=all intra)
 36  plane_ids[8]   u8 x8             semantic tags: 1=luma 2=edge 3=highlight 4=detail...
 44  index_offset   u64               patched at close (factory streams frames first)
 52  meta_offset    u64
 60  reserved       u32

CHUNKS (RIFF-style, after header, each: tag u32(FourCC) | flags u8 | pad u24 | size u64 | payload | [crc32 u32]):
  "META"  CBOR map: ramp hints, glyph-palette suggestions, per-plane gamma,
          source provenance, factory version. CBOR = extensible w/o format rev;
          unknown keys ignored. (~1-4 KB)
  "NORM"  per-scene normalization: u32 scene_count, then per scene:
          {first_frame u32, per-plane min/max/percentile u8 x N}. Flat struct, mmap-read.
  "FRAM"  one per frame, streamed in order. Payload:
          frame_idx u32 | flags u8 (bit0=keyframe) | plane_subblocks:
          [plane_id u8 | comp_size u32 | raw_size u32 | zstd_data...] x plane_count
          (per-plane blocks → runtime can skip planes a low-tier backend doesn't use)
  "FIDX"  frame index, written LAST: frame_count entries x 16 B:
          {file_offset u64, comp_size u32, flags u8 (keyframe), pad u24}
          → 86 KB for 5,400 frames; load or mmap whole.
  "TRLR"  8 B: "SLPY_END" — absence ⇒ truncated file ⇒ factory rerun.
UNKNOWN CHUNKS: reader skips via size; if chunk flags bit0 ("required") set and tag
unknown → hard error. This + major/minor is the entire forward-compat contract.
```

**Implementation notes:**
- Seek: binary-search FIDX flags backward to nearest keyframe, decode forward; playback: decode next FRAM, add delta into persistent prev-frame buffer (double-buffer, zero alloc/frame).
- Consider `ZSTD_compress2` with a dictionary trained per-video on keyframes if intra size ever matters (skip for v1 — 80/20).
- SQLite fallback schema if the custom container's index-patch/truncation handling ever gets annoying: `meta(k,v)`, `scenes(...)`, `frames(idx INTEGER PRIMARY KEY, flags INT, data BLOB)` — identical blob payloads (delta+zstd), `PRAGMA user_version` for versioning, `mmap_size` pragma for read path; costs ~1-2% size overhead and the amalgamation dependency, buys crash-safe factory appends and free tooling (`sqlite3` CLI inspection during build-test-improve loops).
- Do NOT store per-cell glyph decisions in the asset (violates brief req 4); planes only, runtime samples grid.
- Test scripts + measurement data: `/tmp/claude-1000/-home-mckay-personal-sleepytime-ascii/ebee3595-786c-40e1-b7b8-0121ac632844/scratchpad/gen_planes.py` (synthetic plane generator, rerunnable for regression baselines).
