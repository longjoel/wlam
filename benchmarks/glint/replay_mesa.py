"""Replay the captured settled TOLYMPIC GL stream using Mesa llvmpipe.

Requires a Linux Mesa install with EGL/GL/GLU and a C compiler. If Pillow is
available, also compares the readback with the browser capture and writes a
top-down PNG next to this script.
"""

import json
import argparse
import os
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
TRACE = HERE / "tolympic-api-trace.json"
FRAME_REFERENCES = {
    68: ("tolympic-before-d.png", "tolympic-mesa-replay.png"),
    90: ("tolympic-resized-settled.png", "tolympic-mesa-resized-settled.png"),
    102: ("tolympic-after-d.png", "tolympic-mesa-doughnuts-blend.png"),
    108: ("tolympic-after-d-no-blend.png", "tolympic-mesa-doughnuts-opaque.png"),
}

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--frame", choices=[str(frame) for frame in FRAME_REFERENCES], default="68")
parser.add_argument("--reference", type=Path, help="override the saved browser PNG for comparison")
parser.add_argument("--output", type=Path, help="override the Mesa PNG output path")
options = parser.parse_args()
FRAME = int(options.frame)
reference_name, output_name = FRAME_REFERENCES[FRAME]
REFERENCE = options.reference or HERE / reference_name
OUTPUT = options.output or HERE / output_name
RAW_OUTPUT = Path(f"/tmp/tolympic-mesa-replay-{FRAME}.rgba")

trace = json.loads(TRACE.read_text())
viewport = next(
    call["args"]
    for call in reversed(trace["apiCalls"])
    if call["library"] == "OPENGL32.dll"
    and call["name"] == "glViewport"
    and call["frame"] <= FRAME
)
WIDTH, HEIGHT = viewport[2:4]
calls = [
    call
    for call in trace["apiCalls"]
    if call["library"] in ("OPENGL32.dll", "GLU32.dll") and call["frame"] <= FRAME
]


def fbits(value):
    return f"F(0x{value:08x}u)"


def doubles(call):
    return call.get("doubleValues", [])


def emit(c):
    name = c["name"]
    args = c.get("args", [])
    if name in ("wglCreateContext", "wglMakeCurrent", "wglDeleteContext"):
        return ""
    if name in ("glClearColor", "glTranslatef", "glRotatef", "glNormal3f", "glVertex3f"):
        return f"{name}({','.join(fbits(value) for value in args)});"
    if name == "glClearIndex":
        return f"glClearIndex({fbits(args[0])});"
    if name in ("glLoadIdentity", "glEnd", "glEndList", "glFlush", "glPushMatrix", "glPopMatrix"):
        return f"{name}();"
    if name in (
        "glGenLists", "glNewList", "glBegin", "glEnable", "glDisable", "glCullFace",
        "glShadeModel", "glBlendFunc", "glMatrixMode", "glClear", "glCallList",
    ):
        return f"{name}({','.join(f'0x{value:x}u' for value in args)});"
    if name == "glViewport":
        return f"glViewport({args[0]},{args[1]},{args[2]},{args[3]});"
    if name == "glClearDepth":
        return f"glClearDepth({doubles(c)[0]:.17g});"
    if name in ("glLightfv", "glLightModelfv", "glMaterialfv"):
        values = c["pointedValues"] + [0.0] * max(0, 4 - len(c["pointedValues"]))
        literals = []
        for value in values:
            literal = f"{value:.9g}"
            if "." not in literal and "e" not in literal:
                literal += ".0"
            literals.append(literal + "f")
        params = ",".join(literals)
        if name == "glLightModelfv":
            invocation = f"{name}(0x{args[0]:x}u,p);"
        else:
            invocation = f"{name}(0x{args[0]:x}u,0x{args[1]:x}u,p);"
        return f"{{const float p[]={{ {params} }};{invocation}}}"
    if name == "glColorMaterial":
        return f"glColorMaterial(0x{args[0]:x}u,0x{args[1]:x}u);"
    if name == "glColor3ubv":
        values = ",".join(map(str, c["pointedValues"]))
        return f"{{const unsigned char p[]={{{values}}};glColor3ubv(p);}}"
    if name in ("gluPerspective", "gluLookAt"):
        values = ",".join(f"{value:.17g}" for value in doubles(c))
        return f"{name}({values});"
    raise ValueError(name)

src=r'''#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <dlfcn.h>
typedef void* EGLDisplay; typedef void* EGLConfig; typedef void* EGLSurface; typedef void* EGLContext; typedef int EGLint; typedef unsigned int EGLBoolean; typedef unsigned int EGLenum;
extern EGLDisplay eglGetPlatformDisplay(EGLenum, void*, const EGLint*);
extern EGLBoolean eglInitialize(EGLDisplay,EGLint*,EGLint*);
extern EGLBoolean eglBindAPI(EGLenum);
extern EGLBoolean eglChooseConfig(EGLDisplay,const EGLint*,EGLConfig*,EGLint,EGLint*);
extern EGLSurface eglCreatePbufferSurface(EGLDisplay,EGLConfig,const EGLint*);
extern EGLContext eglCreateContext(EGLDisplay,EGLConfig,EGLContext,const EGLint*);
extern EGLBoolean eglMakeCurrent(EGLDisplay,EGLSurface,EGLSurface,EGLContext);
extern EGLint eglGetError(void);
extern const char *glGetString(unsigned int);
extern void glGetIntegerv(unsigned int,int*);
extern void glReadBuffer(unsigned int);
extern void glReadPixels(int,int,int,int,unsigned int,unsigned int,void*);
extern void glFinish(void);
extern void glClearColor(float,float,float,float); extern void glClearIndex(float); extern void glLoadIdentity(void);
extern unsigned int glGenLists(int); extern void glNewList(unsigned int,unsigned int); extern void glBegin(unsigned int); extern void glNormal3f(float,float,float); extern void glVertex3f(float,float,float); extern void glEnd(void); extern void glEndList(void);
extern void glEnable(unsigned int); extern void glDisable(unsigned int); extern void glCullFace(unsigned int); extern void glClearDepth(double); extern void glLightfv(unsigned int,unsigned int,const float*); extern void glLightModelfv(unsigned int,const float*); extern void glMaterialfv(unsigned int,unsigned int,const float*); extern void glColorMaterial(unsigned int,unsigned int); extern void glShadeModel(unsigned int); extern void glBlendFunc(unsigned int,unsigned int); extern void glMatrixMode(unsigned int); extern void glViewport(int,int,int,int); extern void glPushMatrix(void); extern void glClear(unsigned int); extern void glColor3ubv(const unsigned char*); extern void glTranslatef(float,float,float); extern void glRotatef(float,float,float,float); extern void glCallList(unsigned int); extern void glPopMatrix(void); extern void glFlush(void);
extern void gluPerspective(double,double,double,double); extern void gluLookAt(double,double,double,double,double,double,double,double,double);
static float F(uint32_t x){union{uint32_t u;float f;}v;v.u=x;return v.f;}
int main(){
 EGLDisplay d=eglGetPlatformDisplay(0x31dd,NULL,NULL); EGLint ma=0,mi=0;
 if(!d||!eglInitialize(d,&ma,&mi)||!eglBindAPI(0x30a2)){fprintf(stderr,"EGL init failed %x\n",eglGetError());return 2;}
 EGLint ca[]={0x3033,1,0x3040,8,0x3024,8,0x3023,8,0x3022,8,0x3021,8,0x3025,24,0x3026,8,0x3038}; EGLConfig cfg=0; EGLint count=0;
 if(!eglChooseConfig(d,ca,&cfg,1,&count)||!count){fprintf(stderr,"EGL config failed %x\n",eglGetError());return 3;}
 EGLint pa[]={0x3057,WIDTH,0x3056,HEIGHT,0x3038}; EGLSurface s=eglCreatePbufferSurface(d,cfg,pa); EGLContext c=eglCreateContext(d,cfg,NULL,NULL);
 if(!s||!c||!eglMakeCurrent(d,s,s,c)){fprintf(stderr,"EGL context failed %x\n",eglGetError());return 4;}
 fprintf(stderr,"Mesa GL: %s / %s\n",glGetString(0x1f02),glGetString(0x1f01));
 int depth_bits=0,alpha_bits=0,stencil_bits=0,subpixel_bits=0; glGetIntegerv(0x0d56,&depth_bits); glGetIntegerv(0x0d55,&alpha_bits); glGetIntegerv(0x0d57,&stencil_bits); glGetIntegerv(0x0d50,&subpixel_bits);
 fprintf(stderr,"pbuffer format: depth=%d alpha=%d stencil=%d subpixel_bits=%d\n",depth_bits,alpha_bits,stencil_bits,subpixel_bits);
 glReadBuffer(0x0404);
'''
src+='\n'.join(emit(call) for call in calls)+'\n'
src+=r''' glFinish(); unsigned char* p=malloc(WIDTH*HEIGHT*4); if(!p)return 5; glReadPixels(0,0,WIDTH,HEIGHT,0x1908,0x1401,p); FILE*f=fopen("RAW_OUTPUT","wb"); fwrite(p,1,WIDTH*HEIGHT*4,f); fclose(f); if(WIDTH==600&&HEIGHT==450){float z=1.0f;glReadPixels(389,236,1,1,0x1902,0x1406,&z);fprintf(stderr,"Mesa depth at browser pixel (389,213): %.9g, 24-bit %.0f\n",z,z*16777215.0f);glReadPixels(299,137,1,1,0x1902,0x1406,&z);fprintf(stderr,"Mesa depth at browser pixel (299,312): %.9g, 24-bit %.0f\n",z,z*16777215.0f);glReadPixels(208,281,1,1,0x1902,0x1406,&z);fprintf(stderr,"Mesa depth at browser pixel (208,168): %.12g, 24-bit %.0f; rgba=(%u,%u,%u,%u)\n",z,z*16777215.0f,p[(281*WIDTH+208)*4],p[(281*WIDTH+208)*4+1],p[(281*WIDTH+208)*4+2],p[(281*WIDTH+208)*4+3]);} return 0; }
'''
src = (src.replace("WIDTH", str(WIDTH)).replace("HEIGHT", str(HEIGHT))
       .replace("RAW_OUTPUT", str(RAW_OUTPUT)))
source = Path(f"/tmp/tolympic-mesa-replay-{FRAME}.c")
binary = Path(f"/tmp/tolympic-mesa-replay-{FRAME}")
source.write_text(src)
subprocess.run(
    [
        "cc", str(source), "-o", str(binary), "-Wno-deprecated-declarations",
        "-Wno-implicit-function-declaration", "-l:libEGL.so.1", "-l:libGL.so.1",
        "-l:libGLU.so.1", "-ldl",
    ],
    check=True,
)
mesa_env = os.environ.copy()
mesa_env["LIBGL_ALWAYS_SOFTWARE"] = "1"
subprocess.run([str(binary)], check=True, env=mesa_env)
print(f"replayed {len(calls)} GL/GLU calls through trace frame {FRAME} at {WIDTH}x{HEIGHT}")

try:
    from PIL import Image
except ImportError:
    print(f"Pillow unavailable; raw Mesa readback is {RAW_OUTPUT}")
else:
    mesa = RAW_OUTPUT.read_bytes()
    width, height = WIDTH, HEIGHT
    row_size = width * 4
    top_down = b"".join(
        mesa[y * row_size : (y + 1) * row_size]
        for y in range(height - 1, -1, -1)
    )
    current_rgb = Image.frombytes("RGBA", (width, height), top_down).convert("RGB").tobytes()
    reference_rgb = Image.open(REFERENCE).convert("RGB").tobytes()
    exact_pixels = 0
    greater_than_20 = 0
    rgb_difference_sum = 0
    for offset in range(0, len(reference_rgb), 3):
        current = current_rgb[offset : offset + 3]
        expected = reference_rgb[offset : offset + 3]
        differences = [abs(current[channel] - expected[channel]) for channel in range(3)]
        exact_pixels += current == expected
        greater_than_20 += max(differences) > 20
        rgb_difference_sum += sum(differences)
    # The browser's window surface is opaque. Alpha readback from an EGL
    # pbuffer is implementation-specific for this visual comparison.
    opaque_rgba = bytearray(top_down)
    opaque_rgba[3::4] = b"\xff" * (width * height)
    Image.frombytes("RGBA", (width, height), opaque_rgba).save(OUTPUT)
    pixel_count = width * height
    print(f"exact RGB pixels: {exact_pixels}/{pixel_count}")
    print(f"pixels differing by >20 RGB levels: {greater_than_20}")
    print(f"mean absolute RGB channel difference: {rgb_difference_sum / (pixel_count * 3):.6f}")
    print(f"Mesa capture: {OUTPUT}")
