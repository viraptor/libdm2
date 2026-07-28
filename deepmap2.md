# vImageDeepmap2 Format & API

Private/undocumented image buffer compression in Apple's vImage (Accelerate framework).
Reverse-engineered from binary analysis and black-box testing.

## Symbols

Found in `vImage.framework/vImage` (resolvable via `dlsym(RTLD_DEFAULT, ...)` when linked
with `-framework Accelerate`):

```
vImageDeepmap2Encode
vImageDeepmap2Decode
vImageDeepmap2PixelSize
vImageDeepmap2DecodeScratchBufferSize
vImageDeepmap2DecodeStreamCreate
vImageDeepmap2DecodeStreamProcess
vImageDeepmap2DecodeStreamRelease
vImageDeepmap2DecodeStreamScratchBufferSize
vImageDeepmap2EncodeCreateBuffer
```

There is also an older `vImageDeepmap*` (v1) family with magic `dmpa`.

## Function Signatures

```c
// Encode image buffer into deepmap2 format.
// Returns: encoded byte count, or 0 on failure.
size_t vImageDeepmap2Encode(
    vImage_Buffer *src,        // standard vImage_Buffer {data, height, width, rowBytes}
    uint32_t pixelFormat,      // pixel format (see below)
    Deepmap2Options *opts,     // {compressionType, quality, param}
    void *outBuf,              // caller-allocated output buffer
    size_t outSize             // output buffer capacity
);

// Decode deepmap2 data into image buffer.
// Returns: non-NULL on success, NULL on failure.
void *vImageDeepmap2Decode(
    vImage_Buffer *dst,        // destination buffer (data, height, width, rowBytes must be set)
    uint32_t pixelFormat,      // must match what was encoded
    void *encData,             // encoded data
    size_t encSize,            // encoded data size
    void *scratch              // scratch buffer, or NULL to auto-allocate
);

// Auto-allocate output buffer and encode, trying multiple compression
// methods (lossless, palette if fmt=4, default) and picking the smallest.
// Sets opts->compressionType to the method actually used.
// Returns: encoded byte count, or 0 on failure. *outBuf receives malloc'd buffer.
size_t vImageDeepmap2EncodeCreateBuffer(
    vImage_Buffer *src,
    uint32_t pixelFormat,
    Deepmap2Options *opts,
    void **outBuf              // receives malloc'd buffer (caller must free)
);

// Returns a conservative scratch buffer size for decode.
size_t vImageDeepmap2DecodeScratchBufferSize(void);

// Returns pixel size in bytes for a given format, or 0 if invalid.
uint32_t vImageDeepmap2PixelSize(uint32_t pixelFormat);
```

## Options Struct

```c
typedef struct {
    uint32_t compressionType;  // 1=none, 2=default, 3=lossless, 4=palette
    uint32_t quality;          // mapped to header byte 5; only 0 or 1 accepted for types 2/3
    uint32_t param;            // mapped to header byte 6; no effect on 8-bit format output
} Deepmap2Options;
```

- **quality**: Only values 0 and 1 are accepted for compression types 2 and 3 (≥2 returns failure).
  For type 1 (none), all values 0–255 are stored but have no effect. quality=0 is always lossless.
  quality=1 with type 2 introduces maxerr=1 on R/G channels for RGB and RGBA
  (half-scale chroma storage; gray and gray+A have no chroma and stay lossless).
- **param**: Has no effect on 8-bit format encoded output (all values produce identical data).
  For 16-bit formats (0x11–0x14), `param` must be in range 9–12 (0x09–0x0c); all other values rejected.
- `EncodeCreateBuffer` overrides `compressionType` — it tries 3, then 4 (if fmt=4), then 2, and picks the smallest.

## Pixel Formats

| Format | Channels | Bits/channel | Pixel size (bytes) |
|--------|----------|--------------|--------------------|
| 1      | 1 (gray) | 8           | 1                  |
| 2      | 2 (gray+A) | 8        | 2                  |
| 3      | 3 (RGB) | 8           | 3                  |
| 4      | 4 (RGBA) | 8          | 4                  |
| 0x11   | 1 (gray) | 16         | 2                  |
| 0x12   | 2 (gray+A) | 16      | 4                  |
| 0x13   | 3 (RGB) | 16          | 6                  |
| 0x14   | 4 (RGBA) | 16         | 8                  |

Formats 5–16 (0x05–0x10) return pixel size 0 and are rejected.

## On-Disk Header

Minimum 12 bytes:

```
Offset  Size  Field
0       4     Magic: "dmp2" (0x64 0x6d 0x70 0x32)
4       1     compressionType
5       1     quality
6       1     param
7       1     pixelFormat
8       2     tileWidth  (uint16 LE)
10      2     tileHeight (uint16 LE)
```

For compression type 4 (palette), the header is extended:

```
Offset  Size            Field
0       4               Magic: "dmp2"
4       1               compressionType = 4
5       1               quality
6       1               param
7       1               pixelFormat = 4 (always RGBA)
8       2               tileWidth  (uint16 LE)
10      2               tileHeight (uint16 LE)
12      2               paletteCount (uint16 LE, 1–256)
14      2               bytesPerEntry (uint16 LE, 3 or 4)
16      count×4         Palette entries (always 4 bytes each in storage)
16+count×4              Tile data begins
```

When `bytesPerEntry=4`, each palette entry is RGBA (4 significant bytes).
When `bytesPerEntry=3`, each palette entry is RGB (3 significant bytes, 4th byte = 0);
alpha is stored per-pixel in the tile data instead of in the palette.
In both cases, storage is always 4 bytes per entry.

## Compression Types

| Type | Name     | Notes |
|------|----------|-------|
| 1    | None     | Raw pixel copy. No tiling. Encoded size = 12 + width×height×pixelSize. |
| 2    | Default  | Color transform + prediction + LZFSE. Best ratio on structured data. Tiled (~1 MB). |
| 3    | Lossless | LZFSE on raw pixels. Guaranteed lossless. Tiled (~2 MB). |
| 4    | Palette  | Only valid for pixelFormat=4 (RGBA 8-bit). Builds palette, then tiled encode. |

## Compression Type 1 (None)

```
[12-byte header][raw pixel data, tightly packed]
```

**No tiling**: Tile dimensions = image dimensions. No tile index or per-tile headers.

**Encoded size**: Always exactly `12 + width × height × pixelSize`.

**Row padding is stripped**: If the source `vImage_Buffer` has `rowBytes > width × pixelSize`,
only the active pixel data is written.

## Compression Type 3 (Lossless)

Raw pixel data compressed with **LZFSE** (`COMPRESSION_LZFSE`). No pre-filter or transform.

### Compression backend

For tiles ≥ 4096 raw bytes, the LZFSE stream uses `bvx2` blocks (magic `62 76 78 32`).
For smaller tiles, raw LZVN encoding is used directly (no container header).
The deepmap2 decoder rejects `bvxn` (LZVN-in-container) blocks even though
the standard LZFSE format permits them.
The LZVN end-of-stream marker must be 8 bytes: `0x06` followed by 7 zero bytes.

### Tile layout

After the 12-byte header, tiles are stored sequentially:

```
[12-byte header]
[tile 0: uint32_le compressed_size | compressed_data[compressed_size]]
[tile 1: uint32_le compressed_size | compressed_data[compressed_size]]
...
```

Each tile is a horizontal strip of `tileHeight` rows (last tile may have fewer).

### Tiling threshold

Raw tile data capped at ~2 MB (2,097,152 bytes). Tile width = image width.

### Notes

- Output may exceed raw size for incompressible data.
- Quality field stored in header but has no effect on output.
- Minimum image size: approximately ≥9 pixels.

## Compression Type 2 (Default)

Uses a **color transform → adaptive prediction → zigzag → byte-plane split → LZFSE** pipeline.
Tiles use the same `[uint32_le size][compressed_data]` framing as type 3.
Tiling threshold: ~1 MB (1,044,480 bytes) based on raw pixel data.

### Intermediate buffer

Each tile decompresses to an intermediate buffer. The buffer size is:

```
buf_size = round_up_16(H × (K × W + 1))
```

where `round_up_16(x) = (x + 15) & ~15` (padded to 16-byte alignment), and K is the
number of byte-planes:

| Format     | Channels | K (byte-planes) | Buffer per tile (before padding) |
|------------|----------|-----------------|----------------------------------|
| 1 (gray)   | 1        | 2               | H × (2W + 1)                    |
| 2 (gray+A) | 2        | 3               | H × (3W + 1)                    |
| 3 (RGB)    | 3        | 6               | H × (6W + 1)                    |
| 4 (RGBA)   | 4        | 7               | H × (7W + 1)                    |

### Buffer layout

The decompressed intermediate buffer layout differs between gray (1-channel) and
multi-channel (2+ channels) formats.

#### Gray (1-channel) layout

```
[H mode bytes][H×W high bytes][H×W low bytes]
```

- `buf[0..H]`: one prediction mode byte per row (values 0–4)
- `buf[H..H+W*H]`: high bytes of zigzag-encoded residuals
- `buf[H+W*H..H+2*W*H]`: low bytes of zigzag-encoded residuals

#### Multi-channel layout (RGB, RGBA, GrayA)

For formats with an alpha channel (GrayA, RGBA):

```
[W×H alpha plane][H YCC mode bytes][n_color×W×H high bytes (interleaved)][n_color×W×H low bytes (interleaved)]
```

For formats without alpha (RGB):

```
[H YCC mode bytes][n_color×W×H high bytes (interleaved)][n_color×W×H low bytes (interleaved)]
```

Where `n_color` = 1 for GrayA, 3 for RGB/RGBA.

**Alpha plane**: Raw u8 alpha values in row-major order, `W×H` bytes total.
No prediction is applied to alpha values.

**YCC mode bytes**: One byte per row, values 0–4. These control the prediction mode
applied to the Y/Co/Cg (or Y for GrayA) channels.

**High/low byte planes**: For each pixel position, the color channels are interleaved.
For RGB/RGBA, each pixel contributes 3 values (Y, Co, Cg) to both the high and low planes.
The high plane stores `zigzag >> 8`, the low plane stores `zigzag & 0xFF`.

### Color transform (multi-channel)

Uses standard YCoCg (Reversible Color Transform) with truncation-toward-zero division:

```
Forward (RGB → YCoCg):
  Co = R - B
  t  = B + Co / 2          (truncation toward zero, not >>1)
  Cg = G - t
  Y  = t + Cg / 2

Inverse (YCoCg → RGB):
  t = Y - Cg / 2
  G = Cg + t
  B = t - Co / 2
  R = Co + B
```

Division by 2 uses truncation toward zero (`x / 2`), which differs from arithmetic
right shift for negative odd numbers: `-3 / 2 = -1` vs `-3 >> 1 = -2`.

There is no separate "color decrement" on Co/Cg. The negative adjustment is applied
uniformly to all residuals as described below.

### Prediction modes

| Mode | Name     | Prediction at position i                         |
|------|----------|--------------------------------------------------|
| 0    | None     | pred = 0 (raw values stored directly)            |
| 1    | UpLeft   | 2-way Paeth: select between Up and Left          |
| 2    | Left     | pred = value[i-1]; i=0: pred = 0                |
| 3    | Up       | pred = prev_row[i]                               |
| 4    | Mean     | pred = (left + up + 1) / 2 with truncation fix  |

**Mode selection heuristic**: The encoder computes the sum of absolute residuals (L1 cost)
for each candidate mode and picks the minimum.

- Row 0: only modes 0 and 2 are candidates.
- Rows 1+: modes 0, 2, and 3 are candidates for gray. Multi-channel uses all 5 modes.

**2-way Paeth (mode 1)**: At position `i > 0`:
```
p  = up[i] + left[i] - up_left[i]
pa = abs(p - left[i])
pb = abs(p - up[i])
pred = (pb < pa) ? up[i] : left[i]     // prefer left on tie
```
At position `i = 0`: `pred = up[0]` (falls back to Up prediction).

**Mean (mode 4)**: At position `i > 0`:
```
sum = left[i] + up[i] + 1
if sum < 0: sum += 1       // truncation-toward-zero correction
pred = sum >> 1
```
At position `i = 0`: `pred = up[0]`.

### Negative residual adjustment

After computing the prediction residual (`value - predicted`), the encoder decrements
**all negative values by 1** across all channels:

```
if residual < 0: residual -= 1
```

The decoder reverses this: `if residual < 0: residual += 1`.

This applies uniformly to Y, Co, Cg, and gray channels. It improves zigzag coding
efficiency by shifting the distribution of negative residuals.

For the gray encoder, the adjustment is applied to all modes except None (mode 0).
For the multi-channel encoder, the adjustment is applied to all values unconditionally
(including mode 0 base values). In the multi-channel case, this effectively serves as
the "color decrement" for Co/Cg — there is no separate per-channel adjustment in the
color transform itself.

### Zigzag encoding

Signed int16 residuals are mapped to unsigned int16:

```
zigzag_encode(x) = x >= 0 ? 2*x : -2*x - 1
zigzag_decode(z) = (z >> 1) ^ -(z & 1)
```

### Previous-row tracking

For prediction modes that reference the previous row (Up, UpLeft, Mean), the encoder
and decoder track the **reconstructed values** from the previous row as the reference.

- **Gray**: Previous row values are the actual pixel values (u8, 0–255).
- **Multi-channel**: Previous row values are the YCoCg values after zigzag decode +
  un-adjustment. These are the original (un-decremented) color transform outputs.

### Quality and param

- quality=0: Lossless for all 8-bit formats. Co/Cg are stored at FULL scale.
- quality=1: Lossless for gray and gray+A (no chroma planes). For RGB and RGBA,
  Co/Cg are stored at HALF scale and the decoder doubles them — this halving is
  exactly the maxerr=1 on R/G channels (measured on both RGB and RGBA; an
  earlier note claiming RGB stayed lossless at quality=1 was wrong).
- quality ≥ 2: Encoder returns failure.
- param: Stored in header, no effect on 8-bit output (verified: q1/p0 and q1/p10
  encode byte-identically; the chroma-scale switch is the QUALITY byte, not param).
  For 16-bit formats param selects the fixed-point scale (see below).

### 16-bit formats (RGBA16 reverse-engineered)

16-bit pixels are u16 per channel; in real `.car` renditions (csiheader
pixelFormat `'RGBW'`) the u16s are **IEEE half-float bit patterns**
(little-endian), which is why values like 0x3C00 (1.0h) and sign-bit
patterns ≥ 0x8000 (negative extended-range colors) appear.

**Type 2 for RGBA16 (0x14)** uses the SAME intermediate tile layout as RGBA8 —
K=7 byte-planes: `[W×H alpha][H mode bytes][3×W×H high][3×W×H low]` — with the
same prediction modes, zigzag, negative-residual adjustment, and half-scale
chroma (quality ≠ 0). Only the pixel⇄integer mapping differs:

- **Color channels**: the reconstructed YCoCg→RGB integers are *fixed-point codes*
  of the half-float channel values: `value = code / 2^(param-1)`. `param` must be
  9–12 (real renditions use 10 → scale 512). The decoder emits
  `f16(code / 2^(param-1))` per channel, rounding to nearest-even.
- **Alpha**: stored as a plain 8-bit plane (like RGBA8); the decoder expands it to
  `f16(a8 / 255)`.

**Wrapping arithmetic**: the decoder's prediction accumulation and inverse
YCoCg transform operate in 16-bit lanes that WRAP (two's complement). The Mean
predictor computes `left + up + 1` wrapped at i16 first, then applies the
negative truncation fix and the shift. Garbage half inputs (NaN/Inf/2^13-scale)
make Apple's ENCODER wrap the fixed-point codes at i16, which exposes these
semantics; matching them fixes the observable sign-flip pixels. One corner is
intentionally left unchased: quality=0 param=10..12 streams built from such
garbage inputs still diverge on a handful of wrapped pixels (Apple's own
encode of those inputs is already total value corruption, and every observed
real rendition is quality=1 param=10 with |v| ≲ 1).

Verified byte-identical to `vImageDeepmap2Decode` on Apple-encoded fixtures at
every param 9–12 and quality 0/1 (`tests/data/rgba16_*`), and on random
valid-half images (|v| < 4) across the full param × quality grid.

Encoding is inherently **lossy** (float → fixed-point quantization; quality=0 still
shows ±half-ulp rounding, quality=1 substantially more). An earlier note here
claimed Apple's 16-bit type-2 round-trip was "catastrophically broken" — that
misread the quantization loss measured on raw bit patterns (a small float error
across a half-float exponent boundary produces a huge bit-pattern delta). Apple's
decoder is deterministic and correct for these streams.

The tiling budget also differs for 16-bit type 2: 1024-wide RGBA16 uses
tileHeight 291 ≈ 1,044,480 / (W × K/2) — i.e. the budget appears to be computed
on intermediate-buffer bytes, not the 8-byte raw pixels. Decoders need not care
(tileHeight is in the header); only an encoder implementation would.

Gray16 (0x11), GrayA16 (0x12), and RGB16 (0x13) type 2 remain un-reverse-engineered
(no known real-world samples); libdm2 rejects them. Type 1 (None) fails to encode
16-bit data in Apple's implementation; type 3 (Lossless) is format-agnostic and
works for all 16-bit formats.

## Compression Type 4 (Palette)

Palette compression is **only valid for pixelFormat=4 (RGBA 8-bit)**.

### Palette header

```
[12-byte header (comprType=4, pixFmt=4)]
[uint16 paletteCount]
[uint16 bytesPerEntry]       — 3 (RGB) or 4 (RGBA)
[paletteCount × 4 bytes]    — entries always stored as 4 bytes each
[tile data...]
```

- `bytesPerEntry=4`: Each entry contains RGBA. The 4th byte is the alpha value.
  Tile data contains palette indices only.
- `bytesPerEntry=3`: Each entry contains RGB (4th byte = 0).
  Tile data contains **both** alpha values and palette indices.

Apple's encoder selects bpe=3 when different pixels share the same RGB but have different
alpha values (avoiding duplicate RGB entries). It selects bpe=4 when all alpha values for
each RGB combination are identical.

### Tile data

**When `bpe=4`**: The tile decompresses to `round_up_16(npix)` bytes of palette indices.

**When `bpe=3`**: The tile decompresses to `round_up_16(2 × npix)` bytes:

```
[npix alpha bytes][npix index bytes]
```

Alpha values come first, followed by palette indices. Each pixel's final RGBA is
reconstructed as `(palette[index].R, palette[index].G, palette[index].B, alpha[i])`.

### Palette entry ordering

Entries are **not** in input order. The builder reorders them, likely to improve
compression of the index stream.

### Notes

- Maximum 256 palette entries. Images with >256 distinct RGBA values cannot be palette-encoded.
- Always lossless.
- Quality and param fields stored in header, no effect on output.
- Tiling uses the ~1 MB budget based on the index size (1 byte/pixel).

## Tiling

Tiling is only relevant for compressed formats; type 1 never tiles.

Tile width always equals image width (full-width horizontal strips). Tile height is capped
so raw tile data stays within a budget:

| Compression | Raw tile budget |
|-------------|-----------------|
| 2 (default) | 1,044,480 bytes |
| 3 (lossless)| 2,097,152 bytes |
| 4 (palette) | 1,044,480 bytes |

All compressed types use sequential tile layout:
```
[uint32_le size][compressed_data] [uint32_le size][compressed_data] ...
```

The last tile may have fewer rows than `tileHeight`.

## LZVN End-of-Stream

The LZVN encoder must emit an 8-byte end-of-stream marker: `0x06` followed by 7 zero bytes.
Apple's decoder requires this; a shorter EOS (e.g., just `0x06`) causes decode failures.

When the LZVN decoder fills its output buffer completely, it stops without error (truncation
is expected behavior for the type 2 intermediate buffer).

## Minimum Image Size

Apple's encoder returns failure (size 0) for 1×1 images across all compression types.
Images of approximately ≥9 pixels (e.g. 3×3, 9×1) succeed.

## Internal Functions (not exported, visible in symbols)

- `DeepmapImageHeaderRead` / `DeepmapImageHeaderWrite`
- `ComputeTileSize`
- `EncodeImageWithMehtod` (sic — typo in the binary)
- `DecodeTiledImage`
- `Deepmap2DecodeNone`
- `Deepmap2BuildPalette`
- `Deepmap2Decode{Default,Lossless,Palette}ScratchBufferSize`
- `_RowEncodeY00` — gray identity transform
- `_RowEncodeYCC` — multi-channel YCoCg transform
- `_DeepmapPredict*` — prediction mode implementations
