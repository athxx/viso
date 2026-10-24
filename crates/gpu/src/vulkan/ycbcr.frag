#version 450

// The source, sampled through its Y'CbCr conversion: gamma-encoded RGB,
// stored as it is into the target's UNORM view.
layout(set = 0, binding = 0) uniform sampler2D picture;

layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;

void main() {
    color = vec4(texture(picture, uv).rgb, 1.0);
}
