#[allow(dead_code)]
#[path = "../img_retro.rs"]
mod img_retro;

use std::{
    env,
    error::Error,
    fs,
    path::PathBuf,
};

use img_retro::render_retro_bw_image;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);

    let input_path = args.next().map(PathBuf::from).ok_or(
        "Użycie: cargo run --bin retro_test -- <obraz> [plik-wynikowy.txt]",
    )?;

    let output_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wynik.txt"));

    let image_bytes = fs::read(&input_path)?;
    let label = input_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image");

    let (description, lines) = render_retro_bw_image(
        label,
        "",
        Some(image_bytes.len() as u64),
        &image_bytes,
        80,
        40,
    )
    .ok_or(
        "Nie udało się przekonwertować obrazu: sprawdź format i upewnij się, że plik nie przekracza 1 MB",
    )?;

    let mut output = String::new();
    output.push_str(&description);
    output.push('\n');
    output.push_str(&lines.join("\n"));
    output.push('\n');

    fs::write(&output_path, output)?;

    println!("Zapisano {}", output_path.display());
    Ok(())
}
