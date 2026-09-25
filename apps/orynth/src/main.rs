//! Orynth's operator-facing runtime inspector entry point.

fn main() {
    match orynth::run(std::env::args().skip(1)) {
        Ok(Some(output)) => println!("{output}"),
        Ok(None) => {}
        Err(error) => {
            eprintln!("orynth: {error}");
            std::process::exit(2);
        }
    }
}
