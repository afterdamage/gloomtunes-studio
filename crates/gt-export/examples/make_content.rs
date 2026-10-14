//! Writes the factory content (Prompt 28) into a folder: the sound packs, and the demo songs
//! as `.gloom` projects under `demos/`.
//! `cargo run --release -p gt-export --example make_content -- <folder>`.

use std::path::Path;

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "factory".into());
    let dir = Path::new(&dir);
    match gt_export::packs::ensure_factory_packs(dir) {
        Ok(written) => {
            for p in written {
                let size = std::fs::metadata(&p).map_or(0, |m| m.len());
                println!("{} ({} KB)", p.display(), size / 1024);
            }
        }
        Err(e) => {
            eprintln!("packs: {e}");
            std::process::exit(1);
        }
    }
    let demos = dir.join("demos");
    if let Err(e) = std::fs::create_dir_all(&demos) {
        eprintln!("{}: {e}", demos.display());
        std::process::exit(1);
    }
    for demo in &gt_core::demos::DEMOS {
        let path = demos.join(format!("{}.{}", demo.name, gt_project::file::EXTENSION));
        let opts = gt_project::file::SaveOptions::default();
        match gt_project::file::save(&(demo.build)(), &path, opts) {
            Ok(_) => println!("{}", path.display()),
            Err(e) => eprintln!("{}: {e}", path.display()),
        }
    }
}
