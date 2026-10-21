//! Bake a glTF model's precomputed radiance transfer into sidecar files.
//!
//! ```text
//! cargo run -p wxsl-prt --release -- resources/glb/WXSLCar/wxslCar.glb
//! cargo run -p wxsl-prt --release -- model.glb --bounces 2 --rays 512 --floor
//! cargo run -p wxsl-prt --release -- model.glb --transfer color
//! cargo run -p wxsl-prt --release -- model.glb --albedo
//! cargo run -p wxsl-prt --release -- model.glb --clip 1
//! cargo run -p wxsl-prt --release -- model.glb --ground --bounces 0
//! ```
//!
//! By default both modes are baked from the same rays and written beside
//! the model, as `<model>.gray.prt` and `<model>.color.prt`, which is
//! where the `viewer` example looks for them. With `--ground`, the model
//! is left alone: a grid under it is written as `<model>_ground.glb` and
//! that grid is baked against it, as `<model>_ground.gray.prt` and so on.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use wxsl_prt::{Albedo, BakeOptions, GroundGrid, Transfer};

const USAGE: &str = "wxsl-prt <model.glb> [--transfer gray|color|both] [--bounces N] [--rays N]
         [--clip N | --rest] [--floor] [--albedo] [--threads N] [--out-dir DIR]
         [--ground [--ground-spacing M] [--ground-margin M]]
  --transfer   which modes to write (default both): <model>.gray.prt, <model>.color.prt
  --bounces N  diffuse bounces after the shadowed light (default 3, 0 = shadows only)
  --clip N     pose the model at the first frame of clip N (default 0, the first)
  --rest       bake the pose the file was saved in, ignoring its clips
  --rays N     rays per vertex (default 256)
  --floor      an infinite floor under the model blocks the lower hemisphere
  --albedo     bounces take the base colour alone, ignoring metallic and specular
  --threads N  worker threads (default: every core)
  --out-dir    where to write (default: beside the model)
  --ground     bake a grid under the model instead of the model: <model>_ground.glb and
               its <model>_ground.<mode>.prt, the model's shadow on what it stands on
  --ground-spacing M  the grid's vertex spacing (default 0.025 m)
  --ground-margin M   how far it reaches past the model (default 1.5 m)";

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
    let mut base_color_only = false;
    let mut ground: Option<GroundGrid> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--out-dir" => directory = Some(PathBuf::from(value()?)),
            "--rays" => options.rays = value()?.parse()?,
            "--bounces" => options.bounces = value()?.parse()?,
            "--threads" => options.threads = value()?.parse()?,
            "--clip" => options.clip = Some(value()?.parse()?),
            "--rest" => options.clip = None,
            "--floor" => options.floor = true,
            "--albedo" => base_color_only = true,
            "--ground" => {
                ground.get_or_insert_with(GroundGrid::default);
            }
            "--ground-spacing" => {
                ground.get_or_insert_with(GroundGrid::default).spacing = value()?.parse()?
            }
            "--ground-margin" => {
                ground.get_or_insert_with(GroundGrid::default).margin = value()?.parse()?
            }
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
    let mut albedo = Albedo::load(&model, &imported)?;
    if base_color_only {
        albedo = albedo.base_color_only();
    }
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
    match options.clip.and_then(|clip| imported.animations.get(clip)) {
        Some(animation) => println!("posed at the first frame of clip `{}`", animation.name),
        None if !imported.animations.is_empty() => println!("posed as the file was saved"),
        None => {}
    }
    // The ground, when there is one, is what gets baked and written.
    let mut baked = model.clone();
    let mut receiver = None;
    if let Some(grid) = &ground {
        let stem = model.file_stem().unwrap_or_default().to_string_lossy();
        let name = format!("{stem}_ground.glb");
        baked = match directory.as_deref() {
            Some(directory) => directory.join(name),
            None => model.with_file_name(name),
        };
        std::fs::write(&baked, wxsl_prt::ground(&imported, &options, grid)?)?;
        let grid = wxsl_gltf::import(&baked, &wxsl_stdlib::registry())?;
        let count: usize = grid.meshes.iter().map(|mesh| mesh.vertices.len()).sum();
        println!("wrote {}: {count} vertices", baked.display());
        receiver = Some((grid, count));
    }
    let baking = Instant::now();
    let bakes = match &receiver {
        Some((grid, _)) => wxsl_prt::bake_receiver(&imported, &albedo, grid, &options)?,
        None => wxsl_prt::bake(&imported, &albedo, &options)?,
    };
    let seconds = baking.elapsed().as_secs_f64();
    // The model is traced too only for its bounces.
    let traced = match &receiver {
        Some((_, count)) if options.bounces == 0 => *count,
        Some((_, count)) => vertices + count,
        None => vertices,
    };
    let rays = traced as f64 * f64::from(options.rays) * f64::from(1 + options.bounces);
    println!(
        "baked {} rays per vertex, {} bounce(s), in {seconds:.1}s ({:.1} M rays/s)",
        options.rays,
        options.bounces,
        rays / seconds / 1e6
    );
    for bake in bakes {
        let path = sidecar(&baked, directory.as_deref(), bake.transfer);
        bake.write(std::io::BufWriter::new(std::fs::File::create(&path)?))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
