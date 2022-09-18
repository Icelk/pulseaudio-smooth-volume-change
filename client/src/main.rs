use std::env;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process;

fn main() {
    let mut args = env::args();
    args.next();
    let v = args.next();
    if v.as_deref() == Some("--help") {
        eprintln!(
            "\
pasv 0.1.0
Icelk <main@icelk.dev>

USAGE:
    pasv <VOLUME> [PATH]

VOLUME:
    A decimal value representing the volume.
    If prepended by a `+` or `-`, the volume change is relative.

PATH:
    An optional path to the pasvd socket. Default to the user's run directory.
\
        "
        );
        process::exit(1);
    }
    let v = if let Some(v) = v {
        v
    } else {
        eprintln!(
            "Specify the volume or volume change (-10% should be written as -0.1) as the first argument."
        );
        process::exit(1);
    };
    let path = args
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(socket_path);
    let s = UnixStream::connect(&path);
    let mut s = match s {
        Ok(s) => s,
        Err(err) => {
            eprintln!("pasvd is maybe not running.\nFailed to connect to {}: {err}", path.display());
            process::exit(1);
        }
    };
    s.write_all(v.as_bytes()).unwrap();
}
fn socket_path() -> std::path::PathBuf {
    let mut p = Path::new("/run").to_path_buf();
    let user: u32 = unsafe { libc::getuid() };
    if user != 0 {
        p.push("user");
        p.push(user.to_string());
    }
    p.push("pasvd");
    p
}
