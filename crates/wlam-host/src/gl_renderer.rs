//! CPU rasterizer for the legacy OpenGL triangle path.
//!
//! This is kept independent of WGL and API argument decoding. The caller
//! supplies OpenGL's already transformed matrices, current state and immediate
//! mode triangles, then copies the resulting RGBA surface into a window DC.

use crate::opengl_math::Mat4;

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Light {
    /// Eye-space position. A positional light has `w != 0`.
    pub position: [f32; 4],
    pub ambient: [f32; 4],
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub ambient: [f32; 4],
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    pub emission: [f32; 4],
    pub shininess: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            ambient: [0.2, 0.2, 0.2, 1.0],
            diffuse: [0.8, 0.8, 0.8, 1.0],
            specular: [0.0, 0.0, 0.0, 1.0],
            emission: [0.0, 0.0, 0.0, 1.0],
            shininess: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct State {
    pub modelview: Mat4,
    pub projection: Mat4,
    pub viewport: [i32; 4],
    pub clear_color: [f32; 4],
    pub clear_depth: f32,
    pub depth_test: bool,
    /// OpenGL compare enum; default is GL_LESS.
    pub depth_func: u32,
    pub cull_face: bool,
    /// OpenGL face enum; default is GL_BACK.
    pub cull_mode: u32,
    pub front_ccw: bool,
    pub lighting: bool,
    pub local_viewer: bool,
    pub two_sided_lighting: bool,
    pub normalize_normals: bool,
    pub color_material: bool,
    pub color_material_face: u32,
    pub color_material_mode: u32,
    pub global_ambient: [f32; 4],
    pub lights: [Light; 8],
    pub material: Material,
    pub back_material: Material,
    pub blend: bool,
    pub blend_src: u32,
    pub blend_dst: u32,
    /// GL_DITHER; enabled by default for the RGBA framebuffer.
    pub dither: bool,
    pub fog: bool,
    /// OpenGL fog enum; GL_LINEAR by default.
    pub fog_mode: u32,
    pub fog_color: [f32; 4],
    pub fog_density: f32,
    pub fog_start: f32,
    pub fog_end: f32,
    pub smooth_shading: bool,
}

/// Ordered WebGL-ready operations captured alongside CPU rasterization.
/// Positions stay in homogeneous clip space so the browser GPU owns clipping
/// and viewport rasterization; lighting is evaluated with the same Rust
/// fixed-function equations used by the CPU comparison renderer.
#[derive(Clone, Debug)]
pub enum GpuOp {
    Clear {
        mask: u32,
        color: [f32; 4],
        depth: f32,
    },
    Draw(GpuDraw),
}

#[derive(Clone, Debug)]
pub struct GpuDraw {
    pub state: GpuDrawState,
    /// OpenGL primitive enum used by the browser draw call.
    pub primitive: u32,
    pub vertices: Vec<GpuVertex>,
}

#[derive(Clone, Copy, Debug)]
pub struct GpuDrawState {
    pub viewport: [i32; 4],
    pub depth_test: bool,
    pub depth_func: u32,
    pub cull_face: bool,
    pub cull_mode: u32,
    pub front_ccw: bool,
    pub two_sided_lighting: bool,
    pub blend: bool,
    pub blend_src: u32,
    pub blend_dst: u32,
    pub dither: bool,
    pub fog: bool,
    pub fog_mode: u32,
    pub fog_color: [f32; 4],
    pub fog_density: f32,
    pub fog_start: f32,
    pub fog_end: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct GpuVertex {
    pub clip_position: [f32; 4],
    pub eye_distance: f32,
    pub front_color: [f32; 4],
    pub back_color: [f32; 4],
}

impl Default for State {
    fn default() -> Self {
        Self {
            modelview: Mat4::IDENTITY,
            projection: Mat4::IDENTITY,
            viewport: [0, 0, 640, 480],
            clear_color: [0.0, 0.0, 0.0, 0.0],
            clear_depth: 1.0,
            depth_test: false,
            depth_func: 0x0201, // GL_LESS
            cull_face: false,
            cull_mode: 0x0405, // GL_BACK
            front_ccw: true,
            lighting: false,
            local_viewer: false,
            two_sided_lighting: false,
            normalize_normals: false,
            color_material: false,
            color_material_face: 0x0408, // GL_FRONT_AND_BACK
            color_material_mode: 0x1602, // GL_AMBIENT_AND_DIFFUSE
            global_ambient: [0.2, 0.2, 0.2, 1.0],
            lights: [Light::default(); 8],
            material: Material::default(),
            back_material: Material::default(),
            blend: false,
            blend_src: 1, // GL_ONE
            blend_dst: 0, // GL_ZERO
            dither: true,
            fog: false,
            fog_mode: 0x2601, // GL_LINEAR
            fog_color: [0.0, 0.0, 0.0, 0.0],
            fog_density: 1.0,
            fog_start: 0.0,
            fog_end: 1.0,
            smooth_shading: true,
        }
    }
}

#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub depth: Vec<u32>,
    gpu_ops: Vec<GpuOp>,
    gpu_capture: bool,
    cpu_render: bool,
}

impl Frame {
    pub fn new(width: u32, height: u32, state: &State) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let count = (width as usize).checked_mul(height as usize)?;
        let bytes = count.checked_mul(4)?;
        let mut frame = Self {
            width,
            height,
            rgba: vec![0; bytes],
            depth: vec![depth32(f64::from(state.clear_depth)); count],
            gpu_ops: Vec::new(),
            gpu_capture: false,
            cpu_render: true,
        };
        frame.clear(state, 0x0000_4000 | 0x0000_0100);
        Some(frame)
    }

    pub fn clear(&mut self, state: &State, mask: u32) {
        if self.gpu_capture {
            self.gpu_ops.push(GpuOp::Clear {
                mask,
                color: state.clear_color,
                depth: state.clear_depth,
            });
        }
        if !self.cpu_render {
            // The ordered clear packet is consumed by WebGL; touching the
            // CPU color or depth surfaces here would retain the full-surface
            // bandwidth cost that GPU-only presentation is meant to avoid.
            return;
        }
        if mask & 0x0000_4000 != 0 {
            let color: [u8; 4] = std::array::from_fn(|component| {
                if state.dither {
                    // Keep clears spatially uniform; this deterministic
                    // candidate choice matches llvmpipe's RGBA clear behavior.
                    dithered_byte(state.clear_color[component], 1, 0)
                } else {
                    to_byte(state.clear_color[component])
                }
            });
            for pixel in self.rgba.chunks_exact_mut(4) {
                pixel.copy_from_slice(&color);
            }
        }
        if mask & 0x0000_0100 != 0 {
            self.depth.fill(depth32(f64::from(state.clear_depth)));
        }
    }

    /// Drain the ordered GPU packet since the previous presentation boundary.
    pub fn take_gpu_ops(&mut self) -> Vec<GpuOp> {
        std::mem::take(&mut self.gpu_ops)
    }

    pub fn set_gpu_capture(&mut self, enabled: bool) {
        self.gpu_capture = enabled;
        if !enabled {
            self.gpu_ops.clear();
        }
    }

    pub fn set_cpu_render(&mut self, enabled: bool) {
        self.cpu_render = enabled;
    }

    /// Rasterize triangles with the OpenGL viewport, depth, culling, lighting,
    /// fog and blend state. Coordinates use column-vector matrix convention.
    fn draw_triangles(&mut self, state: &State, triangles: &[Triangle]) {
        let [vx, vy, vw, vh] = state.viewport;
        if vw <= 0 || vh <= 0 {
            return;
        }
        let transform = state.projection.multiply(state.modelview);
        if self.gpu_capture {
            let mut gpu_vertices = Vec::with_capacity(triangles.len() * 3);
            for triangle in triangles {
                let provoking_eye = transform4(state.modelview, triangle.provoking.position);
                let transformed = triangle.vertices.map(|vertex| {
                    (
                        vertex,
                        transform4(transform, vertex.position),
                        transform4(state.modelview, vertex.position),
                    )
                });
                if transformed.iter().any(|(_, clip, eye)| {
                    !clip.iter().chain(eye.iter()).all(|value| value.is_finite())
                }) {
                    continue;
                }
                let flat_front = shade(state, triangle.provoking, provoking_eye, false);
                let flat_back = if state.two_sided_lighting {
                    shade(state, triangle.provoking, provoking_eye, true)
                } else {
                    flat_front
                };
                for (vertex, clip, eye) in transformed {
                    let (front_color, back_color) = if state.smooth_shading {
                        (
                            shade(state, vertex, eye, false),
                            if state.two_sided_lighting {
                                shade(state, vertex, eye, true)
                            } else {
                                shade(state, vertex, eye, false)
                            },
                        )
                    } else {
                        (flat_front, flat_back)
                    };
                    gpu_vertices.push(GpuVertex {
                        clip_position: clip,
                        eye_distance: -eye[2],
                        front_color,
                        back_color,
                    });
                }
            }
            if !gpu_vertices.is_empty() {
                self.gpu_ops.push(GpuOp::Draw(GpuDraw {
                    state: gpu_draw_state(state),
                    primitive: 0x0004, // GL_TRIANGLES
                    vertices: gpu_vertices,
                }));
            }
        }
        if !self.cpu_render {
            return;
        }
        for triangle in triangles {
            let provoking_eye = transform4(state.modelview, triangle.provoking.position);
            let flat_front_color = shade(state, triangle.provoking, provoking_eye, false);
            let flat_back_color = if state.two_sided_lighting {
                shade(state, triangle.provoking, provoking_eye, true)
            } else {
                flat_front_color
            };
            let mut polygon = Vec::with_capacity(9);
            for vertex in triangle.vertices {
                let clip = transform4(transform, vertex.position);
                let eye = transform4(state.modelview, vertex.position);
                if !clip.iter().chain(eye.iter()).all(|x| x.is_finite()) {
                    polygon.clear();
                    break;
                }
                polygon.push(ClipVertex {
                    clip,
                    eye_distance: -eye[2],
                    front_color: shade(state, vertex, eye, false),
                    back_color: if state.two_sided_lighting {
                        shade(state, vertex, eye, true)
                    } else {
                        shade(state, vertex, eye, false)
                    },
                    flat_front_color,
                    flat_back_color,
                });
            }
            // OpenGL clips primitives against all six homogeneous clip planes.
            // Rejecting a whole triangle whenever one vertex lies outside the
            // frustum drops visible edge pieces (notably the outer red ring).
            for plane in 0..6 {
                if polygon.is_empty() {
                    break;
                }
                let input = std::mem::take(&mut polygon);
                let mut previous = *input.last().unwrap();
                let mut previous_distance = previous.clip_distance(plane);
                for current in input.iter().copied() {
                    let current_distance = current.clip_distance(plane);
                    let previous_inside = previous_distance >= 0.0;
                    let current_inside = current_distance >= 0.0;
                    if previous_inside != current_inside {
                        let t = previous_distance / (previous_distance - current_distance);
                        polygon.push(previous.interpolate(current, t));
                    }
                    if current_inside {
                        polygon.push(current);
                    }
                    previous = current;
                    previous_distance = current_distance;
                }
            }
            if polygon.len() < 3 {
                continue;
            }
            let screen = polygon
                .into_iter()
                .filter_map(|vertex| {
                    let w = vertex.clip[3];
                    if w <= 0.0 || !w.is_finite() {
                        return None;
                    }
                    let nx = vertex.clip[0] / w;
                    let ny = vertex.clip[1] / w;
                    let nz = f64::from(vertex.clip[2]) / f64::from(w);
                    Some(ScreenVertex {
                        x: subpixel(vx as f32 + (nx + 1.0) * 0.5 * vw as f32),
                        y: subpixel(
                            self.height as f32 - (vy as f32 + (ny + 1.0) * 0.5 * vh as f32),
                        ),
                        z: nz * 0.5 + 0.5,
                        inv_w: 1.0 / w,
                        eye_distance: vertex.eye_distance,
                        front_color: vertex.front_color,
                        back_color: vertex.back_color,
                        flat_front_color: vertex.flat_front_color,
                        flat_back_color: vertex.flat_back_color,
                    })
                })
                .collect::<Vec<_>>();
            if screen.len() >= 3 {
                for i in 1..screen.len() - 1 {
                    self.raster_triangle(state, [screen[0], screen[i], screen[i + 1]]);
                }
            }
        }
    }

    fn draw_points(&mut self, state: &State, points: &[Vertex]) {
        if state.viewport[2] <= 0 || state.viewport[3] <= 0 {
            return;
        }
        let transform = state.projection.multiply(state.modelview);
        if self.gpu_capture {
            let gpu_vertices = points
                .iter()
                .filter_map(|vertex| {
                    let clip = transform4(transform, vertex.position);
                    let eye = transform4(state.modelview, vertex.position);
                    if clip[3] <= 0.0
                        || !clip.iter().chain(eye.iter()).all(|value| value.is_finite())
                    {
                        return None;
                    }
                    let color = shade(state, *vertex, eye, false);
                    Some(GpuVertex {
                        clip_position: clip,
                        eye_distance: -eye[2],
                        front_color: color,
                        back_color: color,
                    })
                })
                .collect::<Vec<_>>();
            if !gpu_vertices.is_empty() {
                self.gpu_ops.push(GpuOp::Draw(GpuDraw {
                    state: gpu_draw_state(state),
                    primitive: 0x0000, // GL_POINTS
                    vertices: gpu_vertices,
                }));
            }
        }
        if !self.cpu_render {
            return;
        }
        for vertex in points {
            let clip = transform4(transform, vertex.position);
            if !clip.iter().all(|x| x.is_finite())
                || clip[3] <= 0.0
                || clip[0] < -clip[3]
                || clip[0] > clip[3]
                || clip[1] < -clip[3]
                || clip[1] > clip[3]
                || clip[2] < -clip[3]
                || clip[2] > clip[3]
            {
                continue;
            }
            let w = clip[3];
            let [vx, vy, vw, vh] = state.viewport;
            if vw <= 0 || vh <= 0 {
                continue;
            }
            let x = ((vx as f32 + (clip[0] / w + 1.0) * 0.5 * vw as f32).floor()) as i32;
            let y = (self.height as f32 - (vy as f32 + (clip[1] / w + 1.0) * 0.5 * vh as f32))
                .floor() as i32;
            if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
                continue;
            }
            let eye = transform4(state.modelview, vertex.position);
            let color = shade(state, *vertex, eye, false);
            self.write_fragment(state, x as u32, y as u32, clip[2] / w * 0.5 + 0.5, color);
        }
    }

    fn draw_lines(&mut self, state: &State, lines: &[[Vertex; 2]]) {
        let transform = state.projection.multiply(state.modelview);
        let [vx, vy, vw, vh] = state.viewport;
        if vw <= 0 || vh <= 0 {
            return;
        }
        for line in lines {
            let flat_eye = transform4(state.modelview, line[1].position);
            let flat_color = shade(state, line[1], flat_eye, false);
            let clip = line.map(|v| {
                let eye = transform4(state.modelview, v.position);
                LineClipVertex {
                    clip: transform4(transform, v.position),
                    eye_distance: -eye[2],
                    color: shade(state, v, eye, false),
                    flat_color,
                }
            });
            if clip.iter().flat_map(|v| v.clip).any(|x| !x.is_finite()) {
                continue;
            }
            let mut a = clip[0];
            let mut b = clip[1];
            // Homogeneous line clipping against the six canonical planes.
            let distance = |p: [f32; 4], plane: usize| match plane {
                0 => p[0] + p[3],
                1 => p[3] - p[0],
                2 => p[1] + p[3],
                3 => p[3] - p[1],
                4 => p[2] + p[3],
                _ => p[3] - p[2],
            };
            let mut rejected = false;
            for plane in 0..6 {
                let da = distance(a.clip, plane);
                let db = distance(b.clip, plane);
                if da < 0.0 && db < 0.0 {
                    rejected = true;
                    break;
                }
                if da < 0.0 || db < 0.0 {
                    let t = da / (da - db);
                    let point = a.interpolate(b, t);
                    if da < 0.0 {
                        a = point;
                    } else {
                        b = point;
                    }
                }
            }
            if rejected || a.clip[3] <= 0.0 || b.clip[3] <= 0.0 {
                continue;
            }
            let to_screen = |p: LineClipVertex| {
                [
                    vx as f32 + (p.clip[0] / p.clip[3] + 1.0) * 0.5 * vw as f32,
                    self.height as f32
                        - (vy as f32 + (p.clip[1] / p.clip[3] + 1.0) * 0.5 * vh as f32),
                    p.clip[2] / p.clip[3] * 0.5 + 0.5,
                    1.0 / p.clip[3],
                ]
            };
            let sa = to_screen(a);
            let sb = to_screen(b);
            let steps = (sb[0] - sa[0]).abs().max((sb[1] - sa[1]).abs()).ceil() as i32;
            if steps == 0 {
                let color = if state.smooth_shading {
                    b.color
                } else {
                    b.flat_color
                };
                self.write_fragment(
                    state,
                    sa[0].floor() as u32,
                    sa[1].floor() as u32,
                    sa[2],
                    color,
                );
                continue;
            }
            for step in 0..=steps {
                let t = step as f32 / steps as f32;
                let x = (sa[0] + (sb[0] - sa[0]) * t).floor() as i32;
                let y = (sa[1] + (sb[1] - sa[1]) * t).floor() as i32;
                if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
                    continue;
                }
                let reciprocal_w = sa[3] + (sb[3] - sa[3]) * t;
                let c0 = a.color;
                let c1 = b.color;
                let color = if state.smooth_shading {
                    std::array::from_fn(|c| {
                        if reciprocal_w > 1e-20 {
                            (c0[c] * sa[3] * (1.0 - t) + c1[c] * sb[3] * t) / reciprocal_w
                        } else {
                            0.0
                        }
                    })
                } else {
                    a.flat_color
                };
                let depth = sa[2] * (1.0 - t) + sb[2] * t;
                let output = if state.fog {
                    let distance = if reciprocal_w > 1e-20 {
                        (a.eye_distance * sa[3] * (1.0 - t) + b.eye_distance * sb[3] * t)
                            / reciprocal_w
                    } else {
                        0.0
                    };
                    let factor = fog_factor(state, distance);
                    let mut fogged = color;
                    for c in 0..3 {
                        fogged[c] = color[c] * factor + state.fog_color[c] * (1.0 - factor);
                    }
                    fogged
                } else {
                    color
                };
                self.write_fragment(state, x as u32, y as u32, depth, output);
            }
        }
    }

    fn write_fragment(&mut self, state: &State, x: u32, y: u32, depth: f32, src: [f32; 4]) {
        let at = y as usize * self.width as usize + x as usize;
        let old_depth = f64::from(self.depth[at]) / 4_294_967_295.0;
        if state.depth_test && !depth_pass(state.depth_func, f64::from(depth), old_depth) {
            return;
        }
        if state.depth_test {
            self.depth[at] = depth32(f64::from(depth));
        }
        let offset = at * 4;
        let dst = std::array::from_fn(|c| self.rgba[offset + c] as f32 / 255.0);
        let out = if state.blend {
            blend(state.blend_src, state.blend_dst, src, dst)
        } else {
            src
        };
        for c in 0..4 {
            self.rgba[offset + c] = if state.dither {
                dithered_byte(out[c], x, y)
            } else {
                to_byte(out[c])
            };
        }
    }

    fn raster_triangle(&mut self, state: &State, v: [ScreenVertex; 3]) {
        let area = edge(v[0], v[1], v[2].x, v[2].y);
        if area.abs() < 1e-6 {
            return;
        }
        let front = if state.front_ccw {
            area < 0.0
        } else {
            area > 0.0
        };
        if state.cull_face
            && ((state.cull_mode == 0x0404 && front)
                || (state.cull_mode == 0x0405 && !front)
                || state.cull_mode == 0x0408)
        {
            return;
        }
        let min_x = v.iter().map(|p| p.x.floor() as i32).min().unwrap().max(0);
        let max_x = v
            .iter()
            .map(|p| p.x.ceil() as i32)
            .max()
            .unwrap()
            .min(self.width as i32 - 1);
        let min_y = v.iter().map(|p| p.y.floor() as i32).min().unwrap().max(0);
        let max_y = v
            .iter()
            .map(|p| p.y.ceil() as i32)
            .max()
            .unwrap()
            .min(self.height as i32 - 1);
        let inv_area = 1.0 / area;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let e0 = edge(v[1], v[2], px, py);
                let e1 = edge(v[2], v[0], px, py);
                let e2 = edge(v[0], v[1], px, py);
                if !inside_edge(e0, v[1], v[2], area)
                    || !inside_edge(e1, v[2], v[0], area)
                    || !inside_edge(e2, v[0], v[1], area)
                {
                    continue;
                }
                let b0 = e0 * inv_area;
                let b1 = e1 * inv_area;
                let b2 = 1.0 - b0 - b1;
                let reciprocal_w = b0 * v[0].inv_w + b1 * v[1].inv_w + b2 * v[2].inv_w;
                // The pixel format requests 24 depth bits. Keep 32-bit
                // normalized depth internally, a permitted higher precision,
                // and compare interpolated window depth before storage.
                let depth = window_depth_at(v, px, py);
                let at = y as usize * self.width as usize + x as usize;
                let stored_depth = self.depth[at];
                if state.depth_test
                    && !depth_pass(
                        state.depth_func,
                        depth,
                        f64::from(stored_depth) / 4_294_967_295.0,
                    )
                {
                    continue;
                }
                if state.depth_test {
                    self.depth[at] = depth32(depth);
                }
                let mut src = [0.0; 4];
                for (component, value) in src.iter_mut().enumerate() {
                    let color = |vertex: ScreenVertex| {
                        if state.two_sided_lighting && !front {
                            vertex.back_color[component]
                        } else {
                            vertex.front_color[component]
                        }
                    };
                    *value = if state.smooth_shading {
                        let interpolated = b0 * color(v[0]) * v[0].inv_w
                            + b1 * color(v[1]) * v[1].inv_w
                            + b2 * color(v[2]) * v[2].inv_w;
                        if reciprocal_w > 1e-20 {
                            interpolated / reciprocal_w
                        } else {
                            0.0
                        }
                    } else if state.two_sided_lighting && !front {
                        v[0].flat_back_color[component]
                    } else {
                        v[0].flat_front_color[component]
                    };
                }
                if state.fog {
                    let interpolated = b0 * v[0].eye_distance * v[0].inv_w
                        + b1 * v[1].eye_distance * v[1].inv_w
                        + b2 * v[2].eye_distance * v[2].inv_w;
                    let distance = if reciprocal_w > 1e-20 {
                        interpolated / reciprocal_w
                    } else {
                        0.0
                    };
                    let factor = fog_factor(state, distance);
                    for c in 0..3 {
                        src[c] = src[c] * factor + state.fog_color[c] * (1.0 - factor);
                    }
                }
                let offset = at * 4;
                let dst = [
                    self.rgba[offset] as f32 / 255.0,
                    self.rgba[offset + 1] as f32 / 255.0,
                    self.rgba[offset + 2] as f32 / 255.0,
                    self.rgba[offset + 3] as f32 / 255.0,
                ];
                let out = if state.blend {
                    blend(state.blend_src, state.blend_dst, src, dst)
                } else {
                    src
                };
                for c in 0..4 {
                    self.rgba[offset + c] = if state.dither {
                        dithered_byte(out[c], x as u32, y as u32)
                    } else {
                        to_byte(out[c])
                    };
                }
            }
        }
    }
}

fn gpu_draw_state(state: &State) -> GpuDrawState {
    GpuDrawState {
        viewport: state.viewport,
        depth_test: state.depth_test,
        depth_func: state.depth_func,
        cull_face: state.cull_face,
        cull_mode: state.cull_mode,
        front_ccw: state.front_ccw,
        two_sided_lighting: state.two_sided_lighting,
        blend: state.blend,
        blend_src: state.blend_src,
        blend_dst: state.blend_dst,
        dither: state.dither,
        fog: state.fog,
        fog_mode: state.fog_mode,
        fog_color: state.fog_color,
        fog_density: state.fog_density,
        fog_start: state.fog_start,
        fog_end: state.fog_end,
    }
}

/// OpenGL immediate-mode accumulator. Attribute setters update current state;
/// each vertex captures the values in effect at that call.
#[derive(Debug)]
pub struct Immediate {
    current_normal: [f32; 3],
    current_color: [f32; 4],
    mode: Option<u32>,
    vertices: Vec<Vertex>,
}

impl Default for Immediate {
    fn default() -> Self {
        Self {
            current_normal: [0.0, 0.0, 1.0],
            current_color: [1.0, 1.0, 1.0, 1.0],
            mode: None,
            vertices: Vec::new(),
        }
    }
}

impl Immediate {
    pub fn normal3f(&mut self, normal: [f32; 3]) {
        self.current_normal = normal;
    }

    pub fn color4f(&mut self, color: [f32; 4]) {
        self.current_color = color;
    }

    pub fn color3ub(&mut self, color: [u8; 3]) {
        self.current_color = [
            color[0] as f32 / 255.0,
            color[1] as f32 / 255.0,
            color[2] as f32 / 255.0,
            1.0,
        ];
    }

    pub fn begin(&mut self, mode: u32) -> Result<(), &'static str> {
        if self.mode.is_some() {
            return Err("glBegin inside glBegin");
        }
        if !matches!(mode, 0x0000..=0x0009) {
            return Err("unsupported OpenGL primitive mode");
        }
        self.mode = Some(mode);
        self.vertices.clear();
        Ok(())
    }

    pub fn vertex3f(&mut self, position: [f32; 3]) -> Result<(), &'static str> {
        if self.mode.is_none() {
            return Err("glVertex outside glBegin/glEnd");
        }
        if self.vertices.len() == self.vertices.capacity() {
            self.vertices
                .try_reserve(128)
                .map_err(|_| "OpenGL immediate vertex allocation failed")?;
        }
        self.vertices.push(Vertex {
            position,
            normal: self.current_normal,
            color: self.current_color,
        });
        Ok(())
    }

    pub fn end(&mut self, state: &State, frame: &mut Frame) -> Result<(), &'static str> {
        let mode = self.mode.take().ok_or("glEnd without glBegin")?;
        match mode {
            0x0000 => frame.draw_points(state, &self.vertices), // GL_POINTS
            0x0001 => frame.draw_lines(state, &assemble_lines(mode, &self.vertices)),
            0x0002 => frame.draw_lines(state, &assemble_lines(mode, &self.vertices)),
            0x0003 => frame.draw_lines(state, &assemble_lines(mode, &self.vertices)),
            _ => {
                let triangles = assemble(mode, &self.vertices);
                frame.draw_triangles(state, &triangles);
            }
        }
        self.vertices.clear();
        Ok(())
    }

    pub fn is_inside_begin(&self) -> bool {
        self.mode.is_some()
    }
}

fn assemble_lines(mode: u32, vertices: &[Vertex]) -> Vec<[Vertex; 2]> {
    match mode {
        0x0001 => vertices.chunks_exact(2).map(|v| [v[0], v[1]]).collect(), // GL_LINES
        0x0002 => {
            if vertices.len() < 2 {
                return Vec::new();
            }
            (0..vertices.len())
                .map(|i| [vertices[i], vertices[(i + 1) % vertices.len()]])
                .collect()
        }
        0x0003 => vertices.windows(2).map(|v| [v[0], v[1]]).collect(), // GL_LINE_STRIP
        _ => Vec::new(),
    }
}

fn assemble(mode: u32, vertices: &[Vertex]) -> Vec<Triangle> {
    let mut out = Vec::new();
    let mut push = |vertices: [Vertex; 3], provoking: Vertex| {
        out.push(Triangle {
            vertices,
            provoking,
        });
    };
    match mode {
        0x0004 => {
            // GL_TRIANGLES
            for v in vertices.chunks_exact(3) {
                push([v[0], v[1], v[2]], v[2]);
            }
        }
        0x0005 => {
            // GL_TRIANGLE_STRIP
            for i in 0..vertices.len().saturating_sub(2) {
                if i & 1 == 0 {
                    push(
                        [vertices[i], vertices[i + 1], vertices[i + 2]],
                        vertices[i + 2],
                    );
                } else {
                    push(
                        [vertices[i + 1], vertices[i], vertices[i + 2]],
                        vertices[i + 2],
                    );
                }
            }
        }
        0x0006 | 0x0009 => {
            // GL_TRIANGLE_FAN / GL_POLYGON
            for i in 1..vertices.len().saturating_sub(1) {
                let provoking = if mode == 0x0009 {
                    vertices[0]
                } else {
                    vertices[i + 1]
                };
                push([vertices[0], vertices[i], vertices[i + 1]], provoking);
            }
        }
        0x0007 => {
            // GL_QUADS
            for v in vertices.chunks_exact(4) {
                push([v[0], v[1], v[2]], v[3]);
                push([v[0], v[2], v[3]], v[3]);
            }
        }
        0x0008 => {
            // GL_QUAD_STRIP
            for i in (0..vertices.len().saturating_sub(3)).step_by(2) {
                let provoking = vertices[i + 3];
                // A quad strip's polygon order is (v0, v1, v3, v2). Split
                // along its fan diagonal and preserve that winding.
                push([vertices[i], vertices[i + 1], vertices[i + 3]], provoking);
                push([vertices[i], vertices[i + 3], vertices[i + 2]], provoking);
            }
        }
        _ => {} // Non-filled primitives are added when a target needs them.
    }
    out
}

#[derive(Clone, Copy, Debug, Default)]
struct ScreenVertex {
    x: f32,
    y: f32,
    z: f64,
    inv_w: f32,
    eye_distance: f32,
    front_color: [f32; 4],
    back_color: [f32; 4],
    flat_front_color: [f32; 4],
    flat_back_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
struct Triangle {
    vertices: [Vertex; 3],
    provoking: Vertex,
}

#[derive(Clone, Copy, Debug)]
struct ClipVertex {
    clip: [f32; 4],
    eye_distance: f32,
    front_color: [f32; 4],
    back_color: [f32; 4],
    flat_front_color: [f32; 4],
    flat_back_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
struct LineClipVertex {
    clip: [f32; 4],
    eye_distance: f32,
    color: [f32; 4],
    flat_color: [f32; 4],
}

impl LineClipVertex {
    fn interpolate(self, other: Self, t: f32) -> Self {
        Self {
            clip: std::array::from_fn(|i| self.clip[i] + (other.clip[i] - self.clip[i]) * t),
            eye_distance: self.eye_distance + (other.eye_distance - self.eye_distance) * t,
            color: std::array::from_fn(|i| self.color[i] + (other.color[i] - self.color[i]) * t),
            flat_color: self.flat_color,
        }
    }
}

impl ClipVertex {
    fn clip_distance(self, plane: usize) -> f32 {
        match plane {
            0 => self.clip[0] + self.clip[3],
            1 => self.clip[3] - self.clip[0],
            2 => self.clip[1] + self.clip[3],
            3 => self.clip[3] - self.clip[1],
            4 => self.clip[2] + self.clip[3],
            _ => self.clip[3] - self.clip[2],
        }
    }

    fn interpolate(self, other: Self, t: f32) -> Self {
        let mut clip = [0.0; 4];
        let mut front_color = [0.0; 4];
        let mut back_color = [0.0; 4];
        for i in 0..4 {
            clip[i] = self.clip[i] + (other.clip[i] - self.clip[i]) * t;
            front_color[i] = self.front_color[i] + (other.front_color[i] - self.front_color[i]) * t;
            back_color[i] = self.back_color[i] + (other.back_color[i] - self.back_color[i]) * t;
        }
        Self {
            clip,
            eye_distance: self.eye_distance + (other.eye_distance - self.eye_distance) * t,
            front_color,
            back_color,
            flat_front_color: self.flat_front_color,
            flat_back_color: self.flat_back_color,
        }
    }
}

fn transform4(matrix: Mat4, point: [f32; 3]) -> [f32; 4] {
    let p = [point[0], point[1], point[2], 1.0];
    let mut out = [0.0; 4];
    for row in 0..4 {
        for col in 0..4 {
            out[row] += matrix.get(row, col) * p[col];
        }
    }
    out
}

fn edge(a: ScreenVertex, b: ScreenVertex, x: f32, y: f32) -> f32 {
    (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x)
}

fn subpixel(value: f32) -> f32 {
    // The host rasterizer uses 8 fractional bits for window coordinates.
    const SCALE: f32 = 256.0;
    (value * SCALE).round() / SCALE
}

/// Apply a top-left fill convention so a sample on a shared triangle edge is
/// rasterized by exactly one triangle.
fn inside_edge(value: f32, a: ScreenVertex, b: ScreenVertex, area: f32) -> bool {
    if area > 0.0 {
        value > 0.0 || (value == 0.0 && is_top_left(a, b))
    } else {
        value < 0.0 || (value == 0.0 && is_top_left(b, a))
    }
}

fn is_top_left(a: ScreenVertex, b: ScreenVertex) -> bool {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    dy < 0.0 || (dy == 0.0 && dx > 0.0)
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if n > 1e-20 && n.is_finite() {
        [v[0] / n, v[1] / n, v[2] / n]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn shade(state: &State, vertex: Vertex, eye: [f32; 4], back_face: bool) -> [f32; 4] {
    if !state.lighting {
        return vertex.color;
    }
    let material = if back_face {
        &state.back_material
    } else {
        &state.material
    };
    let mut normal = transform_normal(state.modelview, vertex.normal);
    if back_face {
        normal = normal.map(|value| -value);
    }
    let n = if state.normalize_normals {
        normalize(normal)
    } else {
        normal
    };
    // With local viewing disabled the viewer is at infinity along eye-space +Z.
    let view = if state.local_viewer {
        normalize([-eye[0], -eye[1], -eye[2]])
    } else {
        [0.0, 0.0, 1.0]
    };
    let diffuse_alpha = if color_material_tracks(state, back_face, 0x1201) {
        vertex.color[3]
    } else {
        material.diffuse[3]
    };
    let mut result = [0.0, 0.0, 0.0, diffuse_alpha];
    for c in 0..3 {
        let ambient = if color_material_tracks(state, back_face, 0x1200) {
            vertex.color[c]
        } else {
            material.ambient[c]
        };
        let emission = if color_material_tracks(state, back_face, 0x1600) {
            vertex.color[c]
        } else {
            material.emission[c]
        };
        result[c] = emission + ambient * state.global_ambient[c];
    }
    for light in state.lights.iter().filter(|l| l.enabled) {
        for c in 0..3 {
            let ambient = if color_material_tracks(state, back_face, 0x1200) {
                vertex.color[c]
            } else {
                material.ambient[c]
            };
            result[c] += ambient * light.ambient[c];
        }
        let direction = if light.position[3] == 0.0 {
            normalize([light.position[0], light.position[1], light.position[2]])
        } else {
            normalize([
                light.position[0] - eye[0],
                light.position[1] - eye[1],
                light.position[2] - eye[2],
            ])
        };
        let ndotl = (n[0] * direction[0] + n[1] * direction[1] + n[2] * direction[2]).max(0.0);
        for c in 0..3 {
            let diffuse = if color_material_tracks(state, back_face, 0x1201) {
                vertex.color[c]
            } else {
                material.diffuse[c]
            };
            result[c] += diffuse * light.diffuse[c] * ndotl;
        }
        if ndotl > 0.0 && material.shininess > 0.0 {
            let half = normalize([
                direction[0] + view[0],
                direction[1] + view[1],
                direction[2] + view[2],
            ]);
            let ndoth = (n[0] * half[0] + n[1] * half[1] + n[2] * half[2]).max(0.0);
            let spec = ndoth.powf(material.shininess);
            for c in 0..3 {
                let specular = if color_material_tracks(state, back_face, 0x1202) {
                    vertex.color[c]
                } else {
                    material.specular[c]
                };
                result[c] += specular * light.specular[c] * spec;
            }
        }
    }
    result.map(|v| v.clamp(0.0, 1.0))
}

fn color_material_tracks(state: &State, back_face: bool, pname: u32) -> bool {
    let face_matches = state.color_material_face == 0x0408
        || (state.color_material_face == 0x0404 && !back_face)
        || (state.color_material_face == 0x0405 && back_face);
    if !state.color_material || !face_matches {
        return false;
    }
    state.color_material_mode == pname
        || (state.color_material_mode == 0x1602 && matches!(pname, 0x1200 | 0x1201))
}

/// Transform a normal by the inverse transpose of the model-view 3x3. The
/// adjugate equals the inverse transpose after division by its determinant.
/// OpenGL leaves singular normal matrices undefined; return a zero vector here
/// so normalization has the renderer's stable fallback behavior.
fn transform_normal(matrix: Mat4, normal: [f32; 3]) -> [f32; 3] {
    let a = matrix.get(0, 0);
    let b = matrix.get(0, 1);
    let c = matrix.get(0, 2);
    let d = matrix.get(1, 0);
    let e = matrix.get(1, 1);
    let f = matrix.get(1, 2);
    let g = matrix.get(2, 0);
    let h = matrix.get(2, 1);
    let i = matrix.get(2, 2);
    let cofactors = [
        [e * i - f * h, f * g - d * i, d * h - e * g],
        [c * h - b * i, a * i - c * g, b * g - a * h],
        [b * f - c * e, c * d - a * f, a * e - b * d],
    ];
    let determinant = a * cofactors[0][0] + b * cofactors[0][1] + c * cofactors[0][2];
    if !determinant.is_finite() || determinant.abs() <= 1e-20 {
        return [0.0; 3];
    }
    std::array::from_fn(|row| {
        (cofactors[row][0] * normal[0]
            + cofactors[row][1] * normal[1]
            + cofactors[row][2] * normal[2])
            / determinant
    })
}

fn depth_pass(func: u32, source: f64, destination: f64) -> bool {
    match func {
        0x0200 => false, // GL_NEVER
        0x0201 => source < destination,
        0x0202 => source == destination,
        0x0203 => source <= destination,
        0x0204 => source > destination,
        0x0205 => source != destination,
        0x0206 => source >= destination,
        0x0207 => true, // GL_ALWAYS
        _ => false,
    }
}

fn depth32(value: f64) -> u32 {
    const MAX_DEPTH: f64 = 4_294_967_295.0;
    (value.clamp(0.0, 1.0) * MAX_DEPTH).round() as u32
}

/// Interpolate incoming window depth in double precision before converting it
/// to the renderer's 32-bit normalized depth storage.
fn window_depth_at(vertices: [ScreenVertex; 3], x: f32, y: f32) -> f64 {
    let edge64 = |a: ScreenVertex, b: ScreenVertex| {
        let ax = f64::from(a.x);
        let ay = f64::from(a.y);
        let bx = f64::from(b.x);
        let by = f64::from(b.y);
        (bx - ax) * (f64::from(y) - ay) - (by - ay) * (f64::from(x) - ax)
    };
    let area = (f64::from(vertices[1].x) - f64::from(vertices[0].x))
        * (f64::from(vertices[2].y) - f64::from(vertices[0].y))
        - (f64::from(vertices[1].y) - f64::from(vertices[0].y))
            * (f64::from(vertices[2].x) - f64::from(vertices[0].x));
    if area == 0.0 || !area.is_finite() {
        return 1.0;
    }
    let weights = [
        edge64(vertices[1], vertices[2]) / area,
        edge64(vertices[2], vertices[0]) / area,
        edge64(vertices[0], vertices[1]) / area,
    ];
    let depth =
        weights[0] * vertices[0].z + weights[1] * vertices[1].z + weights[2] * vertices[2].z;
    depth.clamp(0.0, 1.0)
}

fn fog_factor(state: &State, distance: f32) -> f32 {
    match state.fog_mode {
        0x0800 => ((state.fog_end - distance) / (state.fog_end - state.fog_start)).clamp(0.0, 1.0),
        0x0801 => (-state.fog_density * distance).exp().clamp(0.0, 1.0),
        0x0802 => (-(state.fog_density * distance).powi(2))
            .exp()
            .clamp(0.0, 1.0),
        _ => 1.0,
    }
}

fn blend(src_factor: u32, dst_factor: u32, src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let factor = |mode: u32, c: usize, is_source: bool| match mode {
        0 => 0.0, // GL_ZERO
        1 => 1.0, // GL_ONE
        0x0300 => src[c],
        0x0301 => 1.0 - src[c],
        0x0302 => src[3],
        0x0303 => 1.0 - src[3],
        0x0304 => dst[3],
        0x0305 => 1.0 - dst[3],
        0x0306 => dst[c],       // GL_DST_COLOR
        0x0307 => 1.0 - dst[c], // GL_ONE_MINUS_DST_COLOR
        0x0308 if is_source => {
            if c == 3 {
                1.0
            } else {
                src[3].min(1.0 - dst[3])
            }
        } // GL_SRC_ALPHA_SATURATE (not valid as a destination factor)
        _ => 1.0,
    };
    std::array::from_fn(|c| {
        (src[c] * factor(src_factor, c, true) + dst[c] * factor(dst_factor, c, false))
            .clamp(0.0, 1.0)
    })
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).floor() as u8
}

/// Quantize with a stable ordered dither. OpenGL leaves the dither pattern
/// implementation-dependent; this 4x4 Bayer pattern is the software
/// framebuffer's choice for its enabled-by-default RGBA state.
fn dithered_byte(value: f32, x: u32, y: u32) -> u8 {
    const BAYER_4X4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
    let index = ((y & 3) * 4 + (x & 3)) as usize;
    let threshold = (f32::from(BAYER_4X4[index]) + 0.5) / 16.0 - 0.5;
    (value.clamp(0.0, 1.0) * 255.0 + threshold + 0.5).clamp(0.0, 255.0) as u8
}
