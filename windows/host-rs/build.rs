use std::{env, fs, path::PathBuf, process::Command};

fn resource_compiler() -> PathBuf {
    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return PathBuf::from("rc.exe");
    }
    let kits = PathBuf::from(env::var_os("ProgramFiles(x86)").expect("Windows SDK location"))
        .join("Windows Kits/10/bin");
    fs::read_dir(kits)
        .expect("Install the Windows 10 or 11 SDK")
        .flatten()
        .filter_map(|entry| {
            let version = entry
                .file_name()
                .to_str()?
                .split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            let path = entry.path().join("x64/rc.exe");
            path.is_file().then_some((version, path))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .expect("Windows SDK x64 resource compiler")
        .1
}

fn main() {
    println!("cargo:rerun-if-changed=../release_identity.py");
    println!("cargo:rerun-if-changed=../packaging/Lummix.rc");
    println!("cargo:rerun-if-changed=../packaging/Lummix.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let identity = include_str!("../release_identity.py");
    let version = identity
        .lines()
        .find_map(|line| line.strip_prefix("VERSION = \"")?.strip_suffix('"'))
        .expect("Missing Lummix release version");
    let parts: Vec<u16> = version
        .split('.')
        .map(|p| p.parse().expect("Numeric release version"))
        .collect();
    assert_eq!(parts.len(), 3);
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let resource = out.join("lummix.res");
    let source = out.join("lummix.rc");
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../packaging/Lummix.ico");
    let rc = include_str!("../packaging/Lummix.rc")
        .replace("@VERSION@", version)
        .replace(
            "@NUMERIC@",
            &format!("{},{},{},0", parts[0], parts[1], parts[2]),
        )
        .replace("@ICON@", &icon.to_string_lossy().replace('\\', "/"));
    fs::write(&source, rc).unwrap();
    let status = Command::new(resource_compiler())
        .args(["/nologo", "/c65001", "/fo"])
        .arg(&resource)
        .arg(&source)
        .status()
        .expect("Run the Windows resource compiler");
    assert!(
        status.success(),
        "Could not build Lummix version information"
    );
    println!(
        "cargo:rustc-link-arg-bin=lummix-host-cli={}",
        resource.display()
    );
}
