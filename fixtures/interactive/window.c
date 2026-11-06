/* CRT-free PE32 fixture. Keep declarations independent of a Windows SDK. */
typedef unsigned int U; typedef int I; typedef unsigned short W;
#define API __declspec(dllimport) __stdcall
#define EXPORT __declspec(dllexport)
typedef struct { U hwnd, message, wparam, lparam, time; I x,y; } MSG;
typedef struct { U size,style,proc,clsExtra,wndExtra,instance,icon,cursor,background,menu,name,smallIcon; } CLASS;
typedef struct { U size; I width,height; W planes,bits; U compression,imageSize; I xppm,yppm; U used,important; } BITMAPINFO;
U API RegisterClassExA(CLASS*); U API CreateWindowExA(U,const char*,const char*,U,I,I,I,I,U,U,U,U);
I API ShowWindow(U,I); I API UpdateWindow(U); I API GetMessageA(MSG*,U,U,U);
I API TranslateMessage(MSG*); I API DispatchMessageA(MSG*); I API DefWindowProcA(U,U,U,U);
I API InvalidateRect(U,U,I); I API DestroyWindow(U); void API PostQuitMessage(I);
U API BeginPaint(U,void*); I API EndPaint(U,void*);
U API CreateCompatibleDC(U); U API CreateDIBSection(U,BITMAPINFO*,U,U**,U,U);
U API SelectObject(U,U); I API BitBlt(U,I,I,I,I,U,I,I,U);
I API DeleteDC(U); I API DeleteObject(U); void API ExitProcess(U);
static char className[]="WlamFixture"; static char title[]="Interactive PE fixture";
static U dc,bitmap,oldBitmap; static U *pixels; static U color=0x00123456;
static U surfaceWidth=32,surfaceHeight=24;
EXPORT volatile U fixture_paints,fixture_clicks,fixture_keys,fixture_closed;
static BITMAPINFO info={40,32,-24,1,32,0,0,0,0,0,0};
static void fill(void) { U i; for(i=0;i<surfaceWidth*surfaceHeight;i++) pixels[i]=color; }
static I createSurface(void) {
    dc=CreateCompatibleDC(0);
    info.width=surfaceWidth; info.height=-(I)surfaceHeight;
    bitmap=CreateDIBSection(dc,&info,0,&pixels,0,0);
    if(!dc || !bitmap || !pixels) return -1;
    oldBitmap=SelectObject(dc,bitmap); fill(); return 0;
}
static void destroySurface(void) {
    if(dc) SelectObject(dc,oldBitmap);
    if(bitmap) DeleteObject(bitmap);
    if(dc) DeleteDC(dc);
    dc=bitmap=oldBitmap=0; pixels=0;
}
EXPORT I __stdcall WindowProc(U window,U message,U wparam,U lparam) {
    if(message==0x81) return 1;
    if(message==5) {
        U w=lparam&0xffff,h=lparam>>16;
        if(w&&h&&(w!=surfaceWidth||h!=surfaceHeight)) {
            destroySurface(); surfaceWidth=w; surfaceHeight=h;
            if(createSurface()) return -1;
            InvalidateRect(window,0,0);
        }
        return 0;
    }
    if(message==1) {
        return createSurface();
    }
    if(message==15) {
        U paint[16]; U target=BeginPaint(window,paint);
        BitBlt(target,0,0,surfaceWidth,surfaceHeight,dc,0,0,0x00cc0020);
        EndPaint(window,paint); fixture_paints++; return 0;
    }
    if(message==0x201) {fixture_clicks++; color=0x00e04020; fill(); InvalidateRect(window,0,0); return 0;}
    if(message==0x100) {
        fixture_keys++;
        if(wparam==27) DestroyWindow(window);
        else {color=0x0020c060; fill(); InvalidateRect(window,0,0);}
        return 0;
    }
    if(message==0x10) {DestroyWindow(window); return 0;}
    if(message==2) {
        destroySurface();
        fixture_closed=1; PostQuitMessage(0); return 0;
    }
    return DefWindowProcA(window,message,wparam,lparam);
}
static CLASS windowClass={48,0,(U)WindowProc,0,0,0x400000,0,0,0,0,(U)className,0};
void entry(void) {
    MSG message; U window;
    if(!RegisterClassExA(&windowClass)) ExitProcess(10);
    window=CreateWindowExA(0,className,title,0,0,0,32,24,0,0,0x400000,0);
    if(!window) ExitProcess(11);
    ShowWindow(window,1); UpdateWindow(window);
    while(GetMessageA(&message,0,0,0)>0) {TranslateMessage(&message); DispatchMessageA(&message);}
    ExitProcess(0);
}
