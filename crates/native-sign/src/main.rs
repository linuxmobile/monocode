use std::{env, path::Path};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let first = args.next().ok_or("Expected the file to sign")?;
    if first == "--generate" {
        let path = args.next().ok_or("Expected the path of the new key")?;
        let password = env::var("MONOCODE_NATIVE_SIGNING_KEY_PASSWORD").ok();
        let (_, public) = native_sign::generate_keys(Path::new(&path), password.as_deref())?;
        println!("{public}");
        return Ok(());
    }
    native_sign::Signer::from_env()?.sign_file(Path::new(&first))
}
