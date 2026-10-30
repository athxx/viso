import viso::math::Vec2F32

// Rings of `tint` spreading out from `center`, `rings` of them across the
// box, fading out toward its edge. Reading `time` keeps the window drawing
// while the ripples are on screen.
shader Ripple {
    instance tint: ColorLinear
    instance rings: F32
    instance center: Vec2F32

    fragment () -> ColorLinear {
        let d = distance(uv, center)
        let wave = 0.5 + 0.5 * cos((d * rings - time * 0.5) * 6.2831855)
        let fade = 1.0 - smoothstep(0.3, 0.5, d)
        let k = wave * fade
        return ColorLinear(tint.r * k, tint.g * k, tint.b * k, 1.0)
    }
}

// An image tiled `tiles` times across the box, bent by a wave running out
// from its middle; the sampler shows each texel as a crisp square and tiles
// past the image's edge.
shader Lens {
    instance tiles: F32
    texture image: Texture2D<Vec4F32>
    @sampler(filter: nearest, address: repeat)
    sampler pixels: Sampler

    fragment () -> Vec4F32 {
        let d = (uv - Vec2F32(0.5, 0.5)) * rect_size
        let bend = 1.0 + 0.08 * sin(length(d) * 0.1 - time * 3.0)
        let at = (Vec2F32(0.5, 0.5) + d * bend / rect_size) * Vec2F32(tiles * 3.0, tiles)
        return sample(image, pixels, at)
    }
}

component Ripples {
    state rings: F32 = 6.0
    view {
        Column {
            width: 288dp
            height: 440dp
            gap: 16dp
            ShaderView {
                shader: Ripple { tint: #3366ff, rings: rings, center: Vec2F32::new(0.5, 0.5) }
                width: 288dp
                height: 288dp
            }
            Slider {
                min: 1.0
                max: 16.0
                bind value <=> rings
                width: 288dp
                height: 24dp
            }
            ShaderView {
                shader: Lens { tiles: 2.0, image: asset("images/checker.png") }
                width: 288dp
                height: 96dp
            }
        }
    }
}
