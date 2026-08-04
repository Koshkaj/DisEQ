//! Read-only DDC probe, exercised through the worker thread. Writes nothing.

use kd_core::ddc::{self, DdcService};
use kd_sys::ddc::vcp;
use kd_sys::display;

fn main() {
    let survey = ddc::survey();
    if survey.is_empty() {
        println!("no DDC-capable external displays found");
        return;
    }

    let service = DdcService::start();
    for (id, confidence) in survey {
        let snapshot = display::snapshot(id);
        println!("── {} (id {})  match: {confidence:?}", snapshot.name, id.0);

        for (name, code) in [
            ("luminance      0x10", vcp::LUMINANCE),
            ("contrast       0x12", vcp::CONTRAST),
            ("input source   0x60", vcp::INPUT_SOURCE),
            ("speaker volume 0x62", vcp::SPEAKER_VOLUME),
            ("power mode     0xD6", vcp::POWER_MODE),
        ] {
            match service.read(id, code).recv() {
                Ok(Some(value)) => println!("   {name}: {} / {}", value.current, value.max),
                _ => println!("   {name}: unsupported"),
            }
        }
        println!();
    }
}
