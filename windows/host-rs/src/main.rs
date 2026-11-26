#![cfg_attr(windows, windows_subsystem = "windows")]

use std::env;
use std::io;
use std::process::{self, ExitCode};
use std::thread;

#[cfg(windows)]
mod diagnostics;
#[cfg(windows)]
mod sign_in;
mod studio;
mod studio_files;
#[cfg(windows)]
mod update;

fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    #[cfg(windows)]
    if let [mode, rest @ ..] = args.as_slice() {
        if mode == "diagnostics" {
            return diagnostics::run(rest).map_err(Into::into);
        }
        if mode == "sign-in" {
            return sign_in::run(rest).map_err(Into::into);
        }
        if mode == "verify-update" {
            return update::run(rest).map_err(Into::into);
        }
    }
    let linked = match args.as_slice() {
        [mode] if mode == "studio" => false,
        [mode, link] if mode == "studio" && link == "--parent-stdin" => true,
        _ => {
            return Err(
                "Usage: Lummix Host.exe studio [--parent-stdin] | diagnostics | sign-in".into(),
            )
        }
    };
    let executable = env::current_exe()?;
    let root = executable.with_file_name("studio");
    let address = if env::var("LUMMIX_QA_LOOPBACK").as_deref() == Ok("2") {
        "127.0.0.2:41835"
    } else {
        studio::HOST
    };
    let server = tiny_http::Server::http(address)?;
    if linked {
        thread::Builder::new()
            .name("lummix-parent-stdin".into())
            .spawn(|| {
                let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
                // The studio has no mutable state. Don't let a half-open HTTP
                // connection keep the process or its listener alive.
                process::exit(0);
            })?;
    }
    studio::serve(server, &root)?;
    Ok(())
}

fn main() -> ExitCode {
    if let Err(error) = run() {
        use io::Write;
        let _ = writeln!(io::stderr().lock(), "Lummix host: {error}");
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
        {
            return ExitCode::from(5);
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
