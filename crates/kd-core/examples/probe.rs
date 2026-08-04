use kd_core::DisplayCatalog;

fn main() {
    let catalog = DisplayCatalog::load();
    println!("{} display(s) online\n", catalog.displays.len());

    for d in &catalog.displays {
        let s = &d.snapshot;
        println!("── {} (id {})", s.name, s.id.0);
        println!(
            "   builtin={} main={} active={} online={} rotation={}°",
            s.is_builtin, s.is_main, s.is_active, s.is_online, s.rotation
        );
        println!(
            "   vendor=0x{:04X} model=0x{:04X} serial=0x{:08X} unit={}",
            s.vendor, s.model, s.serial, s.unit
        );
        println!(
            "   bounds=({:.0},{:.0}) {:.0}x{:.0}  mirrors={:?}",
            s.origin.0,
            s.origin.1,
            s.size.0,
            s.size.1,
            s.mirrors.map(|m| m.0)
        );

        if let Some(m) = &s.current_mode {
            println!(
                "   current: {}x{} pts / {}x{} px @ {:.0}Hz  hidpi={}",
                m.width,
                m.height,
                m.pixel_width,
                m.pixel_height,
                m.refresh_rate,
                m.is_hidpi()
            );
        }
        if let Some(m) = &d.native {
            println!("   native:  {}x{} px", m.pixel_width, m.pixel_height);
        }
        if let Some(scale) = d.resolution_scale() {
            println!("   scale:   {:.0}%", scale * 100.0);
        }

        let selectable = d.selectable_modes();
        println!(
            "   {} total modes, {} selectable (index {:?})",
            d.modes.len(),
            selectable.len(),
            d.current_mode_index()
        );
        for m in selectable.iter().rev().take(6) {
            println!(
                "      {}x{}  ({}x{} px){}",
                m.width,
                m.height,
                m.pixel_width,
                m.pixel_height,
                if m.is_hidpi() { "  HiDPI" } else { "" }
            );
        }
        println!();
    }
}
