#version 450

// A picture in NV12 planes: luma, then Cb and Cr interleaved at half
// resolution. Each row of the matrix turns (Y', Cb, Cr, 1) into one channel
// of gamma-encoded RGB, stored as it is into the target's UNORM view.
layout(set = 0, binding = 0) uniform sampler2D luma;
layout(set = 0, binding = 1) uniform sampler2D chroma;

layout(push_constant) uniform Convert {
    vec4 region;
    vec4 r;
    vec4 g;
    vec4 b;
    // How chroma's texture coordinates follow luma's: a scale, for a plane
    // rounded up to whole samples, then an offset, for its siting.
    vec4 site;
} pc;

layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;

void main() {
    vec4 ycc = vec4(texture(luma, uv).r, texture(chroma, uv * pc.site.xy + pc.site.zw).rg, 1.0);
    vec3 rgb = vec3(dot(ycc, pc.r), dot(ycc, pc.g), dot(ycc, pc.b));
    color = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
