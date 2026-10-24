#version 450

// One triangle over the whole target; the picture's region of the source,
// an offset and a scale in its texture coordinates, spans it.
layout(push_constant) uniform Region {
    vec4 region;
} pc;

layout(location = 0) out vec2 uv;

void main() {
    vec2 p = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    uv = pc.region.xy + p * pc.region.zw;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
