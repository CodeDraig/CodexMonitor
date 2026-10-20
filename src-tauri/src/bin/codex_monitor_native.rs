use std::path::PathBuf;

fn usage() -> &'static str {
    "USAGE: codex-monitor-native [--data-dir <path>] [--help]\n\nThe default data directory matches the existing CodexMonitor desktop application."
}

fn parse_args() -> Result<Option<PathBuf>, String> {
    let mut data_dir = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--data-dir" => {
                let path = args.next().ok_or("--data-dir requires a path")?;
                if path.trim().is_empty() {
                    return Err("--data-dir requires a non-empty path".into());
                }
                data_dir = Some(PathBuf::from(path));
            }
            _ => return Err(format!("unknown argument: {arg}\n\n{}", usage())),
        }
    }
    Ok(data_dir)
}

fn main() {
    let data_dir = match parse_args() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = codex_monitor_lib::run_native(data_dir) {
        eprintln!("failed to start native CodexMonitor: {error}");
        std::process::exit(1);
    }
}
