#version 450

// One triangle over the whole target; the picture's region of the source,
// turned upright, spans it: a target point p reads the source at
// origin + p.x * across + p.y * down, in its texture coordinates.
layout(push_constant) uniform Place {
    // origin.xy, across.xy
    vec4 a;
    // down.xy
    vec4 b;
} pc;

layout(location = 0) out vec2 uv;

void main() {
    vec2 p = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    uv = pc.a.xy + p.x * pc.a.zw + p.y * pc.b.xy;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
