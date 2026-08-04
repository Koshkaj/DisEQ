//! Reports which backend drives each control on this machine. Read-only.

use kd_core::{DisplayCatalog, Service};
use kd_sys::gamma;

fn main() {
    let service = Service::start();
    let catalog = DisplayCatalog::load();

    println!("audio");
    match service.output_name() {
        Some(name) => println!("   output: {name}"),
        None => println!("   output: none"),
    }
    println!(
        "   volume: {:?}   muted: {:?}",
        service.volume(),
        service.is_muted()
    );

    for display in &catalog.displays {
        let id = display.id();
        println!("\n── {} (id {})", display.name(), id.0);
        println!("   brightness backend: {:?}", service.backend(id));
        println!("   brightness value:   {:?}", service.brightness(id));
        println!("   gamma honoured:     {}", gamma::verify(id));
        println!("   ddc match:          {:?}", service.ddc_confidence(id));
        println!("   auto brightness:    {:?}", service.auto_brightness(id));
        println!(
            "   hidpi twin avail:   on={} off={}",
            service.hidpi_available(id, true),
            service.hidpi_available(id, false)
        );
        println!("   connected:          {}", service.is_connected(id));
        println!(
            "   selectable modes:   {}",
            display.selectable_modes().len()
        );
    }
}
