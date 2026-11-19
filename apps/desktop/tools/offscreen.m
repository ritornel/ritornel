// A display no one sees, for test windows to open on while someone uses the
// Mac: macOS draws a window on a display as long as it is on one, so a
// test's pictures and frames are real, and none of it is on the screen in
// front of the person at the machine. The display is there while this runs
// and goes when it is stopped (SIGTERM or SIGINT).
//
// It is made with CGVirtualDisplay, the private CoreGraphics class that
// DeskPad and BetterDisplay use, at 1600 x 1000 points and twice that in
// pixels at 120 Hz, as a Retina screen is, and placed off the main display's far
// corner, so that the pointer does not wander onto it.
//
// Prints the display's ID and its origin in global points once it is up:
// `offscreen <id> <x> <y>`; what goes wrong, on standard error.
//
// clang -fobjc-arc -framework Foundation -framework CoreGraphics offscreen.m -o offscreen

#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#include <signal.h>
#include <unistd.h>

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(retain, nonatomic) NSString *name;
@property(nonatomic) unsigned int maxPixelsWide;
@property(nonatomic) unsigned int maxPixelsHigh;
@property(nonatomic) CGSize sizeInMillimeters;
@property(nonatomic) unsigned int productID;
@property(nonatomic) unsigned int vendorID;
@property(nonatomic) unsigned int serialNum;
@end

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width
                       height:(unsigned int)height
                  refreshRate:(double)refreshRate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray<CGVirtualDisplayMode *> *modes;
@property(nonatomic) unsigned int hiDPI;
@end

@interface CGVirtualDisplay : NSObject
@property(readonly, nonatomic) unsigned int displayID;
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@end

static const unsigned int WIDE = 1600, HIGH = 1000;

static void stop(int signal) {
  (void)signal;
  exit(0);
}

int main(void) {
  @autoreleasepool {
    CGVirtualDisplayDescriptor *descriptor = [CGVirtualDisplayDescriptor new];
    descriptor.queue = dispatch_get_main_queue();
    descriptor.name = @"Ritornel tests";
    // Room for twice the mode in pixels, or the system offers no mode at
    // twice the density of the largest.
    descriptor.maxPixelsWide = WIDE * 4;
    descriptor.maxPixelsHigh = HIGH * 4;
    descriptor.sizeInMillimeters = CGSizeMake(WIDE * 0.2, HIGH * 0.2);
    descriptor.productID = 0x5254;
    descriptor.vendorID = 0x5254;
    // Each its own: two agents' runs may each want a display at once.
    descriptor.serialNum = (unsigned int)getpid();
    CGVirtualDisplay *display = [[CGVirtualDisplay alloc] initWithDescriptor:descriptor];
    if (display == nil) {
      fprintf(stderr, "offscreen: the display could not be made\n");
      return 1;
    }
    CGVirtualDisplaySettings *settings = [CGVirtualDisplaySettings new];
    settings.hiDPI = 1;
    settings.modes = @[ [[CGVirtualDisplayMode alloc] initWithWidth:WIDE height:HIGH refreshRate:120] ];
    if (![display applySettings:settings]) {
      fprintf(stderr, "offscreen: the display's mode could not be set\n");
      return 1;
    }
    CGDirectDisplayID number = display.displayID;
    // Off the far bottom right corner of all the other displays, touching
    // them only at that point: the pointer reaches it only by aiming for
    // the very corner, and another run's display is never in the way.
    int x = 0, y = 0;
    uint32_t count = 0;
    CGDirectDisplayID others[32];
    if (CGGetActiveDisplayList(32, others, &count) == kCGErrorSuccess) {
      for (uint32_t at = 0; at < count; at++) {
        if (others[at] == number) continue;
        CGRect bounds = CGDisplayBounds(others[at]);
        x = MAX(x, (int)(bounds.origin.x + bounds.size.width));
        y = MAX(y, (int)(bounds.origin.y + bounds.size.height));
      }
    }
    // Its points at twice the pixels, as a Retina screen's are.
    CGDisplayModeRef retina = NULL;
    NSDictionary *all = @{(__bridge NSString *)kCGDisplayShowDuplicateLowResolutionModes : @YES};
    NSArray *modes = CFBridgingRelease(CGDisplayCopyAllDisplayModes(number, (__bridge CFDictionaryRef)all));
    for (id each in modes) {
      CGDisplayModeRef mode = (__bridge CGDisplayModeRef)each;
      if (CGDisplayModeGetWidth(mode) == WIDE && CGDisplayModeGetPixelWidth(mode) == WIDE * 2) {
        retina = mode;
      }
    }
    CGDisplayConfigRef config;
    if (CGBeginDisplayConfiguration(&config) == kCGErrorSuccess) {
      if (retina != NULL) {
        CGConfigureDisplayWithDisplayMode(config, number, retina, NULL);
      }
      CGConfigureDisplayOrigin(config, number, x, y);
      CGCompleteDisplayConfiguration(config, kCGConfigureForSession);
    }
    CGRect at = CGDisplayBounds(number);
    printf("offscreen %u %d %d\n", number, (int)at.origin.x, (int)at.origin.y);
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    dispatch_main();
  }
}
