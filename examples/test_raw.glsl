#version 330 core
out vec4 fragColor;
void main() {
    vec2 uv = gl_FragCoord.xy / iResolution.xy;
    fragColor = vec4(uv, 0.5 + 0.5 * sin(iTime), 1.0);
}
