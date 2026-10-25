mod install;

fn main() {
    if let Err(e) = install::install() {
        eprintln!("install-nova-musique: {e}");
        std::process::exit(1);
    }
}
