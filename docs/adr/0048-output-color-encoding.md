# ADR 0048 — Output color encoding

- Status: Accepted
- Date: 2026-10-24

## Context

Rendering §5.4 fixes the working space as linear light with premultiplied
alpha. It also requires the sRGB transfer function to run exactly once on the
way to an sRGB surface: in hardware through an `*Srgb` format, or else in the
final shader.

The pipeline did neither:

- Public colors entered as linear light (`into_linear_straight`).
- Every surface was a plain unorm format, and no shader encoded its output.
  The display read linear values as sRGB-encoded ones, so every solid color
  showed darker than authored: `#1565c0` reached the screen as `#022186`.
- Image texels were stored sRGB-encoded and sampled raw, so images happened to
  look right. They mixed with linear colors in the same blend.
- The 3D scene lane decoded its colors with `pow(2.2)` and re-encoded its
  output with `pow(1/2.2)`. It worked against the 2D lane, not with it.

## Decision

- `TextureFormat` gains the sRGB variant of each 8-bit color format:
  - `Rgba8UnormSrgb` and `Bgra8UnormSrgb`;
  - `Bc1RgbaUnormSrgb`, `Bc3RgbaUnormSrgb` and `Bc7RgbaUnormSrgb`;
  - `Etc2Rgb8UnormSrgb`, `Etc2Rgb8A1UnormSrgb` and `Etc2Rgba8UnormSrgb`;
  - `Astc4x4UnormSrgb`, `Astc6x6UnormSrgb` and `Astc8x8UnormSrgb`.

  A texel of one of these formats holds sRGB-encoded color. Sampling decodes
  it to linear light, rendering into it encodes, and blending into it happens
  in linear light.

  `TextureFormat::srgb` names the variant, `linear` strips it, and `is_srgb`
  tells them apart. Storage and block layout are those of the linear twin.
  Converted assets keep storing the linear twin; the variant is chosen when
  the texture is created.
- A surface in `ColorSpace::Srgb` reports the sRGB variant as its format.
  Every built-in pipeline, and every intermediate the renderer plans (whose
  format follows the surface's), targets it. The encode then happens once, in
  hardware.

  Each backend maps the formats as follows:

  | Backend | How the sRGB surface is made |
  |---|---|
  | Metal | The layer's `BGRA8Unorm_sRGB` |
  | Vulkan | An `SRGB` swapchain format |
  | D3D12 | An `_SRGB` render-target view of the flip-model buffer |
  | WebGPU | The canvas configured with its `-srgb` view format, drawn through such a view |
  | GLES | An 8-bit window in the sRGB colorspace (`EGL_KHR_gl_colorspace`); `SRGB8_ALPHA8` textures |
  | CPU raster | Software, as below |

  Wide-gamut and HDR targets are unchanged. A backend whose surface offers no
  sRGB format (a GLES 565 panel, an EGL without the colorspace extension)
  reports the linear one and draws it unencoded, as before.
- Color textures are sRGB: images, the color-glyph atlas, a 3D scene's target
  and the gradient LUT. The LUT bakes encoded stops, which also gains dark-tone
  precision over storing linear values in 8 bits.

  Data textures stay linear: A8 glyph coverage, the MTSDF field, clip and mask
  pages, and every float target.
- An sRGB texel is premultiplied in linear light: `enc(dec(c) · a)`. This
  applies to image decode and to the build-time converter alike, and an image
  is resampled in linear light. An opaque texel is unchanged.
- The 16-bit packed formats (`Rgb565Unorm`, `Rgba4444Unorm`, `Rgba5551Unorm`)
  have no hardware sRGB variant:
  - GPU backends do not sample them for color. The asset worker decodes such
    an image to `Rgba8UnormSrgb`.
  - The CPU raster samples them and decodes them in software.

  A 565 surface is sRGB-encoded.
- The CPU raster keeps the same contract in software:
  - an sRGB target or texture decodes through a 256-entry table on load and
    sample;
  - it encodes through a 4096-entry table on store;
  - source-over runs in linear light.

  An opaque span that replaces its destination only encodes.
- Glyph coverage is adjusted by the ink's linear luminance `l`, so text keeps
  the weight it had when blended in encoded space (`dec` is the sRGB decode):

  ```text
  cov' = (1 − l) · (1 − dec(1 − cov)) + l · dec(cov)
  ```

  For a black ink on white, this makes the displayed result equal the
  encoded-space blend exactly, and likewise for a white ink on black. It
  applies to A8 glyphs, MTSDF glyphs and the CPU raster's glyph lane.
- The 3D scene lane takes its colors linear and returns linear premultiplied
  color. Its sRGB target encodes, and the 2D lane decodes it when it samples
  the target.

## Consequences

- Authored colors show as authored on every backend. A 50% blend of two
  colors is their linear-light midpoint, as §5.4 requires, so translucent
  overlays and image edges blend physically.
- Every headless snapshot golden is blessed again once.
- On the CPU raster, a translucent pixel costs a decode and an encode more,
  and an opaque one costs only the encode. A pixel drawn as its neighbour
  was (the same source over the same destination) reuses its result, so a
  fill pays once per run. Measured on the 800×480 panel bench (5% damage,
  one thread, `-Oz`): XRGB8888 linear 242 µs, sRGB 204 µs; RGB565 318 µs
  before, 279 µs after.
- A packed image on a GPU backend takes `Rgba8` memory.
