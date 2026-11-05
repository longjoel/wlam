//! Legacy fixed-function OpenGL command state for the TOLYMPIC path.
use crate::{
    gdi::Gdi,
    gl_renderer::{Frame, Immediate, State},
    memory::MemoryAccess,
    opengl_math::Mat4,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
enum Command {
    Begin(u32),
    End,
    Normal([f32; 3]),
    Vertex([f32; 3]),
    Color([u8; 3]),
    Clear(u32),
    ClearColor([f32; 4]),
    ClearDepth(f32),
    Viewport([i32; 4]),
    MatrixMode(u32),
    LoadIdentity,
    Push,
    Pop,
    Translate([f32; 3]),
    Rotate(f32, [f32; 3]),
    Ortho([f64; 6]),
    Perspective([f64; 4]),
    LookAt([f64; 9]),
    Enable(u32),
    Disable(u32),
    Shade(u32),
    Depth(u32),
    Cull(u32),
    Blend(u32, u32),
    ColorMaterial(u32, u32),
    Light(u32, u32, Vec<f32>),
    LightModel(u32, Vec<f32>),
    Material(u32, u32, Vec<f32>),
    Fog(u32, Vec<f32>),
    CallList(u32),
}
#[derive(Debug)]
struct Context {
    state: State,
    mv: Vec<Mat4>,
    pr: Vec<Mat4>,
    mode: u32,
    immediate: Immediate,
    frame: Frame,
    lists: BTreeMap<u32, Vec<Command>>,
    recording: Option<(u32, Vec<Command>, bool)>,
    error: u32,
}
#[derive(Debug, Default)]
pub struct OpenGl {
    contexts: BTreeMap<u32, Context>,
    next_list: u32,
    gpu_capture: bool,
    gpu_only: bool,
}

impl Context {
    fn new(w: u32, h: u32) -> Result<Self, String> {
        let mut state = State::default();
        state.viewport = [0, 0, w as i32, h as i32];
        let frame = Frame::new(w, h, &state).ok_or("invalid OpenGL surface dimensions")?;
        Ok(Self {
            state,
            mv: Vec::new(),
            pr: Vec::new(),
            mode: 0x1700,
            immediate: Immediate::default(),
            frame,
            lists: BTreeMap::new(),
            recording: None,
            error: 0,
        })
    }
    fn resize(&mut self, w: u32, h: u32) -> Result<(), String> {
        if (w, h) != (self.frame.width, self.frame.height) {
            self.frame =
                Frame::new(w, h, &self.state).ok_or("invalid OpenGL surface dimensions")?;
        }
        Ok(())
    }
    fn matrix(&mut self) -> &mut Mat4 {
        if self.mode == 0x1701 {
            &mut self.state.projection
        } else {
            &mut self.state.modelview
        }
    }
    fn set_error(&mut self, error: u32) {
        if self.error == 0 {
            self.error = error;
        }
    }
    fn is_inside_begin(&self) -> bool {
        self.immediate.is_inside_begin()
    }
    fn begin_end_legal(name: &str, inside: bool) -> bool {
        if inside {
            matches!(
                name,
                "glEnd" | "glNormal3f" | "glVertex3f" | "glColor3ubv" | "glCallList"
            )
        } else {
            !matches!(name, "glEnd" | "glVertex3f")
        }
    }
    fn command_legal_in_begin(command: &Command, inside: bool) -> bool {
        match command {
            Command::Begin(_) => !inside,
            Command::End => inside,
            Command::Vertex(_) => inside,
            Command::Normal(_) | Command::Color(_) => true,
            Command::CallList(_) => true,
            _ => !inside,
        }
    }
    fn execute_list(&mut self, list: u32, depth: u32) {
        const MAX_LIST_NESTING: u32 = 64;
        if list == 0 {
            self.set_error(0x0501);
            return;
        }
        if depth >= MAX_LIST_NESTING {
            self.set_error(0x0502);
            return;
        }
        let Some(commands) = self.lists.get(&list).cloned() else {
            return;
        };
        for command in commands {
            if let Command::CallList(nested) = command {
                self.execute_list(nested, depth + 1);
                continue;
            }
            if !Self::command_legal_in_begin(&command, self.immediate.is_inside_begin()) {
                self.set_error(0x0502);
                continue;
            }
            if self.execute(command).is_err() {
                self.set_error(0x0502);
            }
        }
    }
    fn execute(&mut self, c: Command) -> Result<(), String> {
        match c {
            Command::Begin(v) => {
                if self.immediate.begin(v).is_err() {
                    self.set_error(0x0502)
                }
            }
            Command::End => {
                if self.immediate.end(&self.state, &mut self.frame).is_err() {
                    self.set_error(0x0502)
                }
            }
            Command::Normal(v) => self.immediate.normal3f(v),
            Command::Vertex(v) => {
                if self.immediate.vertex3f(v).is_err() {
                    self.set_error(0x0502)
                }
            }
            Command::Color(v) => self.immediate.color3ub(v),
            Command::Clear(v) => {
                const VALID_CLEAR_BITS: u32 = 0x0000_4000 | 0x0000_0100 | 0x0000_0200 | 0x0000_0400;
                if v & !VALID_CLEAR_BITS != 0 {
                    self.set_error(0x0501);
                } else {
                    self.frame.clear(&self.state, v);
                }
            }
            Command::ClearColor(v) => self.state.clear_color = v,
            Command::ClearDepth(v) => self.state.clear_depth = v.clamp(0.0, 1.0),
            Command::Viewport(v) => {
                if v[2] < 0 || v[3] < 0 {
                    self.set_error(0x0501);
                } else {
                    self.state.viewport = v;
                }
            }
            Command::MatrixMode(v) => {
                if v == 0x1700 || v == 0x1701 {
                    self.mode = v
                } else {
                    self.set_error(0x0500)
                }
            }
            Command::LoadIdentity => *self.matrix() = Mat4::IDENTITY,
            Command::Push => {
                let m = *self.matrix();
                let stack = if self.mode == 0x1701 {
                    &mut self.pr
                } else {
                    &mut self.mv
                };
                let maximum_depth = if self.mode == 0x1701 { 2 } else { 32 };
                if stack.len() >= maximum_depth - 1 {
                    self.set_error(0x0503)
                } else {
                    stack.push(m)
                }
            }
            Command::Pop => {
                let stack = if self.mode == 0x1701 {
                    &mut self.pr
                } else {
                    &mut self.mv
                };
                if let Some(m) = stack.pop() {
                    *self.matrix() = m
                } else {
                    self.set_error(0x0504)
                }
            }
            Command::Translate(v) => match self.matrix().translated(v[0], v[1], v[2]) {
                Ok(m) => *self.matrix() = m,
                Err(_) => self.set_error(0x0501),
            },
            Command::Rotate(a, v) => match self.matrix().rotated(a, v) {
                Ok(m) => *self.matrix() = m,
                Err(_) => self.set_error(0x0501),
            },
            Command::Ortho(v) => {
                match Mat4::ortho(
                    v[0] as f32,
                    v[1] as f32,
                    v[2] as f32,
                    v[3] as f32,
                    v[4] as f32,
                    v[5] as f32,
                ) {
                    Ok(m) => {
                        let old = *self.matrix();
                        *self.matrix() = old.multiply(m)
                    }
                    Err(_) => self.set_error(0x0501),
                }
            }
            Command::Perspective(v) => match Mat4::perspective_f64(v[0], v[1], v[2], v[3]) {
                Ok(m) => self.state.projection = self.state.projection.multiply(m),
                Err(_) => self.set_error(0x0501),
            },
            Command::LookAt(v) => match Mat4::look_at(
                [v[0] as f32, v[1] as f32, v[2] as f32],
                [v[3] as f32, v[4] as f32, v[5] as f32],
                [v[6] as f32, v[7] as f32, v[8] as f32],
            ) {
                Ok(m) => self.state.modelview = self.state.modelview.multiply(m),
                Err(_) => self.set_error(0x0501),
            },
            Command::Enable(v) => self.cap(v, true),
            Command::Disable(v) => self.cap(v, false),
            Command::Shade(v) => match v {
                0x1d00 => self.state.smooth_shading = false,
                0x1d01 => self.state.smooth_shading = true,
                _ => self.set_error(0x0500),
            },
            Command::Depth(v) => {
                if (0x0200..=0x0207).contains(&v) {
                    self.state.depth_func = v
                } else {
                    self.set_error(0x0500)
                }
            }
            Command::Cull(v) => {
                if matches!(v, 0x0404 | 0x0405 | 0x0408) {
                    self.state.cull_mode = v
                } else {
                    self.set_error(0x0500)
                }
            }
            Command::Blend(s, d) => {
                if blend_factor(s, true) && blend_factor(d, false) {
                    self.state.blend_src = s;
                    self.state.blend_dst = d
                } else {
                    self.set_error(0x0500)
                }
            }
            Command::ColorMaterial(face, mode) => {
                if valid_face(face) && valid_color_material_mode(mode) {
                    self.state.color_material_face = face;
                    self.state.color_material_mode = mode
                } else {
                    self.set_error(0x0500)
                }
            }
            Command::Light(i, p, v) => self.light(i, p, &v),
            Command::LightModel(p, v) => match p {
                0x0b51 if !v.is_empty() => self.state.local_viewer = v[0] != 0.0,
                0x0b52 if !v.is_empty() => self.state.two_sided_lighting = v[0] != 0.0,
                0x0b53 if v.len() >= 4 => self.state.global_ambient.copy_from_slice(&v[..4]),
                _ => self.set_error(0x0500),
            },
            Command::Material(face, p, v) => self.material(face, p, &v),
            Command::Fog(p, v) => self.fog(p, &v),
            Command::CallList(list) => self.execute_list(list, 0),
        }
        Ok(())
    }
    fn cap(&mut self, c: u32, on: bool) {
        match c {
            0x0b71 => self.state.depth_test = on,
            0x0b50 => self.state.lighting = on,
            0x0b57 => self.state.color_material = on,
            0x0b60 => self.state.fog = on,
            0x0b44 => self.state.cull_face = on,
            0x0be2 => self.state.blend = on,
            0x0bd0 => self.state.dither = on,
            0x0ba1 => self.state.normalize_normals = on,
            0x4000..=0x4007 => self.state.lights[(c - 0x4000) as usize].enabled = on,
            _ => self.set_error(0x0500),
        }
    }
    fn light(&mut self, id: u32, p: u32, v: &[f32]) {
        let Some(i) = id.checked_sub(0x4000).filter(|i| *i < 8) else {
            self.set_error(0x0500);
            return;
        };
        let l = &mut self.state.lights[i as usize];
        let dst = match p {
            0x1200 => &mut l.ambient,
            0x1201 => &mut l.diffuse,
            0x1202 => &mut l.specular,
            0x1203 => &mut l.position,
            _ => {
                self.set_error(0x0500);
                return;
            }
        };
        if v.len() < 4 {
            self.set_error(0x0501);
            return;
        }
        dst.copy_from_slice(&v[..4]);
        if p == 0x1203 {
            let pos = l.position;
            let mut out = [0.0; 4];
            for r in 0..4 {
                for col in 0..4 {
                    out[r] += self.state.modelview.get(r, col) * pos[col]
                }
            }
            l.position = out
        }
    }
    fn material(&mut self, face: u32, p: u32, v: &[f32]) {
        if !valid_face(face) {
            self.set_error(0x0500);
            return;
        }
        let mut valid = true;
        if face == 0x0404 || face == 0x0408 {
            valid &= set_material(&mut self.state.material, p, v)
        }
        if face == 0x0405 || face == 0x0408 {
            valid &= set_material(&mut self.state.back_material, p, v)
        }
        if !valid {
            self.set_error(0x0500)
        }
    }
    fn fog(&mut self, p: u32, v: &[f32]) {
        match p {
            0x0b66 if v.len() >= 4 => self.state.fog_color.copy_from_slice(&v[..4]),
            0x0b62 if !v.is_empty() => self.state.fog_density = v[0].max(0.0),
            0x0b63 if !v.is_empty() => self.state.fog_start = v[0],
            0x0b64 if !v.is_empty() => self.state.fog_end = v[0],
            0x0b65 if !v.is_empty() => self.state.fog_mode = v[0] as u32,
            _ => self.set_error(0x0500),
        }
    }
}

impl OpenGl {
    /// Enable or disable ordered GPU draw-packet capture. It is opt-in so the
    /// native CPU renderer does not pay for duplicate vertex lighting work.
    pub fn set_gpu_capture(&mut self, enabled: bool) {
        self.gpu_capture = enabled;
        for context in self.contexts.values_mut() {
            context.frame.set_gpu_capture(enabled);
        }
    }

    /// Select the browser GPU renderer and skip CPU fragment rasterization.
    /// This backend currently targets the TOLYMPIC triangle/quad-strip path.
    pub fn set_gpu_mode(&mut self, enabled: bool) {
        self.gpu_capture = enabled;
        self.gpu_only = enabled;
        for context in self.contexts.values_mut() {
            context.frame.set_gpu_capture(enabled);
            context.frame.set_cpu_render(!enabled);
        }
    }

    /// Drain the ordered clip-space draw packet captured since the last
    /// presentation boundary. Browser hosts can transfer this to WebGL while
    /// native hosts continue presenting the CPU framebuffer.
    pub fn take_gpu_ops(&mut self, id: u32) -> Vec<crate::gl_renderer::GpuOp> {
        self.contexts
            .get_mut(&id)
            .map(|context| context.frame.take_gpu_ops())
            .unwrap_or_default()
    }

    /// Implement `glGetError` for the current context. The query itself is
    /// illegal inside a display-list `glBegin`/`glEnd` region; in that state
    /// it returns zero without consuming the pending error and records
    /// `GL_INVALID_OPERATION` for the next legal query.
    pub fn get_error(&mut self, id: u32) -> u32 {
        let Some(context) = self.contexts.get_mut(&id) else {
            return 0;
        };
        let inside_begin = context
            .recording
            .as_ref()
            .map(|(_, _, inside)| *inside)
            .unwrap_or_else(|| context.is_inside_begin());
        if inside_begin {
            context.set_error(0x0502);
            0
        } else {
            std::mem::replace(&mut context.error, 0)
        }
    }

    /// Return and clear the current context's first pending OpenGL error.
    /// OpenGL error flags are sticky until queried and retain the first error
    /// when more than one error occurs before `glGetError`.
    pub fn take_error(&mut self, id: u32) -> u32 {
        let Some(context) = self.contexts.get_mut(&id) else {
            return 0;
        };
        std::mem::replace(&mut context.error, 0)
    }

    pub fn context_destroyed(&mut self, id: u32) {
        self.contexts.remove(&id);
    }
    pub fn call(
        &mut self,
        id: u32,
        window: u32,
        name: &str,
        args: &[u32],
        memory: &mut dyn MemoryAccess,
        gdi: &Gdi,
    ) -> Result<u32, String> {
        let surface = gdi
            .surface(window)
            .ok_or("OpenGL context has no window surface")?;
        let (w, h) = (surface.width, surface.height);
        let gpu_capture = self.gpu_capture;
        let gpu_only = self.gpu_only;
        let gl = self.contexts.entry(id).or_insert(Context::new(w, h)?);
        gl.resize(w, h)?;
        gl.frame.set_gpu_capture(gpu_capture);
        gl.frame.set_cpu_render(!gpu_only);
        let inside_begin = gl
            .recording
            .as_ref()
            .map(|(_, _, inside)| *inside)
            .unwrap_or_else(|| gl.is_inside_begin());
        if !Context::begin_end_legal(name, inside_begin) {
            // OpenGL ignores the illegal command. In particular, an invalid
            // operation between glBegin/glEnd must leave the active
            // primitive intact so later legal vertex commands still apply.
            gl.set_error(0x0502);
            return Ok(0);
        }
        match name {
            "glGenLists" => {
                let n = arg(args, 0)?;
                if n == 0 {
                    return Ok(0);
                }
                let first = self.next_list.max(1);
                let Some(next) = first.checked_add(n) else {
                    return Ok(0);
                };
                self.next_list = next;
                for list in first..next {
                    gl.lists.entry(list).or_default();
                }
                return Ok(first);
            }
            "glNewList" => {
                let list = arg(args, 0)?;
                let mode = arg(args, 1)?;
                if gl.recording.is_some() {
                    gl.set_error(0x0502)
                } else if list == 0 {
                    gl.set_error(0x0501)
                } else if mode != 0x1300 {
                    gl.set_error(0x0500)
                } else {
                    gl.recording = Some((list, Vec::new(), false))
                }
                return Ok(0);
            }
            "glEndList" => {
                if let Some((list, commands, inside_begin)) = gl.recording.take() {
                    if inside_begin {
                        gl.set_error(0x0502);
                    }
                    gl.lists.insert(list, commands);
                } else {
                    gl.set_error(0x0502)
                }
                return Ok(0);
            }
            "glCallList" => {
                let list = arg(args, 0)?;
                if let Some((_, commands, _)) = &mut gl.recording {
                    commands.push(Command::CallList(list));
                } else {
                    gl.execute_list(list, 0);
                }
                return Ok(0);
            }
            _ => {}
        }
        let Some(c) = decode(name, args, memory)? else {
            return Ok(0);
        };
        if let Some((_, commands, recorded_inside_begin)) = &mut gl.recording {
            match c {
                Command::Begin(_) => *recorded_inside_begin = true,
                Command::End => *recorded_inside_begin = false,
                _ => {}
            }
            commands.push(c)
        } else {
            gl.execute(c)?
        }
        Ok(0)
    }
    pub fn present(&mut self, id: u32, window: u32, gdi: &mut Gdi) -> Result<(), String> {
        if self.gpu_only {
            return Ok(());
        }
        let gl = self
            .contexts
            .get(&id)
            .ok_or("OpenGL context has no renderer state")?;
        gdi.present_rgba(window, &gl.frame.rgba)
            .map_err(|e| format!("SwapBuffers: {e:?}"))
    }
}

fn arg(a: &[u32], i: usize) -> Result<u32, String> {
    a.get(i)
        .copied()
        .ok_or_else(|| format!("missing OpenGL argument {i}"))
}
fn f32a(a: &[u32], i: usize) -> Result<f32, String> {
    Ok(f32::from_bits(arg(a, i)?))
}
fn f64a(a: &[u32], i: usize) -> Result<f64, String> {
    Ok(f64::from_bits(
        arg(a, i)? as u64 | ((arg(a, i + 1)? as u64) << 32),
    ))
}
fn doubles(a: &[u32], count: usize) -> Result<Vec<f64>, String> {
    (0..count).map(|i| f64a(a, i * 2)).collect()
}
fn blend_factor(mode: u32, source: bool) -> bool {
    matches!(mode, 0 | 1 | 0x0300..=0x0307) || source && mode == 0x0308
}
fn valid_face(face: u32) -> bool {
    matches!(face, 0x0404 | 0x0405 | 0x0408)
}
fn valid_color_material_mode(mode: u32) -> bool {
    matches!(mode, 0x1200 | 0x1201 | 0x1202 | 0x1600 | 0x1602)
}
fn set_material(material: &mut crate::gl_renderer::Material, p: u32, v: &[f32]) -> bool {
    match p {
        0x1200 if v.len() >= 4 => material.ambient.copy_from_slice(&v[..4]),
        0x1201 if v.len() >= 4 => material.diffuse.copy_from_slice(&v[..4]),
        0x1202 if v.len() >= 4 => material.specular.copy_from_slice(&v[..4]),
        0x1600 if v.len() >= 4 => material.emission.copy_from_slice(&v[..4]),
        0x1601 if !v.is_empty() => material.shininess = v[0].clamp(0.0, 128.0),
        _ => return false,
    }
    true
}
fn floats(m: &mut dyn MemoryAccess, p: u32, n: usize) -> Result<Vec<f32>, String> {
    if n > 16 {
        return Err("OpenGL array too large".into());
    }
    let mut b = vec![0; n * 4];
    m.read(p, &mut b)
        .map_err(|e| format!("OpenGL array: {e:?}"))?;
    Ok(b.chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect())
}
fn decode(name: &str, a: &[u32], m: &mut dyn MemoryAccess) -> Result<Option<Command>, String> {
    let c = match name {
        "glBegin" => Command::Begin(arg(a, 0)?),
        "glEnd" => Command::End,
        "glNormal3f" => Command::Normal([f32a(a, 0)?, f32a(a, 1)?, f32a(a, 2)?]),
        "glVertex3f" => Command::Vertex([f32a(a, 0)?, f32a(a, 1)?, f32a(a, 2)?]),
        "glColor3ubv" => {
            let mut b = [0; 3];
            m.read(arg(a, 0)?, &mut b)
                .map_err(|e| format!("glColor3ubv: {e:?}"))?;
            Command::Color(b)
        }
        "glClear" => Command::Clear(arg(a, 0)?),
        "glClearColor" => Command::ClearColor([f32a(a, 0)?, f32a(a, 1)?, f32a(a, 2)?, f32a(a, 3)?]),
        "glClearDepth" => Command::ClearDepth(f64a(a, 0)? as f32),
        "glViewport" => Command::Viewport([
            arg(a, 0)? as i32,
            arg(a, 1)? as i32,
            arg(a, 2)? as i32,
            arg(a, 3)? as i32,
        ]),
        "glMatrixMode" => Command::MatrixMode(arg(a, 0)?),
        "glLoadIdentity" => Command::LoadIdentity,
        "glPushMatrix" => Command::Push,
        "glPopMatrix" => Command::Pop,
        "glTranslatef" => Command::Translate([f32a(a, 0)?, f32a(a, 1)?, f32a(a, 2)?]),
        "glRotatef" => Command::Rotate(f32a(a, 0)?, [f32a(a, 1)?, f32a(a, 2)?, f32a(a, 3)?]),
        "glOrtho" => Command::Ortho(doubles(a, 6)?.try_into().unwrap()),
        "gluPerspective" => Command::Perspective(doubles(a, 4)?.try_into().unwrap()),
        "gluLookAt" => Command::LookAt(doubles(a, 9)?.try_into().unwrap()),
        "glEnable" => Command::Enable(arg(a, 0)?),
        "glDisable" => Command::Disable(arg(a, 0)?),
        "glShadeModel" => Command::Shade(arg(a, 0)?),
        "glDepthFunc" => Command::Depth(arg(a, 0)?),
        "glCullFace" => Command::Cull(arg(a, 0)?),
        "glBlendFunc" => Command::Blend(arg(a, 0)?, arg(a, 1)?),
        "glColorMaterial" => Command::ColorMaterial(arg(a, 0)?, arg(a, 1)?),
        "glLightfv" => {
            let p = arg(a, 1)?;
            Command::Light(
                arg(a, 0)?,
                p,
                floats(m, arg(a, 2)?, if p == 0x1204 { 3 } else { 4 })?,
            )
        }
        "glLightModelfv" => {
            let p = arg(a, 0)?;
            let n = if p == 0x0b53 { 4 } else { 1 };
            Command::LightModel(p, floats(m, arg(a, 1)?, n)?)
        }
        "glMaterialfv" => {
            let p = arg(a, 1)?;
            Command::Material(
                arg(a, 0)?,
                p,
                floats(m, arg(a, 2)?, if p == 0x1601 { 1 } else { 4 })?,
            )
        }
        "glFogfv" => {
            let p = arg(a, 0)?;
            Command::Fog(p, floats(m, arg(a, 1)?, if p == 0x0b66 { 4 } else { 1 })?)
        }
        "glFogf" => Command::Fog(arg(a, 0)?, vec![f32a(a, 1)?]),
        "glFogi" => Command::Fog(arg(a, 0)?, vec![arg(a, 1)? as f32]),
        "glClearIndex" | "glIndexi" | "glDrawBuffer" | "glFlush" => return Ok(None),
        _ => return Err(format!("unsupported OpenGL operation {name}")),
    };
    Ok(Some(c))
}
