fn main() {
    if let Err(error) = openhuman_tui::run_from_cli(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
