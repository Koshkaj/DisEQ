//! DDC/CI over the Apple Silicon `IOAVService` path.
//!
//! The Intel `IOFramebuffer` I2C route (`IOFBCopyI2CInterfaceForBus` /
//! `IOI2CSendRequest`) is not implemented: on Apple Silicon those calls return
//! success but never put anything on the wire, so they are worse than useless.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::Duration;

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};

use crate::display::{self, DisplayId};
use crate::dylib::Framework;
use crate::iokit::{self, IoService, PropertyDictionary};

/// I2C address of the display's DDC/CI endpoint.
const DDC_CHIP_ADDRESS: u32 = 0x37;
/// Sub-address `IOAVServiceRead/WriteI2C` expects for DDC traffic.
const DDC_DATA_OFFSET: u32 = 0x51;
/// Checksum seed for outgoing packets: the 8-bit form of the chip address.
///
/// Some implementations seed with `0x6E ^ 0x51`, folding in the sub-address.
/// Displays reject those packets — a Dell U2720Q answers every such request
/// with a null message — so the plain address is what actually works.
const REQUEST_CHECKSUM_SEED: u8 = (DDC_CHIP_ADDRESS as u8) << 1;
/// The DDC/CI spec requires giving the display time to compose a reply.
const REPLY_DELAY: Duration = Duration::from_millis(50);
/// Levels to climb looking for the display metadata node.
///
/// The node carrying `DisplayAttributes` sat 9 levels above the AV service on
/// the hardware this was developed against, so the limit has headroom.
const ANCESTRY_DEPTH: usize = 14;
/// Displays routinely drop the first request, so reads are retried.
const READ_ATTEMPTS: usize = 5;
const RETRY_DELAY: Duration = Duration::from_millis(20);
/// Sending a set twice makes it stick on displays that ignore a lone write.
const WRITE_CYCLES: usize = 2;
const WRITE_DELAY: Duration = Duration::from_millis(10);

pub mod vcp {
    pub const LUMINANCE: u8 = 0x10;
    pub const CONTRAST: u8 = 0x12;
    pub const RED_GAIN: u8 = 0x16;
    pub const GREEN_GAIN: u8 = 0x18;
    pub const BLUE_GAIN: u8 = 0x1A;
    pub const INPUT_SOURCE: u8 = 0x60;
    pub const SPEAKER_VOLUME: u8 = 0x62;
    pub const AUDIO_MUTE: u8 = 0x8D;
    pub const POWER_MODE: u8 = 0xD6;
}

type IOAVServiceRef = *mut c_void;
type CreateWithService = unsafe extern "C" fn(*const c_void, IoService) -> IOAVServiceRef;
type ReadI2C = unsafe extern "C" fn(IOAVServiceRef, u32, u32, *mut u8, u32) -> i32;
type WriteI2C = unsafe extern "C" fn(IOAVServiceRef, u32, u32, *const u8, u32) -> i32;

struct AvSymbols {
    create: CreateWithService,
    read: ReadI2C,
    write: WriteI2C,
}

fn symbols() -> Option<&'static AvSymbols> {
    static SYMBOLS: OnceLock<Option<AvSymbols>> = OnceLock::new();
    SYMBOLS
        .get_or_init(|| {
            let iokit = Framework::open("/System/Library/Frameworks/IOKit.framework/IOKit")?;
            unsafe {
                Some(AvSymbols {
                    create: iokit.symbol("IOAVServiceCreateWithService")?,
                    read: iokit.symbol("IOAVServiceReadI2C")?,
                    write: iokit.symbol("IOAVServiceWriteI2C")?,
                })
            }
        })
        .as_ref()
}

/// How confidently an `IOAVService` was matched to a display.
///
/// CoreGraphics and IOKit do not always report the same vendor/product numbers
/// for the same panel, so the match can degrade to positional guessing. The UI
/// needs to know when that happened: with two or more externals attached, a
/// positional match may well be driving the wrong monitor.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MatchConfidence {
    /// Vendor, product *and* serial all agreed.
    Exact,
    /// Vendor and product agreed; the display reports no usable serial.
    VendorProduct,
    /// Nothing matched; assigned by enumeration order.
    Positional,
}

pub struct DdcLink {
    service: IOAVServiceRef,
    pub confidence: MatchConfidence,
}

// The service handle is only passed back to IOAVService calls, which are
// serialised by the caller owning `&mut`.
unsafe impl Send for DdcLink {}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct VcpValue {
    pub current: u16,
    pub max: u16,
}

impl DdcLink {
    pub fn read(&self, code: u8) -> Option<VcpValue> {
        let symbols = symbols()?;
        for attempt in 0..READ_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(RETRY_DELAY);
            }
            if let Some(value) = self.try_read(symbols, code) {
                return Some(value);
            }
        }
        None
    }

    fn try_read(&self, symbols: &AvSymbols, code: u8) -> Option<VcpValue> {
        let mut request = [0x82u8, 0x01, code, 0];
        request[3] = checksum(&request[..3]);
        let written = unsafe {
            (symbols.write)(
                self.service,
                DDC_CHIP_ADDRESS,
                DDC_DATA_OFFSET,
                request.as_ptr(),
                request.len() as u32,
            )
        };
        if written != 0 {
            return None;
        }

        std::thread::sleep(REPLY_DELAY);

        let mut reply = [0u8; 12];
        let read = unsafe {
            (symbols.read)(
                self.service,
                DDC_CHIP_ADDRESS,
                DDC_DATA_OFFSET,
                reply.as_mut_ptr(),
                reply.len() as u32,
            )
        };
        if read != 0 {
            return None;
        }

        // Reply layout: [src, len, 0x02, result, vcp, type, max_hi, max_lo,
        //                cur_hi, cur_lo, checksum].
        // A length byte of 0x80 means the display sent a null message, i.e. it
        // did not answer; a non-zero result byte means it rejected the feature.
        if reply[1] == 0x80 || reply[2] != 0x02 || reply[3] != 0x00 || reply[4] != code {
            return None;
        }
        Some(VcpValue {
            max: u16::from_be_bytes([reply[6], reply[7]]),
            current: u16::from_be_bytes([reply[8], reply[9]]),
        })
    }

    pub fn write(&self, code: u8, value: u16) -> bool {
        let Some(symbols) = symbols() else {
            return false;
        };
        let [high, low] = value.to_be_bytes();
        let mut packet = [0x84u8, 0x03, code, high, low, 0];
        packet[5] = checksum(&packet[..5]);

        let mut ok = false;
        for cycle in 0..WRITE_CYCLES {
            if cycle > 0 {
                std::thread::sleep(WRITE_DELAY);
            }
            let result = unsafe {
                (symbols.write)(
                    self.service,
                    DDC_CHIP_ADDRESS,
                    DDC_DATA_OFFSET,
                    packet.as_ptr(),
                    packet.len() as u32,
                )
            };
            ok |= result == 0;
        }
        ok
    }
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(REQUEST_CHECKSUM_SEED, |acc, b| acc ^ b)
}

/// Builds a DDC link for every external display that answers on I2C.
pub fn discover() -> Vec<(DisplayId, DdcLink)> {
    let Some(symbols) = symbols() else {
        return Vec::new();
    };

    let externals: Vec<DisplayId> = display::online_displays()
        .into_iter()
        .filter(|id| !display::snapshot(*id).is_builtin)
        .collect();
    if externals.is_empty() {
        return Vec::new();
    }

    let mut candidates: Vec<(IOAVServiceRef, Option<PanelIdentity>)> = Vec::new();
    iokit::for_each_service("DCPAVServiceProxy", |service| {
        // Some drivers omit "Location" entirely, so only a positive "not
        // external" is treated as a reason to skip.
        if iokit::property_string(service, "Location").is_some_and(|l| l != "External") {
            return true;
        }

        let av = unsafe { (symbols.create)(std::ptr::null(), service) };
        if av.is_null() {
            return true;
        }

        // Confirm the endpoint really answers before trusting it.
        let mut probe = [0u8; 32];
        let answered = unsafe {
            (symbols.read)(
                av,
                DDC_CHIP_ADDRESS,
                DDC_DATA_OFFSET,
                probe.as_mut_ptr(),
                probe.len() as u32,
            )
        } == 0;

        if answered {
            candidates.push((av, panel_identity(service)));
        }
        true
    });

    match_candidates(externals, candidates)
}

fn match_candidates(
    externals: Vec<DisplayId>,
    candidates: Vec<(IOAVServiceRef, Option<PanelIdentity>)>,
) -> Vec<(DisplayId, DdcLink)> {
    let mut links: Vec<(DisplayId, DdcLink)> = Vec::new();
    let mut unmatched = Vec::new();

    for (service, identity) in candidates {
        let taken: Vec<DisplayId> = links.iter().map(|(id, _)| *id).collect();
        let matched = identity.as_ref().and_then(|identity| {
            externals
                .iter()
                .filter(|id| !taken.contains(id))
                .find_map(|id| identity.confidence_against(*id).map(|c| (*id, c)))
        });

        match matched {
            Some((id, confidence)) => links.push((
                id,
                DdcLink {
                    service,
                    confidence,
                },
            )),
            None => unmatched.push(service),
        }
    }

    // Anything left over is assigned by order, which is a guess.
    let remaining: Vec<DisplayId> = externals
        .into_iter()
        .filter(|id| !links.iter().any(|(matched, _)| matched == id))
        .collect();
    for (id, service) in remaining.into_iter().zip(unmatched) {
        links.push((
            id,
            DdcLink {
                service,
                confidence: MatchConfidence::Positional,
            },
        ));
    }

    links
}

struct PanelIdentity {
    vendor: Option<u32>,
    product: Option<u32>,
    serial: Option<u32>,
}

impl PanelIdentity {
    fn confidence_against(&self, id: DisplayId) -> Option<MatchConfidence> {
        let snapshot = display::snapshot(id);
        let vendor = self.vendor?;
        let product = self.product?;
        if vendor != snapshot.vendor || product != snapshot.model {
            return None;
        }
        match self.serial {
            Some(serial) if serial == snapshot.serial => Some(MatchConfidence::Exact),
            Some(_) => None,
            None => Some(MatchConfidence::VendorProduct),
        }
    }
}

/// Reads the panel's identity from `DisplayAttributes`, the modern IORegistry
/// location, falling back to the legacy top-level keys.
fn panel_identity(service: IoService) -> Option<PanelIdentity> {
    let attributes = iokit::find_in_ancestry(service, ANCESTRY_DEPTH, |node| {
        let value = iokit::search_child_property(node, "DisplayAttributes")?;
        let dictionary = value.downcast::<CFDictionary>().ok()?;
        Some(unsafe { CFRetained::cast_unchecked::<PropertyDictionary>(dictionary) })
    });

    if let Some(product) = attributes.and_then(|attributes| {
        attributes
            .get(&CFString::from_str("ProductAttributes"))
            .and_then(|value| value.downcast::<CFDictionary>().ok())
            .map(|dict| unsafe { CFRetained::cast_unchecked::<PropertyDictionary>(dict) })
    }) {
        return Some(PanelIdentity {
            vendor: dict_u32(&product, "LegacyManufacturerID")
                .or_else(|| dict_u32(&product, "ManufacturerID")),
            product: dict_u32(&product, "ProductID"),
            serial: dict_u32(&product, "SerialNumber"),
        });
    }

    Some(PanelIdentity {
        vendor: registry_u32(service, "DisplayVendorID"),
        product: registry_u32(service, "DisplayProductID"),
        serial: registry_u32(service, "DisplaySerialNumber"),
    })
}

fn dict_u32(dict: &PropertyDictionary, key: &str) -> Option<u32> {
    number_to_u32(&dict.get(&CFString::from_str(key))?)
}

fn registry_u32(service: IoService, key: &str) -> Option<u32> {
    let value = iokit::search_parent_property(service, key)?;
    number_to_u32(&value)
}

fn number_to_u32(value: &CFRetained<CFType>) -> Option<u32> {
    // IORegistry integers arrive as CFNumber of varying width.
    let number = value.downcast_ref::<CFNumber>()?;
    number.as_i64().map(|v| v as u32)
}
