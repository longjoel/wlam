/* CRT-free game-oriented GDI behavior probe. Runs unchanged on Windows. */
typedef unsigned char B; typedef unsigned int U; typedef int I; typedef unsigned short W;
#define API __declspec(dllimport) __stdcall
typedef struct { U size; I width,height; W planes,bits; U compression,imageSize; I xppm,yppm; U used,important; } INFO;
typedef struct { I left,top,right,bottom; } RECT;
U API CreateCompatibleDC(U); U API CreateDIBSection(U,INFO*,U,U**,U,U);
U API SelectObject(U,U); I API DeleteObject(U); I API DeleteDC(U);
U API CreateSolidBrush(U); U API GetStockObject(I);
I API SaveDC(U); I API RestoreDC(U,I);
U API SetTextColor(U,U); U API GetTextColor(U); U API SetBkColor(U,U); U API GetBkColor(U);
I API SetBkMode(U,I); I API GetBkMode(U);
I API IntersectClipRect(U,I,I,I,I); I API GetClipBox(U,RECT*);
I API PatBlt(U,I,I,I,I,U); I API BitBlt(U,I,I,I,I,U,I,I,U);
I API TextOutA(U,I,I,const char*,I); I API ExtTextOutA(U,I,I,U,RECT*,const char*,U,I*);
U API GetStdHandle(U); I API WriteFile(U,const void*,U,U*,void*); void API ExitProcess(U);
static U output, failures;
static void bytes(const char *s,U n) { U written; WriteFile(output,s,n,&written,0); }
static void str(const char *s) { U n=0;while(s[n])n++;bytes(s,n); }
static void number(U n) { char buffer[11];U at=11;do{buffer[--at]='0'+n%10;n/=10;}while(n);bytes(buffer+at,11-at); }
static void result(const char *name,U observed,U expected) {
    str("{\"case\":\"");str(name);str("\",\"value\":");number(observed);str(",\"expected\":");number(expected);str("}\n");
    if(observed!=expected)failures++;
}
static U dc,bitmap,oldBitmap,*pixels;
static INFO info={40,8,-8,1,32,0,0,0,0,0,0};
static struct { INFO header; U colors[256]; } indexed={{40,3,-2,1,8,0,0,0,0,256,0},{0}};
void entry(void) {
    U i,red,blue,oldBrush,saved,second,other,stockFont,hasText;
    RECT clip,opaque={1,1,3,3},indexedRect={0,0,1,1};
    U dc8,bmp8,old8,*bits8;B *indices;
    output=GetStdHandle((U)-11);
    dc=CreateCompatibleDC(0);bitmap=CreateDIBSection(dc,&info,0,&pixels,0,0);
    if(!dc||!bitmap||!pixels)ExitProcess(10);
    oldBitmap=SelectObject(dc,bitmap);
    result("default.text-color",GetTextColor(dc),0);
    result("default.background-color",GetBkColor(dc),0xffffff);
    result("default.background-mode",GetBkMode(dc),2);
    stockFont=GetStockObject(13);
    result("stock.system-font-selectable",SelectObject(dc,stockFont)!=0,1);
    for(i=0;i<64;i++)pixels[i]=0x00112233;
    red=CreateSolidBrush(0x000000ff);blue=CreateSolidBrush(0x00ff0000);
    oldBrush=SelectObject(dc,red);
    result("brush.selection-returns-previous",oldBrush!=0,1);
    saved=SaveDC(dc);result("save.first-level",saved,1);
    SetTextColor(dc,0x00123456);SetBkColor(dc,0x00654321);SetBkMode(dc,1);
    SelectObject(dc,blue);
    result("clip.intersect",IntersectClipRect(dc,2,2,6,6),2);
    result("fill.pattern-copy",PatBlt(dc,0,0,8,8,0x00f00021)!=0,1);
    result("fill.clipped-inside",pixels[2*8+2]&0xffffff,0x000000ff);
    result("fill.clipped-outside",pixels[0]&0xffffff,0x00112233);
    result("restore.relative",RestoreDC(dc,-1)!=0,1);
    result("restore.text-color",GetTextColor(dc),0);
    result("restore.background-color",GetBkColor(dc),0xffffff);
    result("restore.background-mode",GetBkMode(dc),2);
    result("restore.clip-type",GetClipBox(dc,&clip),2);
    result("restore.clip-right",clip.right,8);
    result("restore.brush",SelectObject(dc,oldBrush)==red,1);
    result("delete.deselected-brush",DeleteObject(red)!=0,1);
    result("delete.second-brush",DeleteObject(blue)!=0,1);
    saved=SaveDC(dc);SetTextColor(dc,1);second=SaveDC(dc);SetTextColor(dc,2);
    result("save.second-level",second,2);
    result("restore.absolute",RestoreDC(dc,saved)!=0,1);
    result("restore.absolute-color",GetTextColor(dc),0);
    result("restore.discards-newer",RestoreDC(dc,second)!=0,0);
    for(i=0;i<64;i++)pixels[i]=i;
    result("blit.overlap",BitBlt(dc,1,0,7,1,dc,0,0,0x00cc0020)!=0,1);
    for(i=1;i<8;i++)result("blit.overlap-pixel",pixels[i]&0xffffff,i-1);
    result("blit.source-independent-null-dc",BitBlt(dc,0,7,1,1,0,0,0,0x42)!=0,1);
    result("blit.source-independent-pixel",pixels[56]&0xffffff,0);
    pixels[0]=0x000ff00f;pixels[8]=0x00333333;
    result("blit.source-xor",BitBlt(dc,0,1,1,1,dc,0,0,0x00660046)!=0,1);
    result("blit.xor-pixel",pixels[8]&0xffffff,0x003cc33c);
    other=CreateCompatibleDC(0);
    result("bitmap.cannot-select-twice",SelectObject(other,bitmap)!=0,0);
    DeleteDC(other);
    for(i=0;i<64;i++)pixels[i]=0;
    SetBkColor(dc,0x0000ff00);SetBkMode(dc,1);
    result("text.opaque-empty",ExtTextOutA(dc,0,0,2,&opaque,0,0,0)!=0,1);
    result("text.opaque-inside",pixels[9]&0xffffff,0x0000ff00);
    result("text.opaque-outside",pixels[0]&0xffffff,0);
    SetTextColor(dc,0x00ffffff);
    result("text.output",TextOutA(dc,0,0,"M",1)!=0,1);
    hasText=0;for(i=0;i<64;i++)if((pixels[i]&0xffffff)==0xffffff)hasText=1;
    result("text.foreground-present",hasText,1);
    for(i=0;i<256;i++)indexed.colors[i]=0x0000ff00;
    indexed.colors[9]=0;indexed.colors[42]=0x00ff0000;indexed.colors[200]=0x00ffffff;
    dc8=CreateCompatibleDC(0);bmp8=CreateDIBSection(dc8,&indexed.header,0,&bits8,0,0);
    if(!dc8||!bmp8||!bits8)ExitProcess(11);
    old8=SelectObject(dc8,bmp8);indices=(B*)bits8;
    for(i=0;i<8;i++)indices[i]=200;
    indices[3]=0x7a;indices[7]=0x7b;
    result("indexed.black-fill",PatBlt(dc8,0,0,3,2,0x42)!=0,1);
    result("indexed.black-index",indices[0],9);
    result("indexed.row-padding",indices[3],0x7a);
    result("indexed.second-row",indices[4],9);
    result("indexed.second-padding",indices[7],0x7b);
    SetBkColor(dc8,0x000000ff);
    result("indexed.opaque-fill",ExtTextOutA(dc8,0,0,2,&indexedRect,0,0,0)!=0,1);
    result("indexed.red-index",indices[0],42);
    result("indexed.blit-to-rgb",BitBlt(dc,0,0,1,1,dc8,0,0,0x00cc0020)!=0,1);
    result("indexed.rgb-result",pixels[0]&0xffffff,0x00ff0000);
    SelectObject(dc8,old8);DeleteObject(bmp8);DeleteDC(dc8);
    SelectObject(dc,oldBitmap);DeleteObject(bitmap);DeleteDC(dc);
    /* Font glyph shapes differ by Windows version; foreground presence is
       checked separately from exact raster-operation comparisons. */
    ExitProcess(failures);
}
