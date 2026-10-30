import viso::math::Vec2F32

shader Ripple {
    uniform viewport_size: Vec2F32
    instance rect_pos: Vec2F32
    instance rect_size: Vec2F32
    instance tint: ColorLinear
    instance rings: F32
    vertex (vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id)
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        }
    }
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
