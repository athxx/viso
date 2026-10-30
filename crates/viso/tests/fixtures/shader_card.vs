import viso::math::Vec2F32

shader Ripple {
    instance tint: ColorLinear
    instance rings: F32
    fragment () -> Vec4F32 {
        return Vec4F32(tint.r, tint.g, tint.b, 1.0)
    }
}

component Card {
    state rings: F32 = 8.0
    view {
        Column {
            width: 200dp
            height: 100dp
            ShaderView {
                shader: Ripple { tint: #3366ff, rings: rings }
                width: 120dp
                height: 80dp
            }
        }
    }
}
