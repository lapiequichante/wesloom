//! `--screenshot`: one PNG per demo and a contact sheet.

use super::*;

/// One rendered demo, on its way to a PNG and the contact sheet.
pub(crate) struct Shot {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
}

pub(crate) fn run_screenshots(
    size: Option<(u32, u32)>,
    demos: &[Demo],
    dir: &Path,
    ibl_source: &str,
    refraction_quality: RefractionQuality,
) -> Result<(), Box<dyn Error>> {
    let (width, height) = size.unwrap_or((800, 600));
    let gpu = pollster::block_on(GpuContext::headless())?;
    println!("adapter: {}", gpu.adapter.get_info().name);
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let mut stage = Stage::new(
        gpu,
        TargetConfig::new(width, height, target.format()),
        ibl_source,
    )?;
    stage.refraction_quality = refraction_quality;

    std::fs::create_dir_all(dir)?;
    let mut shots: Vec<Shot> = Vec::new();
    for demo in demos {
        // A fixed time, so two runs produce identical images. A temporal
        // pipeline's demo renders its warmup first — the same step, one
        // frame at a time — so the captured frame shows a settled
        // history instead of frame one's empty ring.
        if let Some(motion) = demo.motion {
            for frame in 0..demo.warmup {
                stage.render(
                    demo,
                    target.view(),
                    width,
                    height,
                    0.6 - (demo.warmup - frame) as f32 * motion.step,
                    motion.step,
                )?;
            }
        }
        let step = demo.motion.map_or(0.0, |motion| motion.step);
        stage.render(demo, target.view(), width, height, 0.6, step)?;
        stage.gpu.wait();
        let pixels = target.read_rgba8(&stage.gpu.device, &stage.gpu.queue);
        let file = dir.join(format!("{}.png", demo.name));
        image::save_buffer(
            &file,
            &pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
        println!(
            "{:>16}: {} covered pixels, mean luminance {:.4} -> {}",
            demo.name,
            covered_pixels(&pixels),
            mean_luminance(&pixels),
            file.display()
        );
        shots.push(Shot {
            width,
            height,
            pixels,
        });
    }

    let sheet = dir.join("gallery.png");
    write_contact_sheet(&shots, &sheet)?;
    println!(
        "\ncontact sheet -> {} ({} demos)",
        sheet.display(),
        shots.len()
    );
    Ok(())
}

/// Tile every shot into one image, dark gutters between — the "see their
/// result" file, reviewable at a glance.
pub(crate) fn write_contact_sheet(shots: &[Shot], path: &Path) -> Result<(), Box<dyn Error>> {
    const GUTTER: u32 = 16;
    let cell_w = shots.iter().map(|shot| shot.width).max().unwrap_or(1);
    let cell_h = shots.iter().map(|shot| shot.height).max().unwrap_or(1);
    let cols = ((shots.len() as f32).sqrt().ceil() as u32).max(1);
    let rows = shots.len() as u32 / cols + u32::from(shots.len() as u32 % cols != 0);
    let sheet_w = cols * cell_w + (cols + 1) * GUTTER;
    let sheet_h = rows * cell_h + (rows + 1) * GUTTER;
    let mut sheet = vec![22u8; (sheet_w * sheet_h * 4) as usize];
    for index in (0..sheet.len()).step_by(4) {
        sheet[index + 3] = 255;
    }
    for (position, shot) in shots.iter().enumerate() {
        let position = position as u32;
        let x0 = GUTTER + (position % cols) * (cell_w + GUTTER);
        let y0 = GUTTER + (position / cols) * (cell_h + GUTTER);
        for y in 0..shot.height {
            let from = (y * shot.width * 4) as usize;
            let to = (((y0 + y) * sheet_w + x0) * 4) as usize;
            let row = (shot.width * 4) as usize;
            sheet[to..to + row].copy_from_slice(&shot.pixels[from..from + row]);
        }
    }
    image::save_buffer(
        path,
        &sheet,
        sheet_w,
        sheet_h,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(())
}

pub(crate) fn covered_pixels(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|texel| texel[0] > 12 || texel[1] > 12 || texel[2] > 12)
        .count()
}

pub(crate) fn mean_luminance(pixels: &[u8]) -> f32 {
    let sum: f32 = pixels
        .chunks_exact(4)
        .map(|texel| {
            (0.2126 * texel[0] as f32 + 0.7152 * texel[1] as f32 + 0.0722 * texel[2] as f32) / 255.0
        })
        .sum();
    sum / (pixels.len() / 4) as f32
}
