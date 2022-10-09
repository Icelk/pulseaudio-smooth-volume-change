use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process;

fn print_help() -> ! {
    eprintln!(
        "\
Usage: pasv [OPTIONS] <VOLUME> [PATH]

Volume:
    A decimal or percent value representing the volume.
    If prepended by a `+` or `-`, the volume change is relative.

Path:
    An optional path to the pasvd socket. Default to the user's run directory.\
\
Options:
    -g, --get-volume        Get the volume of the default sink. Returns the value in percents.
        "
    );
    process::exit(1);
}
fn arg_invalid_exit(s: &str) -> ! {
    eprintln!("{s}\nSee --help for usage information.");
    process::exit(1);
}

fn main() {
    let mut args = env::args();
    args.next();

    let mut path = None;
    let mut volume = None;
    let mut get_volume = false;

    for arg in args {
        match arg.as_str() {
            "--help" => print_help(),
            "--get-volume" | "-g" => get_volume = true,
            _ if volume.is_some() && path.is_some() => {
                arg_invalid_exit("Only two arguments are valid.")
            }
            _ if volume.is_some() => path = Some(arg),
            _ => volume = Some(arg),
        }
    }

    let v = if let Some(v) = volume {
        v
    } else if get_volume {
        String::new()
    } else {
        arg_invalid_exit(
            "Specify the volume or volume change (both -10% and -0.1 are valid \
            and mean the same thing) as the first argument.",
        );
    };
    let path = path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(socket_path);
    let s = UnixStream::connect(&path);
    let mut s = match s {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "pasvd is maybe not running.\nFailed to connect to {}: {err}",
                path.display()
            );
            process::exit(1);
        }
    };

    let data = if get_volume {
        b"get-volume"
    } else {
        v.as_bytes()
    };
    s.write_all(data).unwrap();
    s.flush().unwrap();
    s.shutdown(std::net::Shutdown::Write).unwrap();
    if get_volume {
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        if buf.is_empty() {
            eprintln!("Failed to query volume");
            process::exit(1);
        } else {
            std::io::stdout().write_all(&buf).unwrap();
            std::io::stdout().write_all(b"\n").unwrap();
        }
    }
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
