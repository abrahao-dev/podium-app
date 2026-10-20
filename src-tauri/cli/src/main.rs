//! `podium`: thin relay CLI used by agents running inside Podium terminals.
//! All command parsing happens in the app; this binary only forwards argv,
//! cwd and (optionally) stdin over a local TCP connection.

use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Value};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

struct Env {
    port: u16,
    token: String,
}

struct Response {
    stdout: String,
    stderr: String,
    exit: i32,
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    if argv.first().map(String::as_str) == Some("debug") {
        return debug();
    }

    let Some(env) = read_env() else {
        return missing_env();
    };

    let wants_stdin =
        argv.first().map(String::as_str) == Some("hook") || argv.iter().any(|a| a == "--stdin");
    let stdin = if wants_stdin { read_stdin() } else { None };

    match round_trip(&env, &argv, stdin) {
        Ok(resp) => {
            emit(&resp);
            exit_code(resp.exit)
        }
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::from(4)
        }
    }
}

fn read_env() -> Option<Env> {
    let port = std::env::var("PODIUM_PORT").ok()?.trim().parse::<u16>().ok()?;
    let token = std::env::var("PODIUM_TOKEN").ok()?;
    if token.is_empty() {
        return None;
    }
    Some(Env { port, token })
}

fn missing_env() -> ExitCode {
    eprintln!("podium: not running inside a Podium terminal (PODIUM_PORT/PODIUM_TOKEN missing)");
    ExitCode::from(3)
}

fn read_stdin() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = Vec::new();
    stdin.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn round_trip(env: &Env, argv: &[String], stdin: Option<String>) -> Result<Response, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], env.port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| {
        format!(
            "podium: cannot reach Podium on port {} ({e}). Is the app still running?",
            env.port
        )
    })?;

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let request = json!({
        "v": 1,
        "token": env.token,
        "argv": argv,
        "cwd": cwd,
        "stdin": stdin,
    });
    let mut line = request.to_string();
    line.push('\n');

    let closed = || "podium: connection closed by Podium".to_string();
    stream.write_all(line.as_bytes()).map_err(|_| closed())?;
    stream.flush().map_err(|_| closed())?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    let read = reader.read_line(&mut response).map_err(|_| closed())?;
    if read == 0 {
        return Err(closed());
    }
    parse_response(&response).ok_or_else(closed)
}

fn parse_response(line: &str) -> Option<Response> {
    let value: Value = serde_json::from_str(line.trim_end()).ok()?;
    let exit = value.get("exit")?.as_i64()?;
    Some(Response {
        stdout: value.get("stdout")?.as_str()?.to_owned(),
        stderr: value.get("stderr")?.as_str()?.to_owned(),
        exit: i32::try_from(exit).unwrap_or(1),
    })
}

fn emit(resp: &Response) {
    if !resp.stdout.is_empty() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(resp.stdout.as_bytes());
        let _ = out.flush();
    }
    if !resp.stderr.is_empty() {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(resp.stderr.as_bytes());
        let _ = err.flush();
    }
}

fn exit_code(code: i32) -> ExitCode {
    // Exit codes are truncated to 8 bits on Unix; mirror that on every platform.
    ExitCode::from((code & 0xff) as u8)
}

fn presence(name: &str) -> &'static str {
    match std::env::var_os(name) {
        Some(v) if !v.is_empty() => "set",
        _ => "missing",
    }
}

fn debug() -> ExitCode {
    println!("PODIUM_PORT: {}", presence("PODIUM_PORT"));
    println!("PODIUM_TOKEN: {}", presence("PODIUM_TOKEN"));
    println!(
        "PODIUM_TERMINAL_ID: {}",
        std::env::var("PODIUM_TERMINAL_ID").unwrap_or_else(|_| "missing".to_string())
    );
    match std::env::current_exe() {
        Ok(path) => println!("exe: {}", path.display()),
        Err(e) => println!("exe: unknown ({e})"),
    }
    let _ = std::io::stdout().flush();

    let Some(env) = read_env() else {
        return missing_env();
    };

    match round_trip(&env, &["ping".to_string()], None) {
        Ok(resp) if resp.exit == 0 => {
            print!("ping: {}", resp.stdout);
            if !resp.stdout.ends_with('\n') {
                println!();
            }
            let _ = std::io::stdout().flush();
            ExitCode::SUCCESS
        }
        Ok(resp) => {
            println!(
                "ping failed (exit {}): {}{}",
                resp.exit, resp.stdout, resp.stderr
            );
            ExitCode::from(4)
        }
        Err(msg) => {
            println!("ping: {msg}");
            ExitCode::from(4)
        }
    }
}
