#version 330 core

in vec3 v_world_pos;
in vec3 v_normal;

out vec4 fragColor;

void main() {
    vec3 n = normalize(v_normal);
    vec3 l = normalize(vec3(0.4, 0.8, 0.6));
    float diff = max(dot(n, l), 0.0);

    vec3 base = vec3(0.18, 0.55, 0.92);
    base += 0.08 * sin(vec3(1.3, 1.7, 2.1) * (v_world_pos + u_time));

    vec3 camera_dir = normalize(vec3(0.0, 0.0, 1.0));
    float rim = pow(1.0 - max(dot(n, camera_dir), 0.0), 2.2);

    vec3 color = base * (0.2 + 0.8 * diff) + vec3(0.95, 0.55, 0.2) * rim * 0.25;
    fragColor = vec4(color, 1.0);
}
