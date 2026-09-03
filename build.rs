fn main() {
    println!("cargo:rerun-if-env-changed=CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX");
    println!("cargo:rerun-if-env-changed=CLAUDOMETER_RELEASE_SEQUENCE");
    let release_key = std::env::var("CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX").ok();
    let release_sequence = std::env::var("CLAUDOMETER_RELEASE_SEQUENCE").ok();
    match (&release_key, &release_sequence) {
        (Some(key), Some(sequence)) => {
            let valid_key = key.len() == 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            let valid_sequence = sequence.parse::<u64>().is_ok_and(|value| value > 0);
            if !valid_key || !valid_sequence {
                panic!("release trust root must be 64 lowercase hex characters and sequence must be positive");
            }
        }
        (None, None) => {
            println!("cargo:warning=release updater is fail-closed: trust root not provisioned");
        }
        _ => panic!(
            "CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX and CLAUDOMETER_RELEASE_SEQUENCE must be set together"
        ),
    }

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set(
            "FileDescription",
            "Claudometer — Claude usage limits in the tray",
        );
        res.set("ProductName", "Claudometer");
        res.set("LegalCopyright", "MIT License");
        if let Err(e) = res.compile() {
            println!("cargo:warning=resource compile failed: {e}");
        }
    }
}
