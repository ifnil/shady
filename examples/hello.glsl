out vec4 fragColor;

#define PI 3.14159265359

float plot(vec2 uv, float pct) {
  return smoothstep(pct - 0.02, pct, uv.y) - smoothstep(pct, pct + 0.02, uv.y);
}

void main() {
  // vec2 uv = (2.0 * gl_FragCoord.xy - u_resolution.xy) / u_resolution.y;
  vec2 uv = gl_FragCoord.xy / u_resolution.xy;

  float y = smoothstep(0.1, 0.9, uv.x);
  vec3 color = vec3(y);

  float pct = plot(uv, y);
  color = (1.0 - pct) * color + pct * vec3(0.0, 0.0, 1.0);

  fragColor = vec4(color, 1.0);
}
