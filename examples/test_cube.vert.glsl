#version 330 core
// shady:draw_vertices=36

out vec3 v_world_pos;
out vec3 v_normal;

const vec3 CUBE_POS[8] = vec3[8](
    vec3(-1.0, -1.0, -1.0),
    vec3( 1.0, -1.0, -1.0),
    vec3( 1.0,  1.0, -1.0),
    vec3(-1.0,  1.0, -1.0),
    vec3(-1.0, -1.0,  1.0),
    vec3( 1.0, -1.0,  1.0),
    vec3( 1.0,  1.0,  1.0),
    vec3(-1.0,  1.0,  1.0)
);

const int CUBE_IDX[36] = int[36](
    0, 1, 2, 0, 2, 3,
    4, 6, 5, 4, 7, 6,
    0, 4, 5, 0, 5, 1,
    1, 5, 6, 1, 6, 2,
    2, 6, 7, 2, 7, 3,
    3, 7, 4, 3, 4, 0
);

const vec3 FACE_NORMALS[6] = vec3[6](
    vec3( 0.0,  0.0, -1.0),
    vec3( 0.0,  0.0,  1.0),
    vec3( 0.0, -1.0,  0.0),
    vec3( 1.0,  0.0,  0.0),
    vec3( 0.0,  1.0,  0.0),
    vec3(-1.0,  0.0,  0.0)
);

mat3 rot_x(float a) {
    float s = sin(a);
    float c = cos(a);
    return mat3(
        1.0, 0.0, 0.0,
        0.0, c,  -s,
        0.0, s,   c
    );
}

mat3 rot_y(float a) {
    float s = sin(a);
    float c = cos(a);
    return mat3(
         c, 0.0, s,
        0.0, 1.0, 0.0,
        -s, 0.0, c
    );
}

void main() {
    int idx = CUBE_IDX[gl_VertexID];
    vec3 pos = CUBE_POS[idx];
    vec3 n = FACE_NORMALS[gl_VertexID / 6];

    mat3 rot = rot_y(u_time * 0.8) * rot_x(u_time * 0.55);
    vec3 world_pos = rot * (pos * 0.75);
    vec3 world_n = normalize(rot * n);

    vec3 camera = vec3(0.0, 0.0, 3.0);
    vec3 view = world_pos - camera;

    float aspect = u_resolution.x / max(u_resolution.y, 1.0);
    float inv_z = -1.0 / view.z;

    gl_Position = vec4(
        (view.x * inv_z) / aspect,
        view.y * inv_z,
        0.0,
        1.0
    );

    v_world_pos = world_pos;
    v_normal = world_n;
}
