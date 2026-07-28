#ifndef DM2_H
#define DM2_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Pixel formats */
#define DM2_GRAY8   1
#define DM2_GRAYA8  2
#define DM2_RGB8    3
#define DM2_RGBA8   4
#define DM2_GRAY16  0x11
#define DM2_GRAYA16 0x12
#define DM2_RGB16   0x13
#define DM2_RGBA16  0x14

/* Compression types */
#define DM2_COMPRESS_AUTO     0
#define DM2_COMPRESS_NONE     1
#define DM2_COMPRESS_DEFAULT  2
#define DM2_COMPRESS_LOSSLESS 3
#define DM2_COMPRESS_PALETTE  4

/* Error codes (negative = error, 0 = success) */
#define DM2_OK               0
#define DM2_ERR_INVALID_ARG -1
#define DM2_ERR_BAD_MAGIC   -2
#define DM2_ERR_BAD_FORMAT  -3
#define DM2_ERR_BUF_SMALL   -4
#define DM2_ERR_ALLOC       -5
#define DM2_ERR_DECODE      -6
#define DM2_ERR_ENCODE      -7
#define DM2_ERR_IO          -8

typedef struct {
    uint32_t width;
    uint32_t height;
    uint32_t format;
} dm2_image_info_t;

/* Encode pixels to deepmap2. Output is allocated; free with dm2_free().
 * Defaults: quality 0; param 10 for 16-bit formats. */
int dm2_encode(const uint8_t *pixels, size_t pixels_len,
               const dm2_image_info_t *info, uint32_t compression,
               uint8_t **out, size_t *out_len);

/* Encode with explicit Deepmap2Options semantics. quality: 0 or 1 (1
 * halves type-2 chroma — Apple's lossy mode). param: the 16-bit
 * fixed-point scale exponent, 9..=12 (required for 16-bit formats;
 * stored but payload-inert for 8-bit). compression must be 1-4. */
int dm2_encode_opts(const uint8_t *pixels, size_t pixels_len,
                    const dm2_image_info_t *info, uint32_t compression,
                    uint32_t quality, uint32_t param,
                    uint8_t **out, size_t *out_len);

/* Decode deepmap2 data into pixel buffer. info is filled on success.
 * Output is the stream's NATIVE depth, matching vImageDeepmap2Decode:
 * 8-bit formats give 1 byte per channel; 16-bit formats give a little-
 * endian uint16 per channel (DM2_RGBA16 .car renditions carry IEEE
 * half-float bit patterns — CoreUI-style 8-bit output is the caller's
 * downconversion, ~ clamp(half,0,1)*255). Size the buffer with
 * dm2_pixel_size(): pixels_len = width * height * pixel_size. */
int dm2_decode(const uint8_t *data, size_t data_len,
               uint8_t *pixels, size_t pixels_len,
               dm2_image_info_t *info);

/* Read header info without decoding. */
int dm2_read_info(const uint8_t *data, size_t data_len,
                  dm2_image_info_t *info);

/* Bytes per pixel for a format, or 0 if invalid. */
uint32_t dm2_pixel_size(uint32_t format);

/* Upper bound on encoded output size. */
size_t dm2_encode_bound(const dm2_image_info_t *info);

/* Free buffer allocated by dm2_encode. */
void dm2_free(uint8_t *ptr, size_t len);

/* File convenience wrappers */
int dm2_encode_file(const uint8_t *pixels, size_t pixels_len,
                    const dm2_image_info_t *info, uint32_t compression,
                    const char *path);

int dm2_decode_file(const char *path,
                    uint8_t *pixels, size_t pixels_len,
                    dm2_image_info_t *info);

#ifdef __cplusplus
}
#endif

#endif /* DM2_H */
