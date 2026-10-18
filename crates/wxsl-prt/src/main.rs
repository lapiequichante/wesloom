//! Bake a glTF model's precomputed radiance transfer into sidecar files.
//!
//! ```text
//! cargo run -p wxsl-prt --release -- resources/glb/WXSLCar/wxslCar.glb
//! cargo run -p wxsl-prt --release -- model.glb --bounces 2 --rays 512 --floor
//! cargo run -p wxsl-prt --release -- model.glb --transfer color
//! ```
//!
//! By default both modes are baked from the same rays and written beside
//! the model, as `<model>.gray.prt` and `<model>.color.prt`, which is
//! where the `viewer` example looks for them.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use wxsl_prt::{Albedo, BakeOptions, Transfer};

const USAGE: &str = "wxsl-prt <model.glb> [--transfer gray|color|both] [--bounces N] [--rays N]
         [--floor] [--threads N] [--out-dir DIR]
  --transfer   which modes to write (default both): <model>.gray.prt, <model>.color.prt
  --bounces N  diffuse bounces after the shadowed light (default 1, 0 = shadows only)
  --rays N     rays per vertex (default 256)
  --floor      an infinite floor under the model blocks the lower hemisphere
  --threads N  worker threads (default: every core)
  --out-dir    where to write (default: beside the model)";

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

/// Where the `transfer` bake of `model` goes.
fn sidecar(model: &Path, directory: Option<&Path>, transfer: Transfer) -> PathBuf {
    let stem = model.file_stem().unwrap_or_default().to_string_lossy();
    let name = format!("{stem}.{}.prt", transfer.name());
    match directory {
        Some(directory) => directory.join(name),
        None => model.with_file_name(name),
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut model = None;
    let mut directory = None;
    let mut options = BakeOptions::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--out-dir" => directory = Some(PathBuf::from(value()?)),
            "--rays" => options.rays = value()?.parse()?,
            "--bounces" => options.bounces = value()?.parse()?,
            "--threads" => options.threads = value()?.parse()?,
            "--floor" => options.floor = true,
            "--transfer" => {
                options.transfers = match value()?.as_str() {
                    "gray" => vec![Transfer::Gray],
                    "color" => vec![Transfer::Color],
                    "both" => Transfer::ALL.to_vec(),
                    other => return Err(format!("unknown transfer `{other}`\n{USAGE}").into()),
                }
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag `{other}`\n{USAGE}").into())
            }
            path => model = Some(PathBuf::from(path)),
        }
    }
    let model = model.ok_or(USAGE)?;

    let started = Instant::now();
    let imported = wxsl_gltf::import(&model, &wxsl_stdlib::registry())?;
    let albedo = Albedo::load(&model, &imported)?;
    let vertices: usize = imported.meshes.iter().map(|mesh| mesh.vertices.len()).sum();
    let triangles: usize = imported
        .meshes
        .iter()
        .map(|mesh| mesh.indices.len() / 3)
        .sum();
    println!(
        "{}: {} meshes, {vertices} vertices, {triangles} triangles ({:.1?})",
        model.display(),
        imported.meshes.len(),
        started.elapsed()
    );
    let baking = Instant::now();
    let bakes = wxsl_prt::bake(&imported, &albedo, &options)?;
    let seconds = baking.elapsed().as_secs_f64();
    let rays = vertices as f64 * f64::from(options.rays) * f64::from(1 + options.bounces);
    println!(
        "baked {} rays per vertex, {} bounce(s), in {seconds:.1}s ({:.1} M rays/s)",
        options.rays,
        options.bounces,
        rays / seconds / 1e6
    );
    for bake in bakes {
        let path = sidecar(&model, directory.as_deref(), bake.transfer);
        bake.write(std::io::BufWriter::new(std::fs::File::create(&path)?))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
