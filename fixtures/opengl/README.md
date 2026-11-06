# OpenGL fixtures

The source-only host fixtures are in
[`crates/wlam-host/tests/gl_renderer.rs`](../../crates/wlam-host/tests/gl_renderer.rs).
They submit a clipped triangle, overlapping near/far triangles, and a nearer
`GL_QUAD_STRIP`, then check fixed sample pixels at 32×24 and 64×48. A second
case drives `OpenGl::call` directly to check `GL_COMPILE`, deferred drawing,
replay under a changed matrix, and replay after resizing the GDI surface. A
third checks the advertised pixel-format request, one-time format assignment,
valid and invalid context/DC/window handles, unbinding and teardown. A fourth
lights front- and back-facing triangles with distinct ambient materials and
checks their output colors. A fifth verifies that flat shading carries the
primitive's provoking vertex through clipping and quad triangulation. A sixth
checks the sticky first-error latch, one-shot `glGetError` retrieval and
`glBegin`/`glEnd` command legality: vertex attribute calls remain legal and an
ignored invalid command does not terminate an active primitive.
Color and normal state may also be set outside a primitive. Display-list
fixtures verify reserved generated names, undefined positive calls as no-ops,
`GL_INVALID_VALUE` for list zero, nested compilation/replay, and invocation
inside a primitive.
The matrix-stack case checks the guaranteed projection and model-view limits,
overflow, and underflow. Invalid negative viewport dimensions report
`GL_INVALID_VALUE` and leave the last valid viewport in effect.
Unknown `glClear` mask bits likewise report `GL_INVALID_VALUE` without
modifying the framebuffer.
The renderer suite also draws points, independent lines, line strips and line
loops, checks homogeneous line clipping and color interpolation, and verifies
second-vertex color selection for flat-shaded line segments.

Run it with:

```sh
cargo test --offline -p wlam-host --test gl_renderer
```

The redistributable PE32 smoke fixture is [`window.c`](window.c). Build its PE,
guest bundle and browser host with `scripts/build-opengl-fixture.sh`, then run
the end-to-end assertions with:

```sh
node apps/web/tests/opengl-fixture.mjs
```

The PE creates a window and WGL context, checks the zero-argument
`glGetError` import before, during and after drawing, verifies the
`glBegin`/`glEnd` error rule, swaps a solid triangle with a green point overlaid,
deletes the context, releases the DC and exits. The Node runner checks CPU
pixels, ordered triangle and point packet modes, reached lifecycle APIs and
exit status. `node apps/web/tests/webgl-opengl-fixture.mjs` verifies that the
GPU-only browser path presents both primitives in the expected colors.

Together the fixtures cover primitive assembly, clipping, depth ordering,
display-list capture/replay, transforms, resize, WGL handle/pixel-format
validation, front/back lighting materials and flat-shaded provoking-vertex
preservation, points and line modes, `glBegin`/`glEnd` legality, matrix-stack
limits and GL error retrieval. Native Windows visual fidelity for TOLYMPIC
remains a separate comparison requirement.
