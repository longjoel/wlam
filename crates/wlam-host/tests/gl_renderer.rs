use wlam_host::{
    gdi::Gdi,
    gl_context::{GlContexts, PixelFormatDescriptor},
    gl_renderer::{Frame, Immediate, Light, State},
    memory::MemoryAccess,
    opengl::OpenGl,
    GuestMemory,
};

fn draw_fixture(frame: &mut Frame, width: u32, height: u32) {
    let mut state = State::default();
    state.viewport = [0, 0, width as i32, height as i32];
    state.depth_test = true;
    state.dither = false;
    state.clear_color = [0.1, 0.1, 0.1, 1.0];
    frame.clear(&state, 0x0000_4100);

    let mut immediate = Immediate::default();
    immediate.begin(0x0004).unwrap(); // GL_TRIANGLES

    // Near blue triangle crosses the left clip plane.
    immediate.color3ub([0, 0, 255]);
    for vertex in [[-1.5, -0.8, -0.5], [0.8, -0.8, -0.5], [-0.35, 0.8, -0.5]] {
        immediate.vertex3f(vertex).unwrap();
    }

    // Far red triangle overlaps the blue one but must fail GL_LESS there.
    immediate.color3ub([255, 0, 0]);
    for vertex in [[-0.8, -0.6, 0.5], [0.8, -0.6, 0.5], [0.0, 0.8, 0.5]] {
        immediate.vertex3f(vertex).unwrap();
    }
    immediate.end(&state, frame).unwrap();

    // A nearer quad strip checks the primitive assembly used by TOLYMPIC.
    immediate.begin(0x0008).unwrap(); // GL_QUAD_STRIP
    immediate.color3ub([0, 255, 0]);
    for vertex in [
        [0.25, 0.25, -0.25],
        [0.25, 0.75, -0.25],
        [0.75, 0.25, -0.25],
        [0.75, 0.75, -0.25],
    ] {
        immediate.vertex3f(vertex).unwrap();
    }
    immediate.end(&state, frame).unwrap();
}

fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * frame.width + x) * 4) as usize;
    frame.rgba[offset..offset + 4].try_into().unwrap()
}

#[test]
fn clipped_overlap_survives_resize_with_depth_ordering() {
    for (width, height) in [(32, 24), (64, 48)] {
        let state = State::default();
        let mut frame = Frame::new(width, height, &state).unwrap();
        draw_fixture(&mut frame, width, height);

        // The near triangle remains visible where the far triangle overlaps.
        assert_eq!(pixel(&frame, width / 2, height / 2), [0, 0, 255, 255]);

        // The green quad strip is independently assembled and depth-tested.
        assert_eq!(pixel(&frame, width * 3 / 4, height / 4), [0, 255, 0, 255]);

        // Its portion beyond x=-1 was clipped, leaving a blue sample at the
        // left edge rather than dropping the complete primitive.
        assert!(
            (0..height).any(|y| pixel(&frame, 0, y) == [0, 0, 255, 255]),
            "clipped blue triangle should reach the left edge at {width}×{height}"
        );

        // A distant sample outside both triangles keeps the clear color.
        assert_eq!(pixel(&frame, width - 1, 0), [25, 25, 25, 255]);
    }
}

#[test]
fn gpu_capture_preserves_ordered_clears_and_per_draw_state() {
    let mut state = State { viewport: [0, 0, 32, 24], dither: false, ..State::default() };
    let mut frame = Frame::new(32, 24, &state).unwrap();
    assert!(frame.take_gpu_ops().is_empty());
    frame.set_gpu_capture(true);
    frame.clear(&state, 0x0000_4100);
    assert!(matches!(frame.take_gpu_ops().as_slice(), [wlam_host::gl_renderer::GpuOp::Clear { .. }]));

    let mut immediate = Immediate::default();
    immediate.begin(0x0004).unwrap();
    for point in [[-0.8, -0.8, 0.0], [0.8, -0.8, 0.0], [0.0, 0.8, 0.0]] {
        immediate.vertex3f(point).unwrap();
    }
    state.depth_test = true;
    immediate.end(&state, &mut frame).unwrap();

    state.clear_color = [0.1, 0.2, 0.3, 1.0];
    frame.clear(&state, 0x0000_4000);
    state.blend = true;
    state.blend_src = 0x0302;
    state.blend_dst = 1;
    immediate.begin(0x0004).unwrap();
    for point in [[-0.5, -0.5, -0.2], [0.5, -0.5, -0.2], [0.0, 0.5, -0.2]] {
        immediate.vertex3f(point).unwrap();
    }
    immediate.end(&state, &mut frame).unwrap();

    let ops = frame.take_gpu_ops();
    assert!(matches!(&ops[..], [
        wlam_host::gl_renderer::GpuOp::Draw(first),
        wlam_host::gl_renderer::GpuOp::Clear { mask: 0x0000_4000, .. },
        wlam_host::gl_renderer::GpuOp::Draw(second),
    ] if first.state.depth_test && !first.state.blend && first.vertices.len() == 3
        && second.state.blend && second.state.blend_src == 0x0302
        && second.vertices.len() == 3));
    assert!(frame.take_gpu_ops().is_empty());
}

#[test]
fn gpu_only_clear_records_work_without_touching_cpu_surfaces() {
    let state = State {
        clear_color: [0.5, 0.5, 0.5, 0.0],
        clear_depth: 0.25,
        ..State::default()
    };
    let mut frame = Frame::new(32, 24, &State::default()).unwrap();
    let color_before = frame.rgba.clone();
    let depth_before = frame.depth.clone();
    frame.set_gpu_capture(true);
    frame.set_cpu_render(false);
    frame.clear(&state, 0x0000_4100);

    assert_eq!(frame.rgba, color_before);
    assert_eq!(frame.depth, depth_before);
    assert!(matches!(frame.take_gpu_ops().as_slice(), [
        wlam_host::gl_renderer::GpuOp::Clear {
            mask: 0x0000_4100,
            color: [0.5, 0.5, 0.5, 0.0],
            depth: 0.25,
        }
    ]));
}

#[test]
fn point_and_line_primitive_modes_render_clipped_samples() {
    let state = State { viewport: [0, 0, 32, 32], dither: false, ..State::default() };
    let mut frame = Frame::new(32, 32, &state).unwrap();
    let mut immediate = Immediate::default();

    immediate.begin(0x0000).unwrap(); // GL_POINTS
    immediate.color3ub([255, 0, 0]);
    immediate.vertex3f([0.0, 0.0, 0.0]).unwrap();
    immediate.vertex3f([1.5, 0.0, 0.0]).unwrap(); // clipped
    immediate.end(&state, &mut frame).unwrap();
    assert_eq!(pixel(&frame, 16, 16), [255, 0, 0, 255]);

    immediate.begin(0x0001).unwrap(); // GL_LINES
    immediate.color3ub([255, 0, 0]);
    immediate.vertex3f([-1.5, 0.0, -0.5]).unwrap();
    immediate.color3ub([0, 0, 255]);
    immediate.vertex3f([0.0, 0.0, -0.5]).unwrap();
    immediate.end(&state, &mut frame).unwrap();
    let clipped = pixel(&frame, 0, 16);
    assert!(clipped[0] > 140 && clipped[2] < 120,
        "clipped line endpoint keeps interpolated color: {clipped:?}");

    immediate.begin(0x0003).unwrap(); // GL_LINE_STRIP
    immediate.color3ub([0, 0, 255]);
    for point in [[0.0, -0.5, -0.25], [0.5, -0.5, -0.25], [0.5, 0.0, -0.25]] {
        immediate.vertex3f(point).unwrap();
    }
    immediate.end(&state, &mut frame).unwrap();
    assert!(pixel(&frame, 24, 20)[2] > 200);

    immediate.begin(0x0002).unwrap(); // GL_LINE_LOOP
    immediate.color3ub([255, 255, 0]);
    for point in [[-0.5, -0.5, -0.75], [0.5, -0.5, -0.75], [0.0, 0.5, -0.75]] {
        immediate.vertex3f(point).unwrap();
    }
    immediate.end(&state, &mut frame).unwrap();
    assert!(frame.rgba.chunks_exact(4).any(|p| p[0] > 200 && p[1] > 200 && p[2] < 20));

    let flat_state = State { smooth_shading: false, ..state };
    immediate.begin(0x0001).unwrap();
    immediate.color3ub([255, 0, 0]);
    immediate.vertex3f([-0.5, 0.5, 0.0]).unwrap();
    immediate.color3ub([0, 0, 255]);
    immediate.vertex3f([0.5, 0.5, 0.0]).unwrap();
    immediate.end(&flat_state, &mut frame).unwrap();
    assert_eq!(pixel(&frame, 16, 8), [0, 0, 255, 255],
        "flat-shaded lines use their second vertex color");
}

fn gl_call(
    gl: &mut OpenGl,
    gdi: &Gdi,
    memory: &mut GuestMemory<'_>,
    name: &str,
    args: &[u32],
) -> u32 {
    gl.call(1, 7, name, args, memory, gdi).unwrap()
}

fn bits(values: [f32; 3]) -> [u32; 3] {
    values.map(f32::to_bits)
}

fn presented_pixel(gdi: &Gdi, x: u32, y: u32) -> [u8; 4] {
    let surface = gdi.surface(7).unwrap();
    let offset = ((y * surface.width + x) * 4) as usize;
    surface.rgba[offset..offset + 4].try_into().unwrap()
}

#[test]
fn display_list_compiles_replays_transforms_and_survives_resize() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 256];
    let mut memory = GuestMemory::new(&mut bytes);
    memory.write(64, &[0, 0, 255]).unwrap();

    let viewport = [0, 0, 32, 24];
    gl_call(
        &mut gl,
        &gdi,
        &mut memory,
        "glViewport",
        &viewport.map(|v| v as u32),
    );
    gl_call(
        &mut gl,
        &gdi,
        &mut memory,
        "glClearColor",
        &[
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            1.0f32.to_bits(),
        ],
    );
    let list = gl_call(&mut gl, &gdi, &mut memory, "glGenLists", &[1]);
    gl_call(&mut gl, &gdi, &mut memory, "glNewList", &[list, 0x1300]); // GL_COMPILE
    gl_call(&mut gl, &gdi, &mut memory, "glBegin", &[0x0004]); // GL_TRIANGLES
    gl_call(&mut gl, &gdi, &mut memory, "glColor3ubv", &[64]);
    for vertex in [[-0.5, -0.5, 0.0], [0.5, -0.5, 0.0], [0.0, 0.5, 0.0]] {
        gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits(vertex));
    }
    gl_call(&mut gl, &gdi, &mut memory, "glEnd", &[]);
    gl_call(&mut gl, &gdi, &mut memory, "glEndList", &[]);

    // GL_COMPILE records commands without drawing them.
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 16, 12), [0, 0, 0, 255]);

    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[list]);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 16, 12), [0, 0, 255, 255]);

    gl_call(&mut gl, &gdi, &mut memory, "glClear", &[0x0000_4100]);
    gl_call(&mut gl, &gdi, &mut memory, "glLoadIdentity", &[]);
    gl_call(
        &mut gl,
        &gdi,
        &mut memory,
        "glTranslatef",
        &bits([0.75, 0.0, 0.0]),
    );
    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[list]);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 16, 12), [0, 0, 0, 255]);
    assert_eq!(presented_pixel(&gdi, 27, 12), [0, 0, 255, 255]);

    gdi.resize_surface(7, 64, 48).unwrap();
    let viewport = [0, 0, 64, 48];
    gl_call(
        &mut gl,
        &gdi,
        &mut memory,
        "glViewport",
        &viewport.map(|v| v as u32),
    );
    gl_call(&mut gl, &gdi, &mut memory, "glLoadIdentity", &[]);
    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[list]);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 32, 24), [0, 0, 255, 255]);
}

#[test]
fn pixel_format_and_context_handles_are_validated() {
    let mut contexts = GlContexts::default();
    let request = PixelFormatDescriptor::rgba_double_buffered();
    assert_eq!(contexts.choose_pixel_format(request), 1);

    let mut unsupported = request;
    unsupported.flags |= 0x0010; // PFD_SUPPORT_GDI is not advertised.
    assert_eq!(contexts.choose_pixel_format(unsupported), 0);

    assert!(contexts.set_pixel_format(7, 1));
    assert!(!contexts.set_pixel_format(7, 1)); // Window pixel format is one-time.
    assert!(!contexts.set_pixel_format(0, 1));
    assert!(!contexts.set_pixel_format(8, 2));

    let context = contexts.create_context(1);
    assert_ne!(context, 0);
    assert_eq!(contexts.create_context(2), 0);
    assert!(!contexts.make_current(0, 7, context));
    assert!(!contexts.make_current(3, 8, context));
    assert!(!contexts.make_current(3, 7, u32::MAX));
    assert!(contexts.make_current(3, 7, context));
    assert_eq!(contexts.require_current(), Ok((context, 3, 7)));
    assert!(contexts.swap_buffers(3, 7));
    assert_eq!(contexts.take_swap(), Some((context, 3, 7)));

    assert!(contexts.make_current(0, 0, 0));
    assert_eq!(contexts.current(), None);
    assert!(contexts.delete_context(context));
    assert!(!contexts.delete_context(context));
}

#[test]
fn two_sided_lighting_uses_front_and_back_materials() {
    let mut state = State::default();
    state.viewport = [0, 0, 64, 32];
    state.lighting = true;
    state.two_sided_lighting = true;
    state.dither = false;
    state.global_ambient = [0.0; 4];
    state.material.ambient = [1.0, 0.0, 0.0, 1.0];
    state.back_material.ambient = [0.0, 1.0, 0.0, 1.0];
    state.lights[0] = Light {
        ambient: [1.0; 4],
        enabled: true,
        ..Light::default()
    };

    let mut frame = Frame::new(64, 32, &state).unwrap();
    let mut immediate = Immediate::default();
    immediate.begin(0x0004).unwrap(); // GL_TRIANGLES

    // Counter-clockwise triangle exercises the front material.
    for vertex in [[-0.9, -0.5, 0.0], [-0.1, -0.5, 0.0], [-0.5, 0.5, 0.0]] {
        immediate.vertex3f(vertex).unwrap();
    }

    // Clockwise triangle exercises the back material.
    for vertex in [[0.1, -0.5, 0.0], [0.5, 0.5, 0.0], [0.9, -0.5, 0.0]] {
        immediate.vertex3f(vertex).unwrap();
    }
    immediate.end(&state, &mut frame).unwrap();

    assert_eq!(pixel(&frame, 10, 16), [255, 0, 0, 255]);
    assert_eq!(pixel(&frame, 42, 16), [0, 255, 0, 255]);
}

#[test]
fn flat_shading_keeps_the_primitive_provoking_vertex_through_clipping() {
    let mut state = State::default();
    state.viewport = [0, 0, 64, 32];
    state.dither = false;
    state.smooth_shading = false;
    state.clear_color = [0.0, 0.0, 0.0, 1.0];
    let mut frame = Frame::new(64, 32, &state).unwrap();
    let mut immediate = Immediate::default();

    // GL_QUADS uses the fourth submitted vertex as the flat color source.
    // The first vertex crosses the left clip plane; all generated triangles
    // still represent one quad primitive and must keep its fourth vertex.
    immediate.begin(0x0007).unwrap(); // GL_QUADS
    for (vertex, color) in [
        ([-1.5, -0.8, 0.0], [255, 0, 0]),
        ([0.8, -0.8, 0.0], [0, 255, 0]),
        ([0.8, 0.8, 0.0], [0, 0, 255]),
        ([-0.8, 0.8, 0.0], [255, 255, 0]),
    ] {
        immediate.color3ub(color);
        immediate.vertex3f(vertex).unwrap();
    }
    immediate.end(&state, &mut frame).unwrap();

    assert_eq!(pixel(&frame, 32, 16), [255, 255, 0, 255]);
    assert_eq!(pixel(&frame, 4, 16), [255, 255, 0, 255]);
}

#[test]
fn gl_get_error_reports_first_error_once_and_accepts_valid_commands() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 32];
    let mut memory = GuestMemory::new(&mut bytes);

    gl_call(&mut gl, &gdi, &mut memory, "glMatrixMode", &[0xdead_beef]);
    gl_call(&mut gl, &gdi, &mut memory, "glShadeModel", &[0xdead_beef]);
    assert_eq!(gl.take_error(1), 0x0500);
    assert_eq!(gl.take_error(1), 0);

    gl_call(&mut gl, &gdi, &mut memory, "glMatrixMode", &[0x1700]);
    gl_call(&mut gl, &gdi, &mut memory, "glLoadIdentity", &[]);
    assert_eq!(gl.take_error(1), 0);

    assert_eq!(
        wlam_pcode::abi::signature("OPENGL32.dll", "glGetError")
            .unwrap()
            .argument_bytes,
        0
    );
}

#[test]
fn gl_get_error_inside_begin_end_is_ignored_and_does_not_consume_errors() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 16];
    let mut memory = GuestMemory::new(&mut bytes);

    gl_call(&mut gl, &gdi, &mut memory, "glBegin", &[0x0004]);
    assert_eq!(gl.get_error(1), 0);
    gl_call(&mut gl, &gdi, &mut memory, "glEnd", &[]);
    assert_eq!(gl.get_error(1), 0x0502);
    assert_eq!(gl.get_error(1), 0);
}

#[test]
fn display_list_names_are_reserved_and_undefined_calls_are_noops() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 16];
    let mut memory = GuestMemory::new(&mut bytes);

    let first = gl_call(&mut gl, &gdi, &mut memory, "glGenLists", &[2]);
    assert_ne!(first, 0);
    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[first + 1]);
    assert_eq!(gl.take_error(1), 0); // reserved but still empty
    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[first + 100]);
    assert_eq!(gl.take_error(1), 0); // undefined positive names are no-ops
    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[0]);
    assert_eq!(gl.take_error(1), 0x0501);
}

#[test]
fn color_and_normal_state_can_be_set_outside_begin_end() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 32];
    let mut memory = GuestMemory::new(&mut bytes);
    memory.write(0, &[255, 0, 0]).unwrap();

    gl_call(&mut gl, &gdi, &mut memory, "glColor3ubv", &[0]);
    gl_call(
        &mut gl,
        &gdi,
        &mut memory,
        "glNormal3f",
        &bits([0.0, 0.0, 1.0]),
    );
    assert_eq!(gl.take_error(1), 0);

    gl_call(&mut gl, &gdi, &mut memory, "glBegin", &[0x0004]);
    for vertex in [[-0.8, -0.8, 0.0], [0.8, -0.8, 0.0], [0.0, 0.8, 0.0]] {
        gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits(vertex));
    }
    gl_call(&mut gl, &gdi, &mut memory, "glEnd", &[]);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 16, 12), [255, 0, 0, 255]);
    assert_eq!(gl.take_error(1), 0);
}

#[test]
fn display_lists_can_nest_and_be_called_inside_begin_end() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 32];
    let mut memory = GuestMemory::new(&mut bytes);
    memory.write(0, &[255, 0, 0]).unwrap();

    let first = gl_call(&mut gl, &gdi, &mut memory, "glGenLists", &[2]);
    gl_call(&mut gl, &gdi, &mut memory, "glNewList", &[first, 0x1300]);
    gl_call(&mut gl, &gdi, &mut memory, "glColor3ubv", &[0]);
    gl_call(&mut gl, &gdi, &mut memory, "glEndList", &[]);

    gl_call(&mut gl, &gdi, &mut memory, "glNewList", &[first + 1, 0x1300]);
    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[first]);
    gl_call(&mut gl, &gdi, &mut memory, "glEndList", &[]);

    gl_call(&mut gl, &gdi, &mut memory, "glBegin", &[0x0004]);
    gl_call(&mut gl, &gdi, &mut memory, "glCallList", &[first + 1]);
    for vertex in [[-0.8, -0.8, 0.0], [0.8, -0.8, 0.0], [0.0, 0.8, 0.0]] {
        gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits(vertex));
    }
    gl_call(&mut gl, &gdi, &mut memory, "glEnd", &[]);
    assert_eq!(gl.take_error(1), 0);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 16, 12), [255, 0, 0, 255]);
}

#[test]
fn begin_end_legality_preserves_vertex_attributes_and_rejects_state_changes() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 64];
    let mut memory = GuestMemory::new(&mut bytes);
    memory.write(32, &[255, 0, 0]).unwrap();

    gl_call(&mut gl, &gdi, &mut memory, "glBegin", &[0x0004]);
    gl_call(&mut gl, &gdi, &mut memory, "glColor3ubv", &[32]);
    gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits([-0.8, -0.8, 0.0]));
    // glClear is not legal between glBegin and glEnd and must not execute.
    gl_call(&mut gl, &gdi, &mut memory, "glClear", &[0x0000_4000]);

    // The invalid command is ignored; legal vertex commands that follow still
    // complete the active primitive, and glEnd does not add a second error.
    gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits([0.8, -0.8, 0.0]));
    gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits([0.0, 0.8, 0.0]));
    gl_call(&mut gl, &gdi, &mut memory, "glEnd", &[]);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 16, 12), [255, 0, 0, 255]);
    assert_eq!(gl.take_error(1), 0x0502);
    assert_eq!(gl.take_error(1), 0);
}

#[test]
fn matrix_stacks_enforce_projection_and_modelview_limits() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 16];
    let mut memory = GuestMemory::new(&mut bytes);

    // OpenGL guarantees a projection stack depth of 2: the current matrix
    // plus one pushed matrix. Model-view depth is guaranteed to be at least 32.
    gl_call(&mut gl, &gdi, &mut memory, "glMatrixMode", &[0x1701]);
    gl_call(&mut gl, &gdi, &mut memory, "glPushMatrix", &[]);
    assert_eq!(gl.take_error(1), 0);
    gl_call(&mut gl, &gdi, &mut memory, "glPushMatrix", &[]);
    assert_eq!(gl.take_error(1), 0x0503);
    gl_call(&mut gl, &gdi, &mut memory, "glPopMatrix", &[]);
    assert_eq!(gl.take_error(1), 0);
    gl_call(&mut gl, &gdi, &mut memory, "glPopMatrix", &[]);
    assert_eq!(gl.take_error(1), 0x0504);

    gl_call(&mut gl, &gdi, &mut memory, "glMatrixMode", &[0x1700]);
    for _ in 0..31 {
        gl_call(&mut gl, &gdi, &mut memory, "glPushMatrix", &[]);
    }
    assert_eq!(gl.take_error(1), 0);
    gl_call(&mut gl, &gdi, &mut memory, "glPushMatrix", &[]);
    assert_eq!(gl.take_error(1), 0x0503);
}

#[test]
fn negative_viewport_dimensions_report_invalid_value_and_keep_previous_viewport() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 16];
    let mut memory = GuestMemory::new(&mut bytes);

    gl_call(&mut gl, &gdi, &mut memory, "glViewport", &[2, 3, 20, 18]);
    gl_call(&mut gl, &gdi, &mut memory, "glViewport", &[0, 0, u32::MAX, 12]);
    assert_eq!(gl.take_error(1), 0x0501);

    // If the rejected viewport had been stored, this small triangle would
    // have no visible sample in the framebuffer.
    gl_call(&mut gl, &gdi, &mut memory, "glBegin", &[0x0004]);
    gl_call(&mut gl, &gdi, &mut memory, "glColor3ubv", &[0]);
    memory.write(0, &[255, 0, 0]).unwrap();
    gl_call(&mut gl, &gdi, &mut memory, "glColor3ubv", &[0]);
    for vertex in [[-0.8, -0.8, 0.0], [0.8, -0.8, 0.0], [0.0, 0.8, 0.0]] {
        gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits(vertex));
    }
    gl_call(&mut gl, &gdi, &mut memory, "glEnd", &[]);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 12, 12), [255, 0, 0, 255]);
    assert_eq!(gl.take_error(1), 0);
}

#[test]
fn invalid_clear_mask_reports_invalid_value_without_clearing() {
    let mut gdi = Gdi::default();
    gdi.create_surface(7, 32, 24).unwrap();
    let mut gl = OpenGl::default();
    let mut bytes = [0; 32];
    let mut memory = GuestMemory::new(&mut bytes);
    memory.write(0, &[255, 0, 0]).unwrap();

    gl_call(&mut gl, &gdi, &mut memory, "glViewport", &[0, 0, 32, 24]);
    gl_call(&mut gl, &gdi, &mut memory, "glBegin", &[0x0004]);
    gl_call(&mut gl, &gdi, &mut memory, "glColor3ubv", &[0]);
    for vertex in [[-0.8, -0.8, 0.0], [0.8, -0.8, 0.0], [0.0, 0.8, 0.0]] {
        gl_call(&mut gl, &gdi, &mut memory, "glVertex3f", &bits(vertex));
    }
    gl_call(&mut gl, &gdi, &mut memory, "glEnd", &[]);
    assert_eq!(gl.take_error(1), 0);

    // 0x8000 is not a GL clear-buffer bit. The command must set an error and
    // preserve the rendered center pixel.
    gl_call(&mut gl, &gdi, &mut memory, "glClear", &[0x0000_8000]);
    assert_eq!(gl.take_error(1), 0x0501);
    gl.present(1, 7, &mut gdi).unwrap();
    assert_eq!(presented_pixel(&gdi, 16, 12), [255, 0, 0, 255]);
}
