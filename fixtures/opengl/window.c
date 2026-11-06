/* CRT-free PE32 OpenGL fixture for the Win32/WGL guest path. */
typedef unsigned int U;
typedef int I;
typedef unsigned short W;
#define API __declspec(dllimport) __stdcall
#define GLAPI __declspec(dllimport) __cdecl
#define EXPORT __declspec(dllexport)

typedef struct {
    U size, style, proc, clsExtra, wndExtra, instance;
    U icon, cursor, background, menu, name, smallIcon;
} CLASS;
typedef struct {
    W size, version;
    U flags;
    unsigned char pixelType, colorBits, redBits, redShift;
    unsigned char greenBits, greenShift, blueBits, blueShift;
    unsigned char alphaBits, alphaShift;
    unsigned char accumBits, accumRedBits, accumGreenBits, accumBlueBits, accumAlphaBits;
    unsigned char depthBits, stencilBits, auxBuffers;
    unsigned char layerType, reserved;
    U layerMask, visibleMask, damageMask;
} PFD;

U API RegisterClassExA(CLASS *);
U API CreateWindowExA(U, const char *, const char *, U, I, I, I, I, U, U, U, U);
I API ShowWindow(U, I);
I API DestroyWindow(U);
U API GetDC(U);
I API ReleaseDC(U, U);
I API DefWindowProcA(U, U, U, U);
void API ExitProcess(U);
I API ChoosePixelFormat(U, PFD *);
I API SetPixelFormat(U, I, PFD *);
I API SwapBuffers(U);
U API wglCreateContext(U);
I API wglMakeCurrent(U, U);
I API wglDeleteContext(U);
U GLAPI glGetError(void);
void API glBegin(U);
void API glClear(U);
void API glClearColor(float, float, float, float);
void API glColor3ubv(const unsigned char *);
void API glEnd(void);
void API glVertex3f(float, float, float);
void API glViewport(I, I, I, I);

static char className[] = "WlamOpenGlFixture";
static char title[] = "OpenGL fixture";
static unsigned char blue[] = {0, 0, 255};
static unsigned char green[] = {0, 255, 0};
static volatile U fixture_window, fixture_context, fixture_swaps;
int _fltused = 0;

EXPORT I __stdcall WindowProc(U window, U message, U wparam, U lparam) {
    if (message == 0x81) return 1; /* WM_NCCREATE */
    return DefWindowProcA(window, message, wparam, lparam);
}

static CLASS windowClass = {48, 0, (U)WindowProc, 0, 0, 0x400000, 0, 0, 0, 0,
                            (U)className, 0};

void entry(void) {
    PFD pfd;
    U window, dc, context;
    I format;

    pfd.size = 40;
    pfd.version = 1;
    pfd.flags = 0x25;
    pfd.colorBits = 24;
    pfd.alphaBits = 8;
    pfd.depthBits = 24;
    pfd.stencilBits = 8;
    pfd.layerType = 0;
    if (!RegisterClassExA(&windowClass)) ExitProcess(10);
    window = CreateWindowExA(0, className, title, 0, 0, 0, 32, 24, 0, 0,
                             0x400000, 0);
    if (!window) ExitProcess(11);
    fixture_window = window;
    ShowWindow(window, 1);
    dc = GetDC(window);
    if (!dc) ExitProcess(12);
    format = ChoosePixelFormat(dc, &pfd);
    if (!format || !SetPixelFormat(dc, format, &pfd)) ExitProcess(13);
    context = wglCreateContext(dc);
    if (!context || !wglMakeCurrent(dc, context)) ExitProcess(14);
    fixture_context = context;

    /* glGetError is a zero-argument cdecl import; check its initial state. */
    if (glGetError() != 0) ExitProcess(17);

    glViewport(0, 0, 32, 24);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(0x00004000 | 0x00000100);
    glBegin(0x0004); /* GL_TRIANGLES */
    if (glGetError() != 0) ExitProcess(19); /* illegal query returns zero */
    glColor3ubv(blue);
    glVertex3f(-0.8f, -0.8f, 0.0f);
    glVertex3f( 0.8f, -0.8f, 0.0f);
    glVertex3f( 0.0f,  0.8f, 0.0f);
    glEnd();
    if (glGetError() != 0x0502) ExitProcess(20);
    if (glGetError() != 0) ExitProcess(18);
    glBegin(0x0000); /* GL_POINTS: also exercise the WebGL point packet. */
    glColor3ubv(green);
    glVertex3f(0.0f, 0.0f, 0.0f);
    glEnd();
    if (!SwapBuffers(dc)) ExitProcess(15);
    fixture_swaps++;

    wglMakeCurrent(0, 0);
    if (!wglDeleteContext(context)) ExitProcess(16);
    ReleaseDC(window, dc);
    DestroyWindow(window);
    ExitProcess(0);
}
