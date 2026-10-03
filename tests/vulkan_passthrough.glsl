// Exercise native GLSL-to-SPIR-V compilation and the sampled frame binding.
void mainImage(out vec4 color, in vec2 pixel) {
    color = texture(iChannel0, pixel / iResolution.xy);
}
