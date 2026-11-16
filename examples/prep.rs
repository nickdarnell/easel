//! Preview how an image will be prepared for the TV: `cargo run --example prep -- in.heic out.jpg [fit] [WxH]`
use samsung_frame::image_prep::{PrepOptions, prepare_file};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output, rest @ ..] = args.as_slice() else {
        eprintln!("usage: prep <input> <output.jpg> [auto|cover|contain|blur|original] [WxH]");
        std::process::exit(2);
    };
    let mut opts = PrepOptions::default();
    if let Some(fit) = rest.first() {
        opts.fit = fit.parse()?;
    }
    if let Some((w, h)) = rest.get(1).and_then(|s| s.split_once('x')) {
        (opts.width, opts.height) = (w.parse()?, h.parse()?);
    }
    let start = std::time::Instant::now();
    let p = prepare_file(input.as_ref(), &opts)?;
    std::fs::write(output, &p.data)?;
    println!(
        "{}x{} {} {} KB, reencoded={} cropped={} date={:?} in {:?}",
        p.width,
        p.height,
        p.file_type,
        p.data.len() / 1024,
        p.reencoded,
        p.cropped,
        p.image_date,
        start.elapsed()
    );
    Ok(())
}
