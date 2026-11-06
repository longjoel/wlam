const VERTEX_SHADER = `
attribute vec4 aClipPosition;
attribute float aEyeDistance;
attribute vec4 aFrontColor;
attribute vec4 aBackColor;
varying float vEyeDistance;
varying vec4 vFrontColor;
varying vec4 vBackColor;
void main() {
  gl_Position = aClipPosition;
  gl_PointSize = 1.0;
  vEyeDistance = aEyeDistance;
  vFrontColor = aFrontColor;
  vBackColor = aBackColor;
}`;

const FRAGMENT_SHADER = `
precision highp float;
uniform float uTwoSided;
uniform float uFogEnabled;
uniform float uFogMode;
uniform vec4 uFogColor;
uniform float uFogDensity;
uniform float uFogStart;
uniform float uFogEnd;
varying float vEyeDistance;
varying vec4 vFrontColor;
varying vec4 vBackColor;
void main() {
  vec4 color = (uTwoSided > 0.5 && !gl_FrontFacing) ? vBackColor : vFrontColor;
  if (uFogEnabled > 0.5) {
    float factor = 1.0;
    if (uFogMode < 0.5) {
      factor = clamp((uFogEnd - vEyeDistance) / (uFogEnd - uFogStart), 0.0, 1.0);
    } else if (uFogMode < 1.5) {
      factor = clamp(exp(-uFogDensity * vEyeDistance), 0.0, 1.0);
    } else {
      float scaled = uFogDensity * vEyeDistance;
      factor = clamp(exp(-(scaled * scaled)), 0.0, 1.0);
    }
    color.rgb = color.rgb * factor + uFogColor.rgb * (1.0 - factor);
  }
  gl_FragColor = color;
}`;

const MAGIC = 0x574c474c;
const FLOATS_PER_VERTEX = 13;
const TRIANGLES = 0x0004;
const POINTS = 0x0000;

function compile(gl, kind, source) {
  const shader = gl.createShader(kind);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(`WebGL shader compile failed: ${gl.getShaderInfoLog(shader)}`);
  }
  return shader;
}

function program(gl) {
  const result = gl.createProgram();
  gl.attachShader(result, compile(gl, gl.VERTEX_SHADER, VERTEX_SHADER));
  gl.attachShader(result, compile(gl, gl.FRAGMENT_SHADER, FRAGMENT_SHADER));
  gl.linkProgram(result);
  if (!gl.getProgramParameter(result, gl.LINK_STATUS)) {
    throw new Error(`WebGL program link failed: ${gl.getProgramInfoLog(result)}`);
  }
  return result;
}

export class WebGlRenderer {
  constructor(canvas) {
    this.canvas = canvas;
    this.gl = canvas.getContext('webgl', {
      // A Win32 guest window is opaque when composited, even when its
      // OpenGL pixel format carries an alpha channel for blending.
      alpha: false, depth: true, stencil: false, antialias: false,
      premultipliedAlpha: false, preserveDrawingBuffer: true,
    });
    if (!this.gl) throw new Error('This browser could not create a WebGL context.');
    const gl = this.gl;
    this.capabilities = Object.freeze({
      depthBits: gl.getParameter(gl.DEPTH_BITS),
      subpixelBits: gl.getParameter(gl.SUBPIXEL_BITS),
    });
    this.program = program(gl);
    this.buffer = gl.createBuffer();
    gl.useProgram(this.program);
    this.attributes = {
      clip: gl.getAttribLocation(this.program, 'aClipPosition'),
      distance: gl.getAttribLocation(this.program, 'aEyeDistance'),
      front: gl.getAttribLocation(this.program, 'aFrontColor'),
      back: gl.getAttribLocation(this.program, 'aBackColor'),
    };
    this.uniforms = Object.fromEntries([
      'uTwoSided', 'uFogEnabled', 'uFogMode', 'uFogColor',
      'uFogDensity', 'uFogStart', 'uFogEnd',
    ].map(name => [name, gl.getUniformLocation(this.program, name)]));
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    const stride = FLOATS_PER_VERTEX * 4;
    for (const [name, size, offset] of [
      ['clip', 4, 0], ['distance', 1, 16], ['front', 4, 20], ['back', 4, 36],
    ]) {
      gl.enableVertexAttribArray(this.attributes[name]);
      gl.vertexAttribPointer(this.attributes[name], size, gl.FLOAT, false, stride, offset);
    }
    gl.disable(gl.SCISSOR_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.colorMask(true, true, true, true);
    gl.depthMask(true);
    this.size = [0, 0];
  }

  resize(width, height) {
    if (width <= 0 || height <= 0) throw new Error(`Invalid WebGL surface ${width}×${height}`);
    if (this.size[0] === width && this.size[1] === height) return;
    this.canvas.width = width;
    this.canvas.height = height;
    this.size = [width, height];
    const gl = this.gl;
    gl.viewport(0, 0, width, height);
    gl.disable(gl.SCISSOR_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.colorMask(true, true, true, true);
    gl.depthMask(true);
  }

  render(words) {
    if (!(words instanceof Uint32Array) || words.length < 5
        || words[0] !== MAGIC || words[1] !== 1) {
      throw new Error('Invalid WebGL frame packet header.');
    }
    const [, , width, height, operationCount] = words;
    this.resize(width, height);
    const gl = this.gl;
    gl.useProgram(this.program);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    let at = 5;
    for (let operation = 0; operation < operationCount; operation++) {
      const opcode = words[at++];
      if (opcode === 0) {
        const mask = words[at++];
        const color = Array.from(words.subarray(at, at + 4), bits => this.float(bits));
        at += 4;
        const depth = this.float(words[at++]);
        gl.depthMask(true);
        gl.colorMask(true, true, true, true);
        gl.clearColor(...color);
        gl.clearDepth(depth);
        gl.clear(mask);
      } else if (opcode === 1) {
        const vertexCount = words[at++];
        const primitive = words[at++];
        const viewport = Array.from(words.subarray(at, at + 4), value => value | 0);
        at += 4;
        const flags = words[at++];
        const depthFunc = words[at++];
        const cullMode = words[at++];
        const blendSrc = words[at++];
        const blendDst = words[at++];
        const fogMode = words[at++];
        const fogColor = Array.from(words.subarray(at, at + 4), bits => this.float(bits));
        at += 4;
        const fogDensity = this.float(words[at++]);
        const fogStart = this.float(words[at++]);
        const fogEnd = this.float(words[at++]);
        if ((primitive === TRIANGLES && vertexCount % 3 !== 0)
            || (primitive !== TRIANGLES && primitive !== POINTS)
            || vertexCount === 0) {
          throw new Error(`Unsupported WebGL primitive packet (${primitive}, ${vertexCount}).`);
        }
        this.draw({
          vertexCount, primitive, viewport, flags, depthFunc, cullMode, blendSrc, blendDst,
          fogMode, fogColor, fogDensity, fogStart, fogEnd,
        }, words.subarray(at, at + vertexCount * FLOATS_PER_VERTEX));
        at += vertexCount * FLOATS_PER_VERTEX;
      } else {
        throw new Error(`Unknown WebGL frame operation ${opcode}.`);
      }
    }
    if (at !== words.length) throw new Error('Trailing data in WebGL frame packet.');
  }

  float(bits) {
    const view = new DataView(new ArrayBuffer(4));
    view.setUint32(0, bits, true);
    return view.getFloat32(0, true);
  }

  draw(batch, vertexWords) {
    const gl = this.gl;
    const f = batch.flags;
    gl.viewport(...batch.viewport);
    this.toggle(gl.DEPTH_TEST, (f & 1) !== 0);
    if (f & 1) gl.depthFunc(batch.depthFunc);
    this.toggle(gl.CULL_FACE, (f & 2) !== 0);
    if (f & 2) {
      gl.frontFace((f & 4) !== 0 ? gl.CCW : gl.CW);
      gl.cullFace(batch.cullMode);
    }
    this.toggle(gl.BLEND, (f & 16) !== 0);
    if (f & 16) gl.blendFunc(batch.blendSrc, batch.blendDst);
    this.toggle(gl.DITHER, (f & 32) !== 0);
    gl.uniform1f(this.uniforms.uTwoSided, (f & 8) !== 0 ? 1 : 0);
    gl.uniform1f(this.uniforms.uFogEnabled, (f & 64) !== 0 ? 1 : 0);
    gl.uniform1f(this.uniforms.uFogMode, batch.fogMode - 0x0800);
    gl.uniform4fv(this.uniforms.uFogColor, batch.fogColor);
    gl.uniform1f(this.uniforms.uFogDensity, batch.fogDensity);
    gl.uniform1f(this.uniforms.uFogStart, batch.fogStart);
    gl.uniform1f(this.uniforms.uFogEnd, batch.fogEnd);
    const floats = new Float32Array(vertexWords.length);
    const bitsView = new DataView(vertexWords.buffer, vertexWords.byteOffset, vertexWords.byteLength);
    for (let i = 0; i < floats.length; i++) floats[i] = bitsView.getFloat32(i * 4, true);
    gl.bufferData(gl.ARRAY_BUFFER, floats, gl.DYNAMIC_DRAW);
    gl.drawArrays(batch.primitive, 0, batch.vertexCount);
  }

  toggle(capability, enabled) {
    if (enabled) this.gl.enable(capability);
    else this.gl.disable(capability);
  }
}
