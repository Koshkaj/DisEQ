//
//  DisEQ.c — a loopback AudioServerPlugIn
//
//  Publishes one virtual device with an output stream apps play into and an
//  input stream DisEQ records from. Between them sits a ring buffer
//  indexed by sample time, which is the only thing the driver really does.
//
//  The point of the device is its volume control. Outputs like DisplayPort
//  audio publish no settable volume at all, so nothing in CoreAudio can give
//  them a slider. This device does publish one, and everything routed through
//  it inherits it.
//
//  Structure follows Apple's NullAudio AudioServerPlugIn sample (the shape of
//  the vtable, the property dispatch, the zero-timestamp arithmetic) and the
//  loopback specifics follow eqMac's driver, Copyright © Bitgapp Ltd, Apache
//  License 2.0 — https://github.com/bitgapp/eqMac, v1.3.2. Changed: ported
//  Swift → C; the mutex in the IO path is replaced with atomics, and gain
//  changes are ramped per sample so a slider drag does not zipper.
//
//  The volume control published here is a control surface rather than a gain
//  stage. The menu bar and the volume keys write to it, the app reads it, and
//  the app applies it on the way to the hardware — at the hardware's own volume
//  control when it has one, so the gain happens as late as possible. Only the
//  loopback input stream, which nothing in this project reads, is attenuated
//  here; the shared ring the app reads carries the mix untouched.
//

#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <math.h>
#include <pthread.h>
#include <fcntl.h>
#include <stdatomic.h>
#include <stddef.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#pragma mark - Constants

enum {
    kObjectID_PlugIn = kAudioObjectPlugInObject,
    kObjectID_Device = 2,
    kObjectID_Stream_Input = 3,
    kObjectID_Stream_Output = 4,
    kObjectID_Volume_Output_Main = 5,
    kObjectID_Mute_Output_Main = 6,
};

#define kDeviceUID          "DisEQDevice"
#define kDeviceModelUID     "DisEQModelUID"
#define kManufacturer       "koshka"
#define kDeviceDefaultName  "DisEQ"

/// Only this bundle may rename the device. Anyone can read the name.
#define kAppBundleID        "com.koshka.DisEQ"

/// Selector for the settable-name custom property. The app renames the device
/// after whatever hardware it is proxying, so users pick an output that reads
/// like their own speakers.
#define kCustomProperty_Name 'kdnm'

/// Selector for the plug-in's visibility, on the plug-in object rather than the
/// device. The app publishes the device when it starts and takes it away when
/// it quits, so a machine with DisEQ installed but not running looks
/// exactly like a machine without it — no orphan entry in Sound settings, and
/// nothing for the system to fall back to when the app is gone.
///
/// It has to live on the plug-in: a hidden device has no object to address, so
/// the property that brings it back cannot live on the device itself.
#define kCustomProperty_Shown 'kdsh'

#define kChannelCount   2
#define kBitsPerChannel 32
#define kBytesPerChannel (kBitsPerChannel / 8)
#define kBytesPerFrame  (kChannelCount * kBytesPerChannel)

/// Also the zero-timestamp period: the HAL is told the device wraps every this
/// many frames, and the ring is exactly that long.
#define kRingBufferFrames 16384

static const Float64 kSupportedSampleRates[] = {
    44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0,
};
#define kSupportedSampleRateCount (sizeof(kSupportedSampleRates) / sizeof(Float64))
#define kDefaultSampleRate 48000.0

#define kVolumeMinDB (-96.0f)
#define kVolumeMaxDB (0.0f)

/// Fraction of the remaining distance the applied gain closes each frame. At
/// 48 kHz this settles a full-scale slider move in about 5 ms — fast enough to
/// feel immediate, slow enough that no step is audible as a click.
#define kGainSmoothing 0.0005f

#pragma mark - State

static pthread_mutex_t gStateMutex = PTHREAD_MUTEX_INITIALIZER;
static AudioServerPlugInHostRef gHost = NULL;
static UInt32 gRefCount = 0;

static Float64 gSampleRate = kDefaultSampleRate;
static Float64 gHostTicksPerFrame = 0.0;
static CFStringRef gDeviceName = NULL;

/// Guarded by gStateMutex for writes; read without it on the IO thread, where
/// a frame-late value is harmless.
/// Whether the device is published. False until the app says otherwise, which
/// is what makes the device appear on launch and vanish on quit.
static _Atomic bool gShown = false;

static _Atomic UInt64 gIOCount = 0;
static _Atomic bool gMuted = false;
/// Volume as the HAL sees it, 0..1.
static _Atomic float gVolumeScalar = 1.0f;
/// Volume as the samples see it, chasing gVolumeScalar cubed.
static float gAppliedGain = 1.0f;

static UInt64 gAnchorHostTime = 0;
static _Atomic UInt64 gTimestampCount = 0;

/// Interleaved stereo, indexed by sample time modulo its length. Static, so
/// StartIO allocates nothing.
static Float32 gRingBuffer[kRingBufferFrames * kChannelCount];

#pragma mark - Shared ring

/// The audio handed to the app, without any capture API in between.
///
/// coreaudiod gives this plug-in every frame the machine plays, because the
/// plug-in *is* the output device — no permission is involved in that. The only
/// reason DisEQ ever needed one was to get those frames back out of
/// coreaudiod and into the app that applies the EQ. Reading a device's input
/// stream (eqMac's approach) is microphone access; a process tap is audio
/// capture. POSIX shared memory is neither, and coreaudiod's sandbox profile
/// allows it outright:
///
///     /System/Library/Sandbox/Profiles/com.apple.audio.coreaudiod.sb
///     (allow ipc-posix-shm)
///
/// One writer — the IO thread here — and one reader, in the app. The reader
/// only ever reads, so nothing needs locking: it follows `written`, which is
/// published after the frames it describes.
#define kSharedName    "/DisEQ.audio"
#define kSharedMagic   0x4B44534E /* 'KDSN' */
#define kSharedVersion 1u

/// Laid out for a reader in another process and another language. Fixed-width
/// types, explicit padding, atomics on everything that changes.
struct kd_SharedHeader {
    _Atomic uint32_t magic;
    _Atomic uint32_t version;
    _Atomic uint32_t channels;
    _Atomic uint32_t capacityFrames;
    _Atomic uint64_t sampleRateBits;
    /// Sample time one past the last frame written. The reader's clock.
    _Atomic int64_t written;
    /// Whether the device is running IO. Cleared on stop so a reader goes
    /// silent rather than replaying whatever the ring still holds.
    _Atomic uint32_t running;
    _Atomic uint32_t reserved;
};

static struct kd_SharedHeader* gShared = NULL;
static Float32* gSharedSamples = NULL;

static void shared_publish_rate(Float64 rate) {
    if (gShared == NULL) {
        return;
    }
    uint64_t bits;
    memcpy(&bits, &rate, sizeof(bits));
    atomic_store(&gShared->sampleRateBits, bits);
}

/// Creates and maps the shared ring. Failure is not fatal: the device still
/// works, and the app falls back to saying so.
static void shared_open(void) {
    if (gShared != NULL) {
        return;
    }
    size_t bytes = sizeof(struct kd_SharedHeader)
                   + (size_t)kRingBufferFrames * kChannelCount * sizeof(Float32);

    int fd = shm_open(kSharedName, O_CREAT | O_RDWR, 0644);
    if (fd < 0) {
        return;
    }
    // The app runs as the user and coreaudiod does not, so the mode has to
    // survive whatever umask coreaudiod was started with. Read-only for
    // everyone else is all the reader needs.
    fchmod(fd, 0644);
    if (ftruncate(fd, (off_t)bytes) != 0) {
        // Already sized by an earlier load, which is fine; a genuine failure
        // shows up as a failed mmap below.
    }

    void* map = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (map == MAP_FAILED) {
        return;
    }

    gShared = (struct kd_SharedHeader*)map;
    gSharedSamples = (Float32*)((char*)map + sizeof(struct kd_SharedHeader));
    memset(gSharedSamples, 0, (size_t)kRingBufferFrames * kChannelCount * sizeof(Float32));

    atomic_store(&gShared->channels, kChannelCount);
    atomic_store(&gShared->capacityFrames, kRingBufferFrames);
    atomic_store(&gShared->written, 0);
    atomic_store(&gShared->running, 0);
    atomic_store(&gShared->reserved, 0);
    shared_publish_rate(gSampleRate);
    atomic_store(&gShared->version, kSharedVersion);
    // Published last: a reader that sees the magic sees a header that is
    // already filled in.
    atomic_store(&gShared->magic, kSharedMagic);
}

#pragma mark - Volume curve

/// Cubic taper. A linear-in-dB slider spends most of its travel inaudible;
/// this keeps the useful range spread across the whole thing.
static float scalar_to_gain(float scalar) {
    if (scalar <= 0.0f) {
        return 0.0f;
    }
    if (scalar >= 1.0f) {
        return 1.0f;
    }
    return scalar * scalar * scalar;
}

static Float32 scalar_to_db(Float32 scalar) {
    if (scalar <= 0.0f) {
        return kVolumeMinDB;
    }
    Float32 db = 60.0f * log10f(scalar);
    return db < kVolumeMinDB ? kVolumeMinDB : (db > kVolumeMaxDB ? kVolumeMaxDB : db);
}

static Float32 db_to_scalar(Float32 db) {
    if (db <= kVolumeMinDB) {
        return 0.0f;
    }
    if (db >= kVolumeMaxDB) {
        return 1.0f;
    }
    return powf(10.0f, db / 60.0f);
}

#pragma mark - Helpers

static void fill_stream_description(AudioStreamBasicDescription* description, Float64 sampleRate) {
    memset(description, 0, sizeof(AudioStreamBasicDescription));
    description->mSampleRate = sampleRate;
    description->mFormatID = kAudioFormatLinearPCM;
    description->mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagsNativeEndian
                                | kAudioFormatFlagIsPacked;
    description->mBytesPerPacket = kBytesPerFrame;
    description->mFramesPerPacket = 1;
    description->mBytesPerFrame = kBytesPerFrame;
    description->mChannelsPerFrame = kChannelCount;
    description->mBitsPerChannel = kBitsPerChannel;
}

static bool is_supported_sample_rate(Float64 rate) {
    for (size_t index = 0; index < kSupportedSampleRateCount; index++) {
        if (kSupportedSampleRates[index] == rate) {
            return true;
        }
    }
    return false;
}

static void recalculate_host_ticks_per_frame(void) {
    struct mach_timebase_info timebase;
    mach_timebase_info(&timebase);
    Float64 clockFrequency = ((Float64)timebase.denom / (Float64)timebase.numer) * 1000000000.0;
    gHostTicksPerFrame = clockFrequency / gSampleRate;
}

#pragma mark - Clients

/// Property calls identify their caller by pid alone, but the bundle ID is what
/// says whether the caller is us. The HAL hands us both when a client attaches,
/// so keep the pairing until it detaches.
///
/// Fixed size: this exists to answer one question about one bundle, and a table
/// that cannot grow cannot leak. Overflow simply means a late client is not
/// recognised, which costs it the ability to rename the device.
#define kMaxClients 64

struct ClientRecord {
    pid_t processID;
    Boolean isOurApp;
    Boolean inUse;
};

static struct ClientRecord gClients[kMaxClients];

static void remember_client(const AudioServerPlugInClientInfo* client) {
    if (client == NULL) {
        return;
    }
    Boolean isOurApp = client->mBundleID != NULL
                       && CFStringCompare(client->mBundleID, CFSTR(kAppBundleID), 0) == kCFCompareEqualTo;

    pthread_mutex_lock(&gStateMutex);
    for (int index = 0; index < kMaxClients; index++) {
        if (!gClients[index].inUse) {
            gClients[index].processID = client->mProcessID;
            gClients[index].isOurApp = isOurApp;
            gClients[index].inUse = true;
            break;
        }
    }
    pthread_mutex_unlock(&gStateMutex);
}

static void forget_client(const AudioServerPlugInClientInfo* client) {
    if (client == NULL) {
        return;
    }
    pthread_mutex_lock(&gStateMutex);
    for (int index = 0; index < kMaxClients; index++) {
        if (gClients[index].inUse && gClients[index].processID == client->mProcessID) {
            gClients[index].inUse = false;
            break;
        }
    }
    pthread_mutex_unlock(&gStateMutex);
}

static Boolean client_is_our_app(pid_t processID) {
    Boolean found = false;
    pthread_mutex_lock(&gStateMutex);
    for (int index = 0; index < kMaxClients; index++) {
        if (gClients[index].inUse && gClients[index].processID == processID) {
            found = gClients[index].isOurApp;
            break;
        }
    }
    pthread_mutex_unlock(&gStateMutex);
    return found;
}

#pragma mark - Forward declarations

static HRESULT kd_QueryInterface(void* inDriver, REFIID inUUID, LPVOID* outInterface);
static ULONG kd_AddRef(void* inDriver);
static ULONG kd_Release(void* inDriver);
static OSStatus kd_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost);
static OSStatus kd_CreateDevice(AudioServerPlugInDriverRef inDriver,
                                CFDictionaryRef inDescription,
                                const AudioServerPlugInClientInfo* inClientInfo,
                                AudioObjectID* outDeviceObjectID);
static OSStatus kd_DestroyDevice(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID);
static OSStatus kd_AddDeviceClient(AudioServerPlugInDriverRef inDriver,
                                   AudioObjectID inDeviceObjectID,
                                   const AudioServerPlugInClientInfo* inClientInfo);
static OSStatus kd_RemoveDeviceClient(AudioServerPlugInDriverRef inDriver,
                                      AudioObjectID inDeviceObjectID,
                                      const AudioServerPlugInClientInfo* inClientInfo);
static OSStatus kd_PerformDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver,
                                                    AudioObjectID inDeviceObjectID,
                                                    UInt64 inChangeAction,
                                                    void* inChangeInfo);
static OSStatus kd_AbortDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver,
                                                  AudioObjectID inDeviceObjectID,
                                                  UInt64 inChangeAction,
                                                  void* inChangeInfo);
static Boolean kd_HasProperty(AudioServerPlugInDriverRef inDriver,
                              AudioObjectID inObjectID,
                              pid_t inClientProcessID,
                              const AudioObjectPropertyAddress* inAddress);
static OSStatus kd_IsPropertySettable(AudioServerPlugInDriverRef inDriver,
                                      AudioObjectID inObjectID,
                                      pid_t inClientProcessID,
                                      const AudioObjectPropertyAddress* inAddress,
                                      Boolean* outIsSettable);
static OSStatus kd_GetPropertyDataSize(AudioServerPlugInDriverRef inDriver,
                                       AudioObjectID inObjectID,
                                       pid_t inClientProcessID,
                                       const AudioObjectPropertyAddress* inAddress,
                                       UInt32 inQualifierDataSize,
                                       const void* inQualifierData,
                                       UInt32* outDataSize);
static OSStatus kd_GetPropertyData(AudioServerPlugInDriverRef inDriver,
                                   AudioObjectID inObjectID,
                                   pid_t inClientProcessID,
                                   const AudioObjectPropertyAddress* inAddress,
                                   UInt32 inQualifierDataSize,
                                   const void* inQualifierData,
                                   UInt32 inDataSize,
                                   UInt32* outDataSize,
                                   void* outData);
static OSStatus kd_SetPropertyData(AudioServerPlugInDriverRef inDriver,
                                   AudioObjectID inObjectID,
                                   pid_t inClientProcessID,
                                   const AudioObjectPropertyAddress* inAddress,
                                   UInt32 inQualifierDataSize,
                                   const void* inQualifierData,
                                   UInt32 inDataSize,
                                   const void* inData);
static OSStatus kd_StartIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID);
static OSStatus kd_StopIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID);
static OSStatus kd_GetZeroTimeStamp(AudioServerPlugInDriverRef inDriver,
                                    AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID,
                                    Float64* outSampleTime,
                                    UInt64* outHostTime,
                                    UInt64* outSeed);
static OSStatus kd_WillDoIOOperation(AudioServerPlugInDriverRef inDriver,
                                     AudioObjectID inDeviceObjectID,
                                     UInt32 inClientID,
                                     UInt32 inOperationID,
                                     Boolean* outWillDo,
                                     Boolean* outWillDoInPlace);
static OSStatus kd_BeginIOOperation(AudioServerPlugInDriverRef inDriver,
                                    AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID,
                                    UInt32 inOperationID,
                                    UInt32 inIOBufferFrameSize,
                                    const AudioServerPlugInIOCycleInfo* inIOCycleInfo);
static OSStatus kd_DoIOOperation(AudioServerPlugInDriverRef inDriver,
                                 AudioObjectID inDeviceObjectID,
                                 AudioObjectID inStreamObjectID,
                                 UInt32 inClientID,
                                 UInt32 inOperationID,
                                 UInt32 inIOBufferFrameSize,
                                 const AudioServerPlugInIOCycleInfo* inIOCycleInfo,
                                 void* ioMainBuffer,
                                 void* ioSecondaryBuffer);
static OSStatus kd_EndIOOperation(AudioServerPlugInDriverRef inDriver,
                                  AudioObjectID inDeviceObjectID,
                                  UInt32 inClientID,
                                  UInt32 inOperationID,
                                  UInt32 inIOBufferFrameSize,
                                  const AudioServerPlugInIOCycleInfo* inIOCycleInfo);

#pragma mark - The interface

static AudioServerPlugInDriverInterface gInterface = {
    NULL,
    kd_QueryInterface,
    kd_AddRef,
    kd_Release,
    kd_Initialize,
    kd_CreateDevice,
    kd_DestroyDevice,
    kd_AddDeviceClient,
    kd_RemoveDeviceClient,
    kd_PerformDeviceConfigurationChange,
    kd_AbortDeviceConfigurationChange,
    kd_HasProperty,
    kd_IsPropertySettable,
    kd_GetPropertyDataSize,
    kd_GetPropertyData,
    kd_SetPropertyData,
    kd_StartIO,
    kd_StopIO,
    kd_GetZeroTimeStamp,
    kd_WillDoIOOperation,
    kd_BeginIOOperation,
    kd_DoIOOperation,
    kd_EndIOOperation,
};

static AudioServerPlugInDriverInterface* gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriverRef = &gInterfacePtr;

/// The CFPlugIn factory named in Info.plist. Everything the driver needs is
/// statically initialised, so this only has to hand back the vtable.
///
/// The visibility attribute is load-bearing: the bundle is built with hidden
/// visibility, and coreaudiod finds this by name.
__attribute__((visibility("default"))) void* DisEQ_Create(CFAllocatorRef inAllocator,
                                                                CFUUIDRef inRequestedTypeUUID);
void* DisEQ_Create(CFAllocatorRef inAllocator, CFUUIDRef inRequestedTypeUUID) {
    (void)inAllocator;
    if (!CFEqual(inRequestedTypeUUID, kAudioServerPlugInTypeUUID)) {
        return NULL;
    }
    return gDriverRef;
}

#pragma mark - IUnknown

static HRESULT kd_QueryInterface(void* inDriver, REFIID inUUID, LPVOID* outInterface) {
    if (inDriver != gDriverRef || outInterface == NULL) {
        return kAudioHardwareBadObjectError;
    }

    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(NULL, inUUID);
    if (requested == NULL) {
        return kAudioHardwareIllegalOperationError;
    }

    HRESULT result = 0x80000004; // E_NOINTERFACE
    if (CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID)) {
        pthread_mutex_lock(&gStateMutex);
        gRefCount++;
        pthread_mutex_unlock(&gStateMutex);
        *outInterface = gDriverRef;
        result = 0;
    }

    CFRelease(requested);
    return result;
}

static ULONG kd_AddRef(void* inDriver) {
    if (inDriver != gDriverRef) {
        return 0;
    }
    pthread_mutex_lock(&gStateMutex);
    if (gRefCount < UINT32_MAX) {
        gRefCount++;
    }
    ULONG count = gRefCount;
    pthread_mutex_unlock(&gStateMutex);
    return count;
}

static ULONG kd_Release(void* inDriver) {
    if (inDriver != gDriverRef) {
        return 0;
    }
    pthread_mutex_lock(&gStateMutex);
    if (gRefCount > 0) {
        gRefCount--;
    }
    ULONG count = gRefCount;
    pthread_mutex_unlock(&gStateMutex);
    return count;
}

#pragma mark - Lifecycle

static OSStatus kd_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost) {
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    shared_open();
    pthread_mutex_lock(&gStateMutex);
    gHost = inHost;
    if (gDeviceName == NULL) {
        gDeviceName = CFSTR(kDeviceDefaultName);
        CFRetain(gDeviceName);
    }
    recalculate_host_ticks_per_frame();
    pthread_mutex_unlock(&gStateMutex);
    return 0;
}

/// The device is published statically, so there is nothing to create.
static OSStatus kd_CreateDevice(AudioServerPlugInDriverRef inDriver,
                                CFDictionaryRef inDescription,
                                const AudioServerPlugInClientInfo* inClientInfo,
                                AudioObjectID* outDeviceObjectID) {
    (void)inDriver;
    (void)inDescription;
    (void)inClientInfo;
    (void)outDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus kd_DestroyDevice(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID) {
    (void)inDriver;
    (void)inDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus kd_AddDeviceClient(AudioServerPlugInDriverRef inDriver,
                                   AudioObjectID inDeviceObjectID,
                                   const AudioServerPlugInClientInfo* inClientInfo) {
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }
    remember_client(inClientInfo);
    return 0;
}

static OSStatus kd_RemoveDeviceClient(AudioServerPlugInDriverRef inDriver,
                                      AudioObjectID inDeviceObjectID,
                                      const AudioServerPlugInClientInfo* inClientInfo) {
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }
    forget_client(inClientInfo);
    return 0;
}

/// The HAL has stopped IO by the time this runs, so the sample rate can just be
/// swapped. `inChangeAction` carries the new rate, as SetPropertyData asked.
static OSStatus kd_PerformDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver,
                                                    AudioObjectID inDeviceObjectID,
                                                    UInt64 inChangeAction,
                                                    void* inChangeInfo) {
    (void)inChangeInfo;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }

    Float64 requested = (Float64)inChangeAction;
    if (!is_supported_sample_rate(requested)) {
        return kAudioHardwareIllegalOperationError;
    }

    pthread_mutex_lock(&gStateMutex);
    gSampleRate = requested;
    shared_publish_rate(requested);
    recalculate_host_ticks_per_frame();
    // The ring's contents belong to the old rate.
    memset(gRingBuffer, 0, sizeof(gRingBuffer));
    gAnchorHostTime = mach_absolute_time();
    atomic_store(&gTimestampCount, 0);
    pthread_mutex_unlock(&gStateMutex);

    return 0;
}

static OSStatus kd_AbortDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver,
                                                  AudioObjectID inDeviceObjectID,
                                                  UInt64 inChangeAction,
                                                  void* inChangeInfo) {
    (void)inChangeAction;
    (void)inChangeInfo;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    return inDeviceObjectID == kObjectID_Device ? 0 : kAudioHardwareBadObjectError;
}

#pragma mark - Properties: plug-in

static Boolean plugin_has_property(const AudioObjectPropertyAddress* address) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioObjectPropertyManufacturer:
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList:
        case kAudioPlugInPropertyTranslateUIDToDevice:
        case kAudioPlugInPropertyResourceBundle:
        case kAudioObjectPropertyCustomPropertyInfoList:
        case kCustomProperty_Shown:
            return true;
        default:
            return false;
    }
}

static OSStatus plugin_get_property_data_size(const AudioObjectPropertyAddress* address, UInt32* outSize) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
            *outSize = sizeof(AudioClassID);
            return 0;
        case kAudioObjectPropertyOwner:
        case kAudioPlugInPropertyTranslateUIDToDevice:
            *outSize = sizeof(AudioObjectID);
            return 0;
        case kAudioObjectPropertyManufacturer:
        case kAudioPlugInPropertyResourceBundle:
            *outSize = sizeof(CFStringRef);
            return 0;
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList:
            // Nothing at all while hidden: an empty device list is how the
            // device stops existing as far as the rest of the system is
            // concerned.
            *outSize = atomic_load(&gShown) ? sizeof(AudioObjectID) : 0;
            return 0;
        case kAudioObjectPropertyCustomPropertyInfoList:
            *outSize = sizeof(AudioServerPlugInCustomPropertyInfo);
            return 0;
        case kCustomProperty_Shown:
            *outSize = sizeof(CFPropertyListRef);
            return 0;
        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus plugin_get_property_data(const AudioObjectPropertyAddress* address,
                                         UInt32 inQualifierDataSize,
                                         const void* inQualifierData,
                                         UInt32 inDataSize,
                                         UInt32* outDataSize,
                                         void* outData) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = kAudioObjectClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = kAudioPlugInClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyOwner:
            if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
            *((AudioObjectID*)outData) = kAudioObjectUnknown;
            *outDataSize = sizeof(AudioObjectID);
            return 0;

        case kAudioObjectPropertyManufacturer:
            if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
            *((CFStringRef*)outData) = CFSTR(kManufacturer);
            CFRetain(*((CFStringRef*)outData));
            *outDataSize = sizeof(CFStringRef);
            return 0;

        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList:
            if (!atomic_load(&gShown) || inDataSize < sizeof(AudioObjectID)) {
                *outDataSize = 0;
                return 0;
            }
            *((AudioObjectID*)outData) = kObjectID_Device;
            *outDataSize = sizeof(AudioObjectID);
            return 0;

        case kAudioObjectPropertyCustomPropertyInfoList: {
            if (inDataSize < sizeof(AudioServerPlugInCustomPropertyInfo)) {
                *outDataSize = 0;
                return 0;
            }
            AudioServerPlugInCustomPropertyInfo* info =
                (AudioServerPlugInCustomPropertyInfo*)outData;
            info[0].mSelector = kCustomProperty_Shown;
            info[0].mPropertyDataType = kAudioServerPlugInCustomPropertyDataTypeCFPropertyList;
            info[0].mQualifierDataType = kAudioServerPlugInCustomPropertyDataTypeNone;
            *outDataSize = sizeof(AudioServerPlugInCustomPropertyInfo);
            return 0;
        }

        case kCustomProperty_Shown: {
            if (inDataSize < sizeof(CFPropertyListRef)) return kAudioHardwareBadPropertySizeError;
            CFBooleanRef shown = atomic_load(&gShown) ? kCFBooleanTrue : kCFBooleanFalse;
            CFRetain(shown);
            *((CFPropertyListRef*)outData) = shown;
            *outDataSize = sizeof(CFPropertyListRef);
            return 0;
        }

        case kAudioPlugInPropertyTranslateUIDToDevice: {
            if (inQualifierDataSize != sizeof(CFStringRef) || inQualifierData == NULL) {
                return kAudioHardwareBadPropertySizeError;
            }
            if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
            CFStringRef uid = *((const CFStringRef*)inQualifierData);
            Boolean matches = atomic_load(&gShown)
                              && CFStringCompare(uid, CFSTR(kDeviceUID), 0) == kCFCompareEqualTo;
            *((AudioObjectID*)outData) = matches ? kObjectID_Device : kAudioObjectUnknown;
            *outDataSize = sizeof(AudioObjectID);
            return 0;
        }

        case kAudioPlugInPropertyResourceBundle:
            if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
            *((CFStringRef*)outData) = CFSTR("");
            CFRetain(*((CFStringRef*)outData));
            *outDataSize = sizeof(CFStringRef);
            return 0;

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

/// The only plug-in property anyone may write: whether the device exists.
///
/// Ungated, unlike the device's name. A hidden device has no client list to
/// check a caller against, so there is nobody to recognise — and the worst an
/// unexpected caller can do is publish or retract a device that belongs to an
/// application which is not running.
static OSStatus plugin_set_property_data(const AudioObjectPropertyAddress* address,
                                         UInt32 inDataSize,
                                         const void* inData,
                                         UInt32* outChangedCount,
                                         AudioObjectPropertyAddress changed[2]) {
    *outChangedCount = 0;

    switch (address->mSelector) {
        case kCustomProperty_Shown: {
            if (inDataSize != sizeof(CFPropertyListRef)) return kAudioHardwareBadPropertySizeError;
            CFPropertyListRef value = *((const CFPropertyListRef*)inData);
            if (value == NULL) return kAudioHardwareIllegalOperationError;

            bool wanted;
            if (CFGetTypeID(value) == CFBooleanGetTypeID()) {
                wanted = CFBooleanGetValue((CFBooleanRef)value);
            } else if (CFGetTypeID(value) == CFNumberGetTypeID()) {
                int number = 0;
                CFNumberGetValue((CFNumberRef)value, kCFNumberIntType, &number);
                wanted = number != 0;
            } else {
                return kAudioHardwareIllegalOperationError;
            }

            if (atomic_exchange(&gShown, wanted) == wanted) {
                return 0;
            }

            // Both are how the HAL discovers devices; changing one without the
            // other leaves the system with a device it can still address and
            // no way to enumerate, or the reverse.
            changed[0].mSelector = kAudioPlugInPropertyDeviceList;
            changed[0].mScope = kAudioObjectPropertyScopeGlobal;
            changed[0].mElement = kAudioObjectPropertyElementMain;
            changed[1].mSelector = kAudioObjectPropertyOwnedObjects;
            changed[1].mScope = kAudioObjectPropertyScopeGlobal;
            changed[1].mElement = kAudioObjectPropertyElementMain;
            *outChangedCount = 2;
            return 0;
        }

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

#pragma mark - Properties: device

static Boolean device_has_property(const AudioObjectPropertyAddress* address) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioObjectPropertyName:
        case kAudioObjectPropertyManufacturer:
        case kAudioObjectPropertyOwnedObjects:
        case kAudioObjectPropertyControlList:
        case kAudioObjectPropertyCustomPropertyInfoList:
        case kAudioDevicePropertyDeviceUID:
        case kAudioDevicePropertyModelUID:
        case kAudioDevicePropertyTransportType:
        case kAudioDevicePropertyRelatedDevices:
        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceIsRunning:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertyStreams:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyNominalSampleRate:
        case kAudioDevicePropertyAvailableNominalSampleRates:
        case kAudioDevicePropertyIsHidden:
        case kAudioDevicePropertyZeroTimeStampPeriod:
        case kAudioDevicePropertyIcon:
        case kCustomProperty_Name:
            return true;
        case kAudioDevicePropertyPreferredChannelsForStereo:
        case kAudioDevicePropertyPreferredChannelLayout:
            return address->mScope == kAudioObjectPropertyScopeInput
                   || address->mScope == kAudioObjectPropertyScopeOutput;
        default:
            return false;
    }
}

/// How many objects the device owns in this scope. Global sees everything;
/// input sees its stream; output sees its stream and both controls.
static UInt32 device_owned_objects(AudioObjectPropertyScope scope, AudioObjectID* out, UInt32 capacity) {
    AudioObjectID all[4];
    UInt32 count = 0;

    if (scope == kAudioObjectPropertyScopeGlobal || scope == kAudioObjectPropertyScopeInput) {
        all[count++] = kObjectID_Stream_Input;
    }
    if (scope == kAudioObjectPropertyScopeGlobal || scope == kAudioObjectPropertyScopeOutput) {
        all[count++] = kObjectID_Stream_Output;
        all[count++] = kObjectID_Volume_Output_Main;
        all[count++] = kObjectID_Mute_Output_Main;
    }

    UInt32 written = count < capacity ? count : capacity;
    for (UInt32 index = 0; index < written; index++) {
        out[index] = all[index];
    }
    return count;
}

static OSStatus device_get_property_data_size(const AudioObjectPropertyAddress* address, UInt32* outSize) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
            *outSize = sizeof(AudioClassID);
            return 0;
        case kAudioObjectPropertyOwner:
            *outSize = sizeof(AudioObjectID);
            return 0;
        case kAudioObjectPropertyName:
        case kAudioObjectPropertyManufacturer:
        case kAudioDevicePropertyDeviceUID:
        case kAudioDevicePropertyModelUID:
        case kCustomProperty_Name:
            *outSize = sizeof(CFStringRef);
            return 0;
        case kAudioObjectPropertyOwnedObjects:
            *outSize = device_owned_objects(address->mScope, NULL, 0) * sizeof(AudioObjectID);
            return 0;
        case kAudioObjectPropertyControlList:
            *outSize = 2 * sizeof(AudioObjectID);
            return 0;
        case kAudioObjectPropertyCustomPropertyInfoList:
            *outSize = sizeof(AudioServerPlugInCustomPropertyInfo);
            return 0;
        case kAudioDevicePropertyTransportType:
        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceIsRunning:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyIsHidden:
        case kAudioDevicePropertyZeroTimeStampPeriod:
            *outSize = sizeof(UInt32);
            return 0;
        case kAudioDevicePropertyRelatedDevices:
            *outSize = sizeof(AudioObjectID);
            return 0;
        case kAudioDevicePropertyStreams:
            if (address->mScope == kAudioObjectPropertyScopeGlobal) {
                *outSize = 2 * sizeof(AudioObjectID);
            } else if (address->mScope == kAudioObjectPropertyScopeInput
                       || address->mScope == kAudioObjectPropertyScopeOutput) {
                *outSize = sizeof(AudioObjectID);
            } else {
                *outSize = 0;
            }
            return 0;
        case kAudioDevicePropertyNominalSampleRate:
            *outSize = sizeof(Float64);
            return 0;
        case kAudioDevicePropertyAvailableNominalSampleRates:
            *outSize = (UInt32)(kSupportedSampleRateCount * sizeof(AudioValueRange));
            return 0;
        case kAudioDevicePropertyPreferredChannelsForStereo:
            *outSize = 2 * sizeof(UInt32);
            return 0;
        case kAudioDevicePropertyPreferredChannelLayout:
            *outSize = (UInt32)(offsetof(AudioChannelLayout, mChannelDescriptions)
                                + (kChannelCount * sizeof(AudioChannelDescription)));
            return 0;
        case kAudioDevicePropertyIcon:
            *outSize = sizeof(CFURLRef);
            return 0;
        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus device_get_property_data(const AudioObjectPropertyAddress* address,
                                         UInt32 inDataSize,
                                         UInt32* outDataSize,
                                         void* outData) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = kAudioObjectClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = kAudioDeviceClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyOwner:
            if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
            *((AudioObjectID*)outData) = kObjectID_PlugIn;
            *outDataSize = sizeof(AudioObjectID);
            return 0;

        case kAudioObjectPropertyName:
        case kCustomProperty_Name: {
            if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
            pthread_mutex_lock(&gStateMutex);
            CFStringRef name = gDeviceName != NULL ? gDeviceName : CFSTR(kDeviceDefaultName);
            CFRetain(name);
            pthread_mutex_unlock(&gStateMutex);
            *((CFStringRef*)outData) = name;
            *outDataSize = sizeof(CFStringRef);
            return 0;
        }

        case kAudioObjectPropertyManufacturer:
            if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
            *((CFStringRef*)outData) = CFSTR(kManufacturer);
            CFRetain(*((CFStringRef*)outData));
            *outDataSize = sizeof(CFStringRef);
            return 0;

        case kAudioObjectPropertyOwnedObjects: {
            UInt32 capacity = inDataSize / sizeof(AudioObjectID);
            UInt32 total = device_owned_objects(address->mScope, (AudioObjectID*)outData, capacity);
            UInt32 written = total < capacity ? total : capacity;
            *outDataSize = written * sizeof(AudioObjectID);
            return 0;
        }

        case kAudioObjectPropertyControlList: {
            AudioObjectID controls[2] = { kObjectID_Volume_Output_Main, kObjectID_Mute_Output_Main };
            UInt32 capacity = inDataSize / sizeof(AudioObjectID);
            UInt32 written = capacity < 2 ? capacity : 2;
            memcpy(outData, controls, written * sizeof(AudioObjectID));
            *outDataSize = written * sizeof(AudioObjectID);
            return 0;
        }

        case kAudioObjectPropertyCustomPropertyInfoList: {
            if (inDataSize < sizeof(AudioServerPlugInCustomPropertyInfo)) {
                *outDataSize = 0;
                return 0;
            }
            AudioServerPlugInCustomPropertyInfo* info = (AudioServerPlugInCustomPropertyInfo*)outData;
            info[0].mSelector = kCustomProperty_Name;
            info[0].mPropertyDataType = kAudioServerPlugInCustomPropertyDataTypeCFString;
            info[0].mQualifierDataType = kAudioServerPlugInCustomPropertyDataTypeNone;
            *outDataSize = sizeof(AudioServerPlugInCustomPropertyInfo);
            return 0;
        }

        case kAudioDevicePropertyDeviceUID:
            if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
            *((CFStringRef*)outData) = CFSTR(kDeviceUID);
            CFRetain(*((CFStringRef*)outData));
            *outDataSize = sizeof(CFStringRef);
            return 0;

        case kAudioDevicePropertyModelUID:
            if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
            *((CFStringRef*)outData) = CFSTR(kDeviceModelUID);
            CFRetain(*((CFStringRef*)outData));
            *outDataSize = sizeof(CFStringRef);
            return 0;

        case kAudioDevicePropertyTransportType:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = kAudioDeviceTransportTypeVirtual;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioDevicePropertyRelatedDevices:
            if (inDataSize < sizeof(AudioObjectID)) {
                *outDataSize = 0;
                return 0;
            }
            *((AudioObjectID*)outData) = kObjectID_Device;
            *outDataSize = sizeof(AudioObjectID);
            return 0;

        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyIsHidden:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = 0;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = 1;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioDevicePropertyDeviceIsRunning:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = atomic_load(&gIOCount) > 0 ? 1 : 0;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioDevicePropertyStreams: {
            AudioObjectID streams[2];
            UInt32 total = 0;
            if (address->mScope == kAudioObjectPropertyScopeGlobal
                || address->mScope == kAudioObjectPropertyScopeInput) {
                streams[total++] = kObjectID_Stream_Input;
            }
            if (address->mScope == kAudioObjectPropertyScopeGlobal
                || address->mScope == kAudioObjectPropertyScopeOutput) {
                streams[total++] = kObjectID_Stream_Output;
            }
            UInt32 capacity = inDataSize / sizeof(AudioObjectID);
            UInt32 written = total < capacity ? total : capacity;
            memcpy(outData, streams, written * sizeof(AudioObjectID));
            *outDataSize = written * sizeof(AudioObjectID);
            return 0;
        }

        case kAudioDevicePropertyNominalSampleRate:
            if (inDataSize < sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
            pthread_mutex_lock(&gStateMutex);
            *((Float64*)outData) = gSampleRate;
            pthread_mutex_unlock(&gStateMutex);
            *outDataSize = sizeof(Float64);
            return 0;

        case kAudioDevicePropertyAvailableNominalSampleRates: {
            UInt32 capacity = inDataSize / sizeof(AudioValueRange);
            UInt32 written = capacity < kSupportedSampleRateCount ? capacity
                                                                  : (UInt32)kSupportedSampleRateCount;
            AudioValueRange* ranges = (AudioValueRange*)outData;
            for (UInt32 index = 0; index < written; index++) {
                ranges[index].mMinimum = kSupportedSampleRates[index];
                ranges[index].mMaximum = kSupportedSampleRates[index];
            }
            *outDataSize = written * sizeof(AudioValueRange);
            return 0;
        }

        case kAudioDevicePropertyPreferredChannelsForStereo: {
            if (inDataSize < 2 * sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            UInt32* channels = (UInt32*)outData;
            channels[0] = 1;
            channels[1] = 2;
            *outDataSize = 2 * sizeof(UInt32);
            return 0;
        }

        case kAudioDevicePropertyPreferredChannelLayout: {
            UInt32 needed = (UInt32)(offsetof(AudioChannelLayout, mChannelDescriptions)
                                     + (kChannelCount * sizeof(AudioChannelDescription)));
            if (inDataSize < needed) return kAudioHardwareBadPropertySizeError;
            AudioChannelLayout* layout = (AudioChannelLayout*)outData;
            memset(layout, 0, needed);
            layout->mChannelLayoutTag = kAudioChannelLayoutTag_UseChannelDescriptions;
            layout->mNumberChannelDescriptions = kChannelCount;
            layout->mChannelDescriptions[0].mChannelLabel = kAudioChannelLabel_Left;
            layout->mChannelDescriptions[1].mChannelLabel = kAudioChannelLabel_Right;
            *outDataSize = needed;
            return 0;
        }

        case kAudioDevicePropertyZeroTimeStampPeriod:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = kRingBufferFrames;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioDevicePropertyIcon:
            // No icon bundled yet; report absence rather than a broken URL.
            return kAudioHardwareUnknownPropertyError;

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus device_set_property_data(pid_t inClientProcessID,
                                         const AudioObjectPropertyAddress* address,
                                         UInt32 inDataSize,
                                         const void* inData,
                                         UInt32* outChangedCount,
                                         AudioObjectPropertyAddress changed[2]) {
    *outChangedCount = 0;

    switch (address->mSelector) {
        case kAudioDevicePropertyNominalSampleRate: {
            if (inDataSize != sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
            Float64 requested = *((const Float64*)inData);
            if (!is_supported_sample_rate(requested)) {
                return kAudioHardwareIllegalOperationError;
            }
            pthread_mutex_lock(&gStateMutex);
            Boolean different = gSampleRate != requested;
            AudioServerPlugInHostRef host = gHost;
            pthread_mutex_unlock(&gStateMutex);

            // Sample-rate changes have to go around through the host, which
            // stops IO before calling PerformDeviceConfigurationChange.
            if (different && host != NULL) {
                host->RequestDeviceConfigurationChange(host, kObjectID_Device, (UInt64)requested, NULL);
            }
            return 0;
        }

        case kCustomProperty_Name: {
            // Anyone may read the name; only DisEQ may change it.
            if (!client_is_our_app(inClientProcessID)) {
                return kAudioHardwareIllegalOperationError;
            }
            if (inDataSize != sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
            CFStringRef requested = *((const CFStringRef*)inData);
            if (requested == NULL) return kAudioHardwareIllegalOperationError;

            CFStringRef name = CFStringGetLength(requested) > 0 ? CFStringCreateCopy(NULL, requested)
                                                                : CFSTR(kDeviceDefaultName);
            if (CFStringGetLength(requested) == 0) {
                CFRetain(name);
            }

            pthread_mutex_lock(&gStateMutex);
            Boolean different = gDeviceName == NULL
                                || CFStringCompare(gDeviceName, name, 0) != kCFCompareEqualTo;
            if (different) {
                if (gDeviceName != NULL) {
                    CFRelease(gDeviceName);
                }
                gDeviceName = name;
            }
            pthread_mutex_unlock(&gStateMutex);

            if (!different) {
                CFRelease(name);
                return 0;
            }

            changed[0].mSelector = kAudioObjectPropertyName;
            changed[0].mScope = kAudioObjectPropertyScopeGlobal;
            changed[0].mElement = kAudioObjectPropertyElementMain;
            *outChangedCount = 1;
            return 0;
        }

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

#pragma mark - Properties: streams

static Boolean stream_has_property(const AudioObjectPropertyAddress* address) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioStreamPropertyIsActive:
        case kAudioStreamPropertyDirection:
        case kAudioStreamPropertyTerminalType:
        case kAudioStreamPropertyStartingChannel:
        case kAudioStreamPropertyLatency:
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat:
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats:
            return true;
        default:
            return false;
    }
}

static OSStatus stream_get_property_data_size(const AudioObjectPropertyAddress* address, UInt32* outSize) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
            *outSize = sizeof(AudioClassID);
            return 0;
        case kAudioObjectPropertyOwner:
            *outSize = sizeof(AudioObjectID);
            return 0;
        case kAudioStreamPropertyIsActive:
        case kAudioStreamPropertyDirection:
        case kAudioStreamPropertyTerminalType:
        case kAudioStreamPropertyStartingChannel:
        case kAudioStreamPropertyLatency:
            *outSize = sizeof(UInt32);
            return 0;
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat:
            *outSize = sizeof(AudioStreamBasicDescription);
            return 0;
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats:
            *outSize = (UInt32)(kSupportedSampleRateCount * sizeof(AudioStreamRangedDescription));
            return 0;
        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus stream_get_property_data(AudioObjectID objectID,
                                         const AudioObjectPropertyAddress* address,
                                         UInt32 inDataSize,
                                         UInt32* outDataSize,
                                         void* outData) {
    Boolean isInput = objectID == kObjectID_Stream_Input;

    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = kAudioObjectClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = kAudioStreamClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyOwner:
            if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
            *((AudioObjectID*)outData) = kObjectID_Device;
            *outDataSize = sizeof(AudioObjectID);
            return 0;

        case kAudioStreamPropertyIsActive:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = 1;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioStreamPropertyDirection:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = isInput ? 1 : 0;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioStreamPropertyTerminalType:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = isInput ? kAudioStreamTerminalTypeMicrophone
                                          : kAudioStreamTerminalTypeSpeaker;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioStreamPropertyStartingChannel:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = 1;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioStreamPropertyLatency:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = 0;
            *outDataSize = sizeof(UInt32);
            return 0;

        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: {
            if (inDataSize < sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
            pthread_mutex_lock(&gStateMutex);
            Float64 rate = gSampleRate;
            pthread_mutex_unlock(&gStateMutex);
            fill_stream_description((AudioStreamBasicDescription*)outData, rate);
            *outDataSize = sizeof(AudioStreamBasicDescription);
            return 0;
        }

        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats: {
            UInt32 capacity = inDataSize / sizeof(AudioStreamRangedDescription);
            UInt32 written = capacity < kSupportedSampleRateCount ? capacity
                                                                  : (UInt32)kSupportedSampleRateCount;
            AudioStreamRangedDescription* formats = (AudioStreamRangedDescription*)outData;
            for (UInt32 index = 0; index < written; index++) {
                fill_stream_description(&formats[index].mFormat, kSupportedSampleRates[index]);
                formats[index].mSampleRateRange.mMinimum = kSupportedSampleRates[index];
                formats[index].mSampleRateRange.mMaximum = kSupportedSampleRates[index];
            }
            *outDataSize = written * sizeof(AudioStreamRangedDescription);
            return 0;
        }

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus stream_set_property_data(const AudioObjectPropertyAddress* address,
                                         UInt32 inDataSize,
                                         const void* inData,
                                         UInt32* outChangedCount,
                                         AudioObjectPropertyAddress changed[2]) {
    *outChangedCount = 0;

    switch (address->mSelector) {
        case kAudioStreamPropertyIsActive:
            // Both streams are always active; accept the write so clients that
            // set it as a matter of course are not surprised.
            if (inDataSize != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            return 0;

        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: {
            if (inDataSize != sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
            const AudioStreamBasicDescription* requested = (const AudioStreamBasicDescription*)inData;
            if (requested->mFormatID != kAudioFormatLinearPCM
                || requested->mChannelsPerFrame != kChannelCount
                || requested->mBitsPerChannel != kBitsPerChannel
                || !(requested->mFormatFlags & kAudioFormatFlagIsFloat)) {
                return kAudioDeviceUnsupportedFormatError;
            }
            if (!is_supported_sample_rate(requested->mSampleRate)) {
                return kAudioDeviceUnsupportedFormatError;
            }

            pthread_mutex_lock(&gStateMutex);
            Boolean different = gSampleRate != requested->mSampleRate;
            AudioServerPlugInHostRef host = gHost;
            pthread_mutex_unlock(&gStateMutex);

            if (different && host != NULL) {
                host->RequestDeviceConfigurationChange(host, kObjectID_Device,
                                                       (UInt64)requested->mSampleRate, NULL);
            }
            (void)changed;
            return 0;
        }

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

#pragma mark - Properties: controls

static Boolean control_has_property(AudioObjectID objectID, const AudioObjectPropertyAddress* address) {
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioControlPropertyScope:
        case kAudioControlPropertyElement:
            return true;
        case kAudioLevelControlPropertyScalarValue:
        case kAudioLevelControlPropertyDecibelValue:
        case kAudioLevelControlPropertyDecibelRange:
        case kAudioLevelControlPropertyConvertScalarToDecibels:
        case kAudioLevelControlPropertyConvertDecibelsToScalar:
            return objectID == kObjectID_Volume_Output_Main;
        case kAudioBooleanControlPropertyValue:
            return objectID == kObjectID_Mute_Output_Main;
        default:
            return false;
    }
}

static OSStatus control_get_property_data_size(AudioObjectID objectID,
                                               const AudioObjectPropertyAddress* address,
                                               UInt32* outSize) {
    (void)objectID;
    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
            *outSize = sizeof(AudioClassID);
            return 0;
        case kAudioObjectPropertyOwner:
            *outSize = sizeof(AudioObjectID);
            return 0;
        case kAudioControlPropertyScope:
            *outSize = sizeof(AudioObjectPropertyScope);
            return 0;
        case kAudioControlPropertyElement:
            *outSize = sizeof(AudioObjectPropertyElement);
            return 0;
        case kAudioLevelControlPropertyScalarValue:
        case kAudioLevelControlPropertyDecibelValue:
        case kAudioLevelControlPropertyConvertScalarToDecibels:
        case kAudioLevelControlPropertyConvertDecibelsToScalar:
            *outSize = sizeof(Float32);
            return 0;
        case kAudioLevelControlPropertyDecibelRange:
            *outSize = sizeof(AudioValueRange);
            return 0;
        case kAudioBooleanControlPropertyValue:
            *outSize = sizeof(UInt32);
            return 0;
        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus control_get_property_data(AudioObjectID objectID,
                                          const AudioObjectPropertyAddress* address,
                                          UInt32 inDataSize,
                                          UInt32* outDataSize,
                                          void* outData) {
    Boolean isVolume = objectID == kObjectID_Volume_Output_Main;

    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = isVolume ? kAudioLevelControlClassID : kAudioBooleanControlClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyClass:
            if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
            *((AudioClassID*)outData) = isVolume ? kAudioVolumeControlClassID : kAudioMuteControlClassID;
            *outDataSize = sizeof(AudioClassID);
            return 0;

        case kAudioObjectPropertyOwner:
            if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
            *((AudioObjectID*)outData) = kObjectID_Device;
            *outDataSize = sizeof(AudioObjectID);
            return 0;

        case kAudioControlPropertyScope:
            if (inDataSize < sizeof(AudioObjectPropertyScope)) return kAudioHardwareBadPropertySizeError;
            *((AudioObjectPropertyScope*)outData) = kAudioObjectPropertyScopeOutput;
            *outDataSize = sizeof(AudioObjectPropertyScope);
            return 0;

        case kAudioControlPropertyElement:
            if (inDataSize < sizeof(AudioObjectPropertyElement)) return kAudioHardwareBadPropertySizeError;
            *((AudioObjectPropertyElement*)outData) = kAudioObjectPropertyElementMain;
            *outDataSize = sizeof(AudioObjectPropertyElement);
            return 0;

        case kAudioLevelControlPropertyScalarValue:
            if (inDataSize < sizeof(Float32)) return kAudioHardwareBadPropertySizeError;
            *((Float32*)outData) = atomic_load(&gVolumeScalar);
            *outDataSize = sizeof(Float32);
            return 0;

        case kAudioLevelControlPropertyDecibelValue:
            if (inDataSize < sizeof(Float32)) return kAudioHardwareBadPropertySizeError;
            *((Float32*)outData) = scalar_to_db(atomic_load(&gVolumeScalar));
            *outDataSize = sizeof(Float32);
            return 0;

        case kAudioLevelControlPropertyDecibelRange: {
            if (inDataSize < sizeof(AudioValueRange)) return kAudioHardwareBadPropertySizeError;
            AudioValueRange* range = (AudioValueRange*)outData;
            range->mMinimum = kVolumeMinDB;
            range->mMaximum = kVolumeMaxDB;
            *outDataSize = sizeof(AudioValueRange);
            return 0;
        }

        case kAudioLevelControlPropertyConvertScalarToDecibels: {
            if (inDataSize < sizeof(Float32)) return kAudioHardwareBadPropertySizeError;
            Float32 scalar = *((Float32*)outData);
            scalar = scalar < 0.0f ? 0.0f : (scalar > 1.0f ? 1.0f : scalar);
            *((Float32*)outData) = scalar_to_db(scalar);
            *outDataSize = sizeof(Float32);
            return 0;
        }

        case kAudioLevelControlPropertyConvertDecibelsToScalar: {
            if (inDataSize < sizeof(Float32)) return kAudioHardwareBadPropertySizeError;
            Float32 db = *((Float32*)outData);
            db = db < kVolumeMinDB ? kVolumeMinDB : (db > kVolumeMaxDB ? kVolumeMaxDB : db);
            *((Float32*)outData) = db_to_scalar(db);
            *outDataSize = sizeof(Float32);
            return 0;
        }

        case kAudioBooleanControlPropertyValue:
            if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            *((UInt32*)outData) = atomic_load(&gMuted) ? 1 : 0;
            *outDataSize = sizeof(UInt32);
            return 0;

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus control_set_property_data(AudioObjectID objectID,
                                          const AudioObjectPropertyAddress* address,
                                          UInt32 inDataSize,
                                          const void* inData,
                                          UInt32* outChangedCount,
                                          AudioObjectPropertyAddress changed[2]) {
    *outChangedCount = 0;

    switch (address->mSelector) {
        case kAudioLevelControlPropertyScalarValue:
        case kAudioLevelControlPropertyDecibelValue: {
            if (objectID != kObjectID_Volume_Output_Main) return kAudioHardwareUnknownPropertyError;
            if (inDataSize != sizeof(Float32)) return kAudioHardwareBadPropertySizeError;

            Float32 requested = *((const Float32*)inData);
            Float32 scalar = address->mSelector == kAudioLevelControlPropertyScalarValue
                                 ? requested
                                 : db_to_scalar(requested);
            scalar = scalar < 0.0f ? 0.0f : (scalar > 1.0f ? 1.0f : scalar);

            if (atomic_exchange(&gVolumeScalar, scalar) == scalar) {
                return 0;
            }

            // Both representations moved, whichever was written.
            changed[0].mSelector = kAudioLevelControlPropertyScalarValue;
            changed[0].mScope = kAudioObjectPropertyScopeGlobal;
            changed[0].mElement = kAudioObjectPropertyElementMain;
            changed[1].mSelector = kAudioLevelControlPropertyDecibelValue;
            changed[1].mScope = kAudioObjectPropertyScopeGlobal;
            changed[1].mElement = kAudioObjectPropertyElementMain;
            *outChangedCount = 2;
            return 0;
        }

        case kAudioBooleanControlPropertyValue: {
            if (objectID != kObjectID_Mute_Output_Main) return kAudioHardwareUnknownPropertyError;
            if (inDataSize != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;

            bool muted = *((const UInt32*)inData) != 0;
            if (atomic_exchange(&gMuted, muted) == muted) {
                return 0;
            }

            changed[0].mSelector = kAudioBooleanControlPropertyValue;
            changed[0].mScope = kAudioObjectPropertyScopeGlobal;
            changed[0].mElement = kAudioObjectPropertyElementMain;
            *outChangedCount = 1;
            return 0;
        }

        default:
            return kAudioHardwareUnknownPropertyError;
    }
}

#pragma mark - Property dispatch

static Boolean kd_HasProperty(AudioServerPlugInDriverRef inDriver,
                              AudioObjectID inObjectID,
                              pid_t inClientProcessID,
                              const AudioObjectPropertyAddress* inAddress) {
    (void)inClientProcessID;
    if (inDriver != gDriverRef || inAddress == NULL) {
        return false;
    }

    switch (inObjectID) {
        case kObjectID_PlugIn:
            return plugin_has_property(inAddress);
        case kObjectID_Device:
            return device_has_property(inAddress);
        case kObjectID_Stream_Input:
        case kObjectID_Stream_Output:
            return stream_has_property(inAddress);
        case kObjectID_Volume_Output_Main:
        case kObjectID_Mute_Output_Main:
            return control_has_property(inObjectID, inAddress);
        default:
            return false;
    }
}

static OSStatus kd_IsPropertySettable(AudioServerPlugInDriverRef inDriver,
                                      AudioObjectID inObjectID,
                                      pid_t inClientProcessID,
                                      const AudioObjectPropertyAddress* inAddress,
                                      Boolean* outIsSettable) {
    (void)inClientProcessID;
    if (inDriver != gDriverRef || inAddress == NULL || outIsSettable == NULL) {
        return kAudioHardwareBadObjectError;
    }
    if (!kd_HasProperty(inDriver, inObjectID, inClientProcessID, inAddress)) {
        return kAudioHardwareUnknownPropertyError;
    }

    switch (inObjectID) {
        case kObjectID_PlugIn:
            *outIsSettable = inAddress->mSelector == kCustomProperty_Shown;
            return 0;

        case kObjectID_Device:
            *outIsSettable = inAddress->mSelector == kAudioDevicePropertyNominalSampleRate
                             || inAddress->mSelector == kCustomProperty_Name;
            return 0;

        case kObjectID_Stream_Input:
        case kObjectID_Stream_Output:
            *outIsSettable = inAddress->mSelector == kAudioStreamPropertyIsActive
                             || inAddress->mSelector == kAudioStreamPropertyVirtualFormat
                             || inAddress->mSelector == kAudioStreamPropertyPhysicalFormat;
            return 0;

        case kObjectID_Volume_Output_Main:
            *outIsSettable = inAddress->mSelector == kAudioLevelControlPropertyScalarValue
                             || inAddress->mSelector == kAudioLevelControlPropertyDecibelValue;
            return 0;

        case kObjectID_Mute_Output_Main:
            *outIsSettable = inAddress->mSelector == kAudioBooleanControlPropertyValue;
            return 0;

        default:
            *outIsSettable = false;
            return 0;
    }
}

static OSStatus kd_GetPropertyDataSize(AudioServerPlugInDriverRef inDriver,
                                       AudioObjectID inObjectID,
                                       pid_t inClientProcessID,
                                       const AudioObjectPropertyAddress* inAddress,
                                       UInt32 inQualifierDataSize,
                                       const void* inQualifierData,
                                       UInt32* outDataSize) {
    (void)inClientProcessID;
    (void)inQualifierDataSize;
    (void)inQualifierData;
    if (inDriver != gDriverRef || inAddress == NULL || outDataSize == NULL) {
        return kAudioHardwareBadObjectError;
    }

    switch (inObjectID) {
        case kObjectID_PlugIn:
            return plugin_get_property_data_size(inAddress, outDataSize);
        case kObjectID_Device:
            return device_get_property_data_size(inAddress, outDataSize);
        case kObjectID_Stream_Input:
        case kObjectID_Stream_Output:
            return stream_get_property_data_size(inAddress, outDataSize);
        case kObjectID_Volume_Output_Main:
        case kObjectID_Mute_Output_Main:
            return control_get_property_data_size(inObjectID, inAddress, outDataSize);
        default:
            return kAudioHardwareBadObjectError;
    }
}

static OSStatus kd_GetPropertyData(AudioServerPlugInDriverRef inDriver,
                                   AudioObjectID inObjectID,
                                   pid_t inClientProcessID,
                                   const AudioObjectPropertyAddress* inAddress,
                                   UInt32 inQualifierDataSize,
                                   const void* inQualifierData,
                                   UInt32 inDataSize,
                                   UInt32* outDataSize,
                                   void* outData) {
    (void)inClientProcessID;
    if (inDriver != gDriverRef || inAddress == NULL || outDataSize == NULL || outData == NULL) {
        return kAudioHardwareBadObjectError;
    }

    switch (inObjectID) {
        case kObjectID_PlugIn:
            return plugin_get_property_data(inAddress, inQualifierDataSize, inQualifierData,
                                            inDataSize, outDataSize, outData);
        case kObjectID_Device:
            return device_get_property_data(inAddress, inDataSize, outDataSize, outData);
        case kObjectID_Stream_Input:
        case kObjectID_Stream_Output:
            return stream_get_property_data(inObjectID, inAddress, inDataSize, outDataSize, outData);
        case kObjectID_Volume_Output_Main:
        case kObjectID_Mute_Output_Main:
            return control_get_property_data(inObjectID, inAddress, inDataSize, outDataSize, outData);
        default:
            return kAudioHardwareBadObjectError;
    }
}

static OSStatus kd_SetPropertyData(AudioServerPlugInDriverRef inDriver,
                                   AudioObjectID inObjectID,
                                   pid_t inClientProcessID,
                                   const AudioObjectPropertyAddress* inAddress,
                                   UInt32 inQualifierDataSize,
                                   const void* inQualifierData,
                                   UInt32 inDataSize,
                                   const void* inData) {
    (void)inQualifierDataSize;
    (void)inQualifierData;
    if (inDriver != gDriverRef || inAddress == NULL || inData == NULL) {
        return kAudioHardwareBadObjectError;
    }

    AudioObjectPropertyAddress changed[2];
    UInt32 changedCount = 0;
    OSStatus status;

    switch (inObjectID) {
        case kObjectID_PlugIn:
            status = plugin_set_property_data(inAddress, inDataSize, inData, &changedCount, changed);
            break;
        case kObjectID_Device:
            status = device_set_property_data(inClientProcessID, inAddress, inDataSize, inData,
                                              &changedCount, changed);
            break;
        case kObjectID_Stream_Input:
        case kObjectID_Stream_Output:
            status = stream_set_property_data(inAddress, inDataSize, inData, &changedCount, changed);
            break;
        case kObjectID_Volume_Output_Main:
        case kObjectID_Mute_Output_Main:
            status = control_set_property_data(inObjectID, inAddress, inDataSize, inData,
                                               &changedCount, changed);
            break;
        default:
            return kAudioHardwareBadObjectError;
    }

    if (status == 0 && changedCount > 0) {
        pthread_mutex_lock(&gStateMutex);
        AudioServerPlugInHostRef host = gHost;
        pthread_mutex_unlock(&gStateMutex);
        if (host != NULL) {
            host->PropertiesChanged(host, inObjectID, changedCount, changed);
        }
    }

    return status;
}

#pragma mark - IO

static OSStatus kd_StartIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID) {
    (void)inClientID;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }

    pthread_mutex_lock(&gStateMutex);
    UInt64 count = atomic_load(&gIOCount);
    if (count == 0) {
        // First client in: reset the clock and clear whatever the last session
        // left behind.
        memset(gRingBuffer, 0, sizeof(gRingBuffer));
        gAnchorHostTime = mach_absolute_time();
        atomic_store(&gTimestampCount, 0);
        gAppliedGain = scalar_to_gain(atomic_load(&gVolumeScalar));
        recalculate_host_ticks_per_frame();
        if (gShared != NULL) {
            memset(gSharedSamples, 0,
                   (size_t)kRingBufferFrames * kChannelCount * sizeof(Float32));
            atomic_store(&gShared->written, 0);
            shared_publish_rate(gSampleRate);
            atomic_store(&gShared->running, 1);
        }
    }
    atomic_store(&gIOCount, count + 1);
    pthread_mutex_unlock(&gStateMutex);

    return 0;
}

static OSStatus kd_StopIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID) {
    (void)inClientID;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }

    pthread_mutex_lock(&gStateMutex);
    UInt64 count = atomic_load(&gIOCount);
    if (count == 0) {
        pthread_mutex_unlock(&gStateMutex);
        return kAudioHardwareIllegalOperationError;
    }
    atomic_store(&gIOCount, count - 1);
    if (count - 1 == 0 && gShared != NULL) {
        // Last client out. The reader goes silent rather than replaying the
        // tail of the last session.
        atomic_store(&gShared->running, 0);
    }
    pthread_mutex_unlock(&gStateMutex);

    return 0;
}

/// The device pretends to be hardware whose buffer wraps every
/// kRingBufferFrames frames. Each wrap is one "zero timestamp", and the HAL
/// interpolates between them to schedule IO.
static OSStatus kd_GetZeroTimeStamp(AudioServerPlugInDriverRef inDriver,
                                    AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID,
                                    Float64* outSampleTime,
                                    UInt64* outHostTime,
                                    UInt64* outSeed) {
    (void)inClientID;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }

    UInt64 currentHostTime = mach_absolute_time();
    Float64 hostTicksPerRing = gHostTicksPerFrame * (Float64)kRingBufferFrames;
    UInt64 count = atomic_load(&gTimestampCount);

    // Advance one period at a time; the HAL calls this often enough that
    // catching up in single steps is enough.
    UInt64 nextHostTime = gAnchorHostTime + (UInt64)(((Float64)count + 1.0) * hostTicksPerRing);
    if (nextHostTime <= currentHostTime) {
        count++;
        atomic_store(&gTimestampCount, count);
    }

    *outSampleTime = (Float64)(count * (UInt64)kRingBufferFrames);
    *outHostTime = gAnchorHostTime + (UInt64)((Float64)count * hostTicksPerRing);
    *outSeed = 1;

    return 0;
}

static OSStatus kd_WillDoIOOperation(AudioServerPlugInDriverRef inDriver,
                                     AudioObjectID inDeviceObjectID,
                                     UInt32 inClientID,
                                     UInt32 inOperationID,
                                     Boolean* outWillDo,
                                     Boolean* outWillDoInPlace) {
    (void)inClientID;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }

    Boolean willDo = false;
    Boolean inPlace = true;

    switch (inOperationID) {
        case kAudioServerPlugInIOOperationWriteMix:
        case kAudioServerPlugInIOOperationReadInput:
            willDo = true;
            break;
        default:
            break;
    }

    if (outWillDo != NULL) *outWillDo = willDo;
    if (outWillDoInPlace != NULL) *outWillDoInPlace = inPlace;
    return 0;
}

static OSStatus kd_BeginIOOperation(AudioServerPlugInDriverRef inDriver,
                                    AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID,
                                    UInt32 inOperationID,
                                    UInt32 inIOBufferFrameSize,
                                    const AudioServerPlugInIOCycleInfo* inIOCycleInfo) {
    (void)inClientID;
    (void)inOperationID;
    (void)inIOBufferFrameSize;
    (void)inIOCycleInfo;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    return inDeviceObjectID == kObjectID_Device ? 0 : kAudioHardwareBadObjectError;
}

/// The loopback itself.
///
/// Real-time thread: no allocation, no locks, no logging. Volume and mute are
/// read once per cycle from atomics; the gain then walks towards its target one
/// frame at a time, so a slider drag does not step.
static OSStatus kd_DoIOOperation(AudioServerPlugInDriverRef inDriver,
                                 AudioObjectID inDeviceObjectID,
                                 AudioObjectID inStreamObjectID,
                                 UInt32 inClientID,
                                 UInt32 inOperationID,
                                 UInt32 inIOBufferFrameSize,
                                 const AudioServerPlugInIOCycleInfo* inIOCycleInfo,
                                 void* ioMainBuffer,
                                 void* ioSecondaryBuffer) {
    (void)inStreamObjectID;
    (void)inClientID;
    (void)ioSecondaryBuffer;

    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    if (inDeviceObjectID != kObjectID_Device) {
        return kAudioHardwareBadObjectError;
    }
    if (ioMainBuffer == NULL || inIOCycleInfo == NULL) {
        return 0;
    }

    Float32* samples = (Float32*)ioMainBuffer;
    const UInt32 ringFrames = kRingBufferFrames;

    switch (inOperationID) {
        case kAudioServerPlugInIOOperationWriteMix: {
            // Apps' audio arrives here already mixed. Fold it into the ring at
            // its sample time, then silence the buffer: this device has no
            // hardware to play to.
            SInt64 sampleTime = (SInt64)inIOCycleInfo->mOutputTime.mSampleTime;
            bool muted = atomic_load(&gMuted);
            float targetGain = muted ? 0.0f : scalar_to_gain(atomic_load(&gVolumeScalar));
            float gain = gAppliedGain;

            for (UInt32 frame = 0; frame < inIOBufferFrameSize; frame++) {
                gain += (targetGain - gain) * kGainSmoothing;

                SInt64 writePosition = (sampleTime + (SInt64)frame) % (SInt64)ringFrames;
                if (writePosition < 0) {
                    writePosition += ringFrames;
                }
                // Stay one ring-length ahead, clearing what the reader has
                // already passed. Without this, a stopped stream leaves a loop
                // of stale audio behind.
                SInt64 clearPosition = (writePosition + (SInt64)(ringFrames / 2)) % (SInt64)ringFrames;

                for (UInt32 channel = 0; channel < kChannelCount; channel++) {
                    UInt32 source = frame * kChannelCount + channel;
                    Float32 raw = samples[source];
                    gRingBuffer[writePosition * kChannelCount + channel] += raw * gain;
                    gRingBuffer[clearPosition * kChannelCount + channel] = 0.0f;
                    if (gSharedSamples != NULL) {
                        // Unattenuated, and deliberately so. This volume control
                        // is a control surface, not a gain stage: the app reads
                        // it and applies it on the way to the hardware, at the
                        // hardware's own volume control where there is one.
                        // Applying it here as well multiplied the two, and at a
                        // low setting the product is inaudible.
                        //
                        // Assigned, not accumulated: the app is the only reader
                        // and it reads each frame once, so there is nothing to
                        // mix with and nothing to clear behind.
                        gSharedSamples[writePosition * kChannelCount + channel] = raw;
                    }
                }
            }

            if (gShared != NULL) {
                // Published after the frames it describes, so a reader that
                // sees this sample time can read up to it.
                atomic_store(&gShared->written, sampleTime + (SInt64)inIOBufferFrameSize);
            }

            gAppliedGain = gain;
            memset(samples, 0, (size_t)inIOBufferFrameSize * kChannelCount * sizeof(Float32));
            return 0;
        }

        case kAudioServerPlugInIOOperationReadInput: {
            // And out again, at the input stream's sample time. Each frame is
            // cleared once it is a whole ring behind — the same slot, but only
            // after the first wrap, so nothing the writer put there during
            // start-up is thrown away before it is read.
            SInt64 sampleTime = (SInt64)inIOCycleInfo->mInputTime.mSampleTime;

            for (UInt32 frame = 0; frame < inIOBufferFrameSize; frame++) {
                SInt64 position = sampleTime + (SInt64)frame;
                SInt64 readPosition = position % (SInt64)ringFrames;
                if (readPosition < 0) {
                    readPosition += ringFrames;
                }
                Boolean wrapped = position >= (SInt64)ringFrames;

                for (UInt32 channel = 0; channel < kChannelCount; channel++) {
                    UInt32 destination = frame * kChannelCount + channel;
                    UInt32 source = (UInt32)(readPosition * kChannelCount + channel);
                    samples[destination] = gRingBuffer[source];
                    if (wrapped) {
                        gRingBuffer[source] = 0.0f;
                    }
                }
            }
            return 0;
        }

        default:
            return 0;
    }
}

static OSStatus kd_EndIOOperation(AudioServerPlugInDriverRef inDriver,
                                  AudioObjectID inDeviceObjectID,
                                  UInt32 inClientID,
                                  UInt32 inOperationID,
                                  UInt32 inIOBufferFrameSize,
                                  const AudioServerPlugInIOCycleInfo* inIOCycleInfo) {
    (void)inClientID;
    (void)inOperationID;
    (void)inIOBufferFrameSize;
    (void)inIOCycleInfo;
    if (inDriver != gDriverRef) {
        return kAudioHardwareBadObjectError;
    }
    return inDeviceObjectID == kObjectID_Device ? 0 : kAudioHardwareBadObjectError;
}
