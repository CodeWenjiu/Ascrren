use std::rc::Rc;
use std::time::{Duration, Instant};

use msu2_core::display::{TileGrid, encode_tile_stream};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};

slint::include_modules!();

const WIDTH: usize = 160;
const HEIGHT: usize = 80;
const TILE: usize = 16;
const TICKS: usize = 24;

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        Ok(())
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform {
        window: window.clone(),
        start: Instant::now(),
    }))
    .expect("Slint platform was already initialized");

    let ui = AppWindow::new()?;
    window.set_size(slint::PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
    ui.show()?;

    let mut frame = vec![Rgb565Pixel(0); WIDTH * HEIGHT];
    let mut previous = vec![0u16; WIDTH * HEIGHT];
    let grid = TileGrid::new(WIDTH as u16, HEIGHT as u16, TILE as u16).expect("valid tile grid");
    let mut rendered_ticks = 0usize;
    let mut total_dirty_tiles = 0usize;
    let mut total_wire_bytes = 0usize;

    for tick in 0..TICKS {
        ui.set_tick((tick % 100) as i32);
        if tick % 4 == 0 {
            ui.set_label(format!("MSU2 tick {tick}").into());
        }

        slint::platform::update_timers_and_animations();

        let mut region_printed = false;
        window.draw_if_needed(|renderer| {
            let region = renderer.render(&mut frame, WIDTH);
            let origin = region.bounding_box_origin();
            let size = region.bounding_box_size();
            println!(
                "tick={tick:02} slint_region=({}, {}) {}x{}",
                origin.x, origin.y, size.width, size.height
            );
            region_printed = true;
        });

        if !region_printed {
            println!("tick={tick:02} slint_region=none");
        } else {
            rendered_ticks += 1;
        }

        let current: Vec<u16> = frame.iter().map(|pixel| pixel.0).collect();
        let changed_pixels = current
            .iter()
            .zip(&previous)
            .filter(|(current, previous)| current != previous)
            .count();
        let dirty_tiles = grid.diff(&previous, &current).expect("valid framebuffers");

        let mut wire_bytes = 0usize;
        let mut preview = None;
        for tile in &dirty_tiles {
            let packet = encode_tile_stream(&current, WIDTH as u16, grid.tile_rect(*tile))
                .expect("valid tile");
            wire_bytes += packet.len();
            if preview.is_none() {
                preview = Some(packet[..packet.len().min(18)].to_vec());
            }
        }

        let tile_coords: Vec<_> = dirty_tiles.iter().map(|tile| (tile.x, tile.y)).collect();
        total_dirty_tiles += dirty_tiles.len();
        total_wire_bytes += wire_bytes;
        println!(
            "          changed_pixels={changed_pixels} dirty_tiles={} {:?} wire_bytes={wire_bytes} preview={}",
            dirty_tiles.len(),
            tile_coords,
            preview
                .as_deref()
                .map(hex_bytes)
                .unwrap_or_else(|| "<none>".to_owned())
        );

        previous.copy_from_slice(&current);
        std::thread::sleep(Duration::from_millis(150));
    }

    println!(
        "summary: rendered_ticks={rendered_ticks} total_dirty_tiles={total_dirty_tiles} total_wire_bytes={total_wire_bytes}"
    );

    let output = std::path::Path::new("target/msu2_dump/slint_frame.bmp");
    std::fs::create_dir_all(output.parent().expect("output parent"))
        .expect("failed to create output directory");
    write_bmp(output, &frame).expect("failed to write BMP");
    println!("frame written to {}", output.display());
    Ok(())
}

fn write_bmp(path: &std::path::Path, frame: &[Rgb565Pixel]) -> std::io::Result<()> {
    let row_stride = (WIDTH * 3).div_ceil(4) * 4;
    let mut bitmap = vec![0u8; row_stride * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let value = frame[y * WIDTH + x].0;
            let r = (((value >> 11) & 0x1f) as u32 * 255 / 31) as u8;
            let g = (((value >> 5) & 0x3f) as u32 * 255 / 63) as u8;
            let b = ((value & 0x1f) as u32 * 255 / 31) as u8;
            let offset = (HEIGHT - 1 - y) * row_stride + x * 3;
            bitmap[offset] = b;
            bitmap[offset + 1] = g;
            bitmap[offset + 2] = r;
        }
    }

    let mut file = std::fs::File::create(path)?;
    use std::io::Write;
    file.write_all(b"BM")?;
    file.write_all(&(54 + bitmap.len() as u32).to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&54u32.to_le_bytes())?;
    file.write_all(&40u32.to_le_bytes())?;
    file.write_all(&(WIDTH as i32).to_le_bytes())?;
    file.write_all(&(HEIGHT as i32).to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&24u16.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&(bitmap.len() as u32).to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&bitmap)?;
    Ok(())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
