fn main() {
    if let Err(error) = herdr_beacon::run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
