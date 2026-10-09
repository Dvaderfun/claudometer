//! Standalone std-only fake; compiled by the lifecycle unit test, never contacts a provider.
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|arg| arg == "descendant") {
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
        return;
    }
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.contains("\"method\":\"initialize\"") {
            println!("{{\"id\":1,\"result\":{{}}}}");
        } else if line.contains("\"method\":\"account/login/start\"") {
            println!("{{\"id\":2,\"result\":{{\"type\":\"chatgptAuthTokens\"}}}}");
        } else if line.contains("\"method\":\"account/rateLimits/read\"") {
            let home = std::env::var_os("CODEX_HOME").unwrap();
            let marker = std::path::PathBuf::from(home).join("scenario");
            let scenario = std::fs::read_to_string(marker).unwrap_or_default();
            match scenario.as_str() {
                "refresh" => println!("{{\"id\":4,\"method\":\"account/chatgptAuthTokens/refresh\"}}"),
                "malformed" => println!("not-json"),
                "missing" => println!("{{\"id\":3,\"result\":{{}}}}"),
                "hung" => {
                    let child = Command::new(std::env::current_exe().unwrap())
                        .arg("descendant").stdin(Stdio::piped()).stdout(Stdio::inherit()).spawn().unwrap();
                    println!("{{\"method\":\"fixture/child\",\"params\":{{\"pid\":{}}}}}", child.id());
                    io::stdout().flush().unwrap();
                    // Keep the root and descendant blocked on private stdin; no sleeping.
                    let mut next = String::new();
                    let _ = io::stdin().read_line(&mut next);
                }
                _ => println!("{{\"id\":3,\"result\":{}}}", include_str!("limits.json").replace('\n', "")),
            }
        }
        io::stdout().flush().unwrap();
    }
}
