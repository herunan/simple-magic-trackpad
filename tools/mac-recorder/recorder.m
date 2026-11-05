// mac-recorder: log macOS trackpad scrolling (finger + momentum phases) and
// the raw finger contacts, to compare the daemon's glide with the real thing.
//
// Build:  clang -fobjc-arc -O2 -framework Cocoa -F/System/Library/PrivateFrameworks \
//           -framework MultitouchSupport recorder.m -o recorder
// Run:    ./recorder [outdir]   (default ~/momentum-capture), then scroll over
//         the window. Files: scroll.csv, touch.csv. Quit with Cmd-Q.
//
// scroll.csv, one row per NSEvent scroll event:
//   t (s since boot), phase, momentumPhase, scrollingDeltaX/Y (points, what
//   apps scroll by), deltaX/Y (lines), raw HID scroll X/Y (unaccelerated
//   counts, via CGEventCopyIOHIDEvent like WebKit's WebEventFactory.mm),
//   CG fixed-point and point deltas.
// touch.csv, one row per contact per multitouch frame (MultitouchSupport),
//   for every trackpad (device = builtin/external):
//   t, frame, id, state, normalized x/y, normalized velocity x/y, size, and
//   the sensor surface size (mm) so positions can be turned into mm.
#import <Cocoa/Cocoa.h>
#include <dlfcn.h>
#include <stdio.h>

// ---- private IOHIDEvent access (as in WebKit) ---------------------------
typedef struct __IOHIDEvent *IOHIDEventRef;
static IOHIDEventRef (*CGEventCopyIOHIDEvent_)(CGEventRef);
static double (*IOHIDEventGetFloatValue_)(IOHIDEventRef, uint32_t);
#define kIOHIDEventTypeScroll 6
#define FIELD(type, n) (((type) << 16) | (n))

// ---- private MultitouchSupport ------------------------------------------
typedef struct { float x, y; } MTPoint;
typedef struct { MTPoint pos, vel; } MTReadout;
typedef struct {
    int frame;
    double timestamp;
    int identifier, state, fingerId, handId;
    MTReadout normalized;
    float size;
    int zero1;
    float angle, majorAxis, minorAxis;
    MTReadout mm;
    int zero2[2];
    float unk2;
} MTFinger;
typedef void *MTDeviceRef;
typedef int (*MTContactCallback)(MTDeviceRef, MTFinger *, int, double, int);
extern MTDeviceRef MTDeviceCreateDefault(void);
extern CFArrayRef MTDeviceCreateList(void);
extern bool MTDeviceIsBuiltIn(MTDeviceRef);
extern void MTRegisterContactFrameCallback(MTDeviceRef, MTContactCallback);
extern void MTDeviceStart(MTDeviceRef, int);
extern int MTDeviceGetSensorSurfaceDimensions(MTDeviceRef, int *, int *);

static FILE *scrollFile, *touchFile;
#define MAXDEV 8
static MTDeviceRef devs[MAXDEV];
static int surfW[MAXDEV], surfH[MAXDEV], builtin[MAXDEV], ndev; // hundredths of a mm
static long nScroll, nTouch;
static NSTextField *label;

static int touchCallback(MTDeviceRef dev, MTFinger *f, int n, double ts, int frame) {
    int d = 0;
    while (d < ndev && devs[d] != dev) d++;
    if (d == ndev) return 0;
    const char *kind = builtin[d] ? "builtin" : "external";
    for (int i = 0; i < n; i++)
        fprintf(touchFile, "%s,%.6f,%d,%d,%d,%.6f,%.6f,%.6f,%.6f,%.4f,%.2f,%.2f\n", kind, ts, frame,
                f[i].identifier, f[i].state, f[i].normalized.pos.x, f[i].normalized.pos.y,
                f[i].normalized.vel.x, f[i].normalized.vel.y, f[i].size, surfW[d] / 100.0,
                surfH[d] / 100.0);
    if (n == 0)
        fprintf(touchFile, "%s,%.6f,%d,-1,0,,,,,,%.2f,%.2f\n", kind, ts, frame, surfW[d] / 100.0,
                surfH[d] / 100.0);
    nTouch++;
    return 0;
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        NSString *dir = argc > 1 ? @(argv[1])
                                 : [NSHomeDirectory() stringByAppendingPathComponent:@"momentum-capture"];
        [[NSFileManager defaultManager] createDirectoryAtPath:dir withIntermediateDirectories:YES
                                                   attributes:nil error:nil];
        scrollFile = fopen([[dir stringByAppendingPathComponent:@"scroll.csv"] UTF8String], "w");
        touchFile = fopen([[dir stringByAppendingPathComponent:@"touch.csv"] UTF8String], "w");
        if (!scrollFile || !touchFile) { perror("open"); return 1; }
        fprintf(scrollFile, "t,phase,momentum,sdx,sdy,dx,dy,hid_x,hid_y,fixed_x,fixed_y,point_x,point_y\n");
        fprintf(touchFile, "device,t,frame,id,state,nx,ny,nvx,nvy,size,surf_w_mm,surf_h_mm\n");

        void *cg = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_LAZY);
        void *iok = dlopen("/System/Library/Frameworks/IOKit.framework/IOKit", RTLD_LAZY);
        CGEventCopyIOHIDEvent_ = cg ? dlsym(cg, "CGEventCopyIOHIDEvent") : NULL;
        IOHIDEventGetFloatValue_ = iok ? dlsym(iok, "IOHIDEventGetFloatValue") : NULL;
        if (!CGEventCopyIOHIDEvent_ || !IOHIDEventGetFloatValue_)
            fprintf(stderr, "warning: no IOHIDEvent access; hid_x/hid_y will be empty\n");

        CFArrayRef list = MTDeviceCreateList();
        for (CFIndex i = 0; list && i < CFArrayGetCount(list) && ndev < MAXDEV; i++) {
            MTDeviceRef dev = (MTDeviceRef)CFArrayGetValueAtIndex(list, i);
            int d = ndev++;
            devs[d] = dev;
            builtin[d] = MTDeviceIsBuiltIn(dev);
            MTDeviceGetSensorSurfaceDimensions(dev, &surfW[d], &surfH[d]);
            fprintf(stderr, "multitouch device %d: %s, %.1f x %.1f mm\n", d,
                    builtin[d] ? "built-in" : "external", surfW[d] / 100.0, surfH[d] / 100.0);
            MTRegisterContactFrameCallback(dev, touchCallback);
            MTDeviceStart(dev, 0);
        }
        if (!ndev) fprintf(stderr, "warning: no multitouch device; touch.csv stays empty\n");

        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        NSMenu *bar = [NSMenu new], *appMenu = [NSMenu new];
        NSMenuItem *item = [NSMenuItem new];
        [bar addItem:item];
        [appMenu addItemWithTitle:@"Quit" action:@selector(terminate:) keyEquivalent:@"q"];
        [item setSubmenu:appMenu];
        [NSApp setMainMenu:bar];

        NSWindow *w = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 900, 700)
                                                  styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskResizable
                                                    backing:NSBackingStoreBuffered defer:NO];
        [w setTitle:@"Momentum recorder: flick over this window, Cmd-Q to finish"];
        label = [NSTextField labelWithString:@"Flick with two fingers over this window."];
        [label setFont:[NSFont systemFontOfSize:22]];
        [label setFrame:NSMakeRect(40, 300, 820, 120)];
        [[w contentView] addSubview:label];
        [w center];
        [w makeKeyAndOrderFront:nil];
        [NSApp activateIgnoringOtherApps:YES];

        [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskScrollWheel handler:^NSEvent *(NSEvent *e) {
            CGEventRef cge = [e CGEvent];
            double hx = NAN, hy = NAN;
            if (cge && CGEventCopyIOHIDEvent_) {
                IOHIDEventRef h = CGEventCopyIOHIDEvent_(cge);
                if (h) {
                    hx = IOHIDEventGetFloatValue_(h, FIELD(kIOHIDEventTypeScroll, 0));
                    hy = IOHIDEventGetFloatValue_(h, FIELD(kIOHIDEventTypeScroll, 1));
                    CFRelease(h);
                }
            }
            fprintf(scrollFile, "%.6f,%lu,%lu,%.4f,%.4f,%.4f,%.4f,%.4f,%.4f,%.4f,%.4f,%.4f,%.4f\n",
                    e.timestamp, (unsigned long)e.phase, (unsigned long)e.momentumPhase,
                    e.scrollingDeltaX, e.scrollingDeltaY, e.deltaX, e.deltaY, hx, hy,
                    cge ? CGEventGetDoubleValueField(cge, kCGScrollWheelEventFixedPtDeltaAxis2) : NAN,
                    cge ? CGEventGetDoubleValueField(cge, kCGScrollWheelEventFixedPtDeltaAxis1) : NAN,
                    cge ? CGEventGetDoubleValueField(cge, kCGScrollWheelEventPointDeltaAxis2) : NAN,
                    cge ? CGEventGetDoubleValueField(cge, kCGScrollWheelEventPointDeltaAxis1) : NAN);
            nScroll++;
            if (e.momentumPhase == NSEventPhaseEnded || e.phase == NSEventPhaseEnded) {
                fflush(scrollFile);
                fflush(touchFile);
            }
            [label setStringValue:[NSString stringWithFormat:@"%ld scroll events, %ld touch frames.\nFlick slow, medium and fast; let each glide stop by itself.", nScroll, nTouch]];
            return e;
        }];
        [NSApp run];
    }
    return 0;
}
