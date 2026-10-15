use std::{error::Error, fs, io::Write, path::Path};

const MAX_FRAME: usize = 4 * 1024 * 1024;

fn frame(raw: &mut Vec<u8>, pack: &mut Vec<u8>, frames: &mut Vec<[usize; 2]>) -> Result<(), Box<dyn Error>> {
    let compressed = zstd::bulk::compress(raw, 19)?;
    frames.push([pack.len(), compressed.len()]);
    pack.extend(compressed);
    raw.clear();
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: sticker-pack PLAN.json ART.pack INDEX.json".into());
    }
    let paths: Vec<String> = serde_json::from_slice(&fs::read(&args[0])?)?;
    let options = usvg::Options::default();
    let writer = usvg::WriteOptions {
        coordinates_precision: 2,
        transforms_precision: 4,
        indent: usvg::Indent::None,
        attributes_indent: usvg::Indent::None,
        ..Default::default()
    };
    let mut raw = Vec::new();
    let mut pack = Vec::new();
    let mut frames = Vec::new();
    let mut art = Vec::new();
    for path in &paths {
        let tree = usvg::Tree::from_data(&fs::read(path)?, &options).map_err(|e| format!("{path}: {e}"))?;
        let svg = tree.to_string(&writer);
        if svg.len() > MAX_FRAME {
            return Err(format!("{path}: SVG larger than 4 MiB").into());
        }
        if raw.len() + svg.len() > MAX_FRAME {
            frame(&mut raw, &mut pack, &mut frames)?;
        }
        art.push([frames.len(), raw.len(), svg.len()]);
        raw.write_all(svg.as_bytes())?;
    }
    if !raw.is_empty() {
        frame(&mut raw, &mut pack, &mut frames)?;
    }
    // Check the actual compressed bytes and leave whole SVGs for the independent XML check.
    let minified = Path::new(&args[2]).parent().unwrap().join("minified");
    fs::create_dir_all(&minified)?;
    let mut checked = 0;
    for (number, [offset, len]) in frames.iter().copied().enumerate() {
        let decoded = zstd::bulk::decompress(&pack[offset..offset + len], MAX_FRAME)?;
        let mut end = 0;
        for (i, [frame, start, len]) in art.iter().copied().enumerate() {
            if frame != number {
                continue;
            }
            if start != end || start + len > decoded.len() {
                return Err("invalid SVG range".into());
            }
            let svg = &decoded[start..start + len];
            usvg::Tree::from_data(svg, &options)?;
            fs::write(minified.join(Path::new(&paths[i]).file_name().unwrap()), svg)?;
            end = start + len;
            checked += 1;
        }
        if end != decoded.len() {
            return Err("unreferenced frame bytes".into());
        }
    }
    fs::write(&args[1], pack)?;
    fs::write(&args[2], serde_json::to_vec(&serde_json::json!({"frames": frames, "art": art}))?)?;
    println!("Checked {checked} SVG ranges in {} independent frames (raw <= 4 MiB)", frames.len());
    Ok(())
}
