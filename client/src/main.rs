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
    -g, --get-volume                Get the volume of the default sink. Returns the value in percents.
    -d, --duration [MILLISECONDS]   Specifies the duration to smoothly change volume.
        "
    );
    process::exit(1);
}
fn arg_invalid_exit(s: impl AsRef<str>) -> ! {
    eprintln!("{}\nSee --help for usage information.", s.as_ref());
    process::exit(1);
}

fn main() {
    let mut args = env::args();
    args.next();

    let mut volume = None;
    let mut path = None;
    let mut get_volume = false;
    let mut next_is_duration = false;
    let mut duration = None;

    for arg in args {
        if next_is_duration {
            duration = Some(arg);
            next_is_duration = false;
            continue;
        }
        match arg.as_str() {
            "--help" => print_help(),
            "--get-volume" | "-g" => {
                if path.is_some() {
                    arg_invalid_exit("Only one argument is valid.")
                } else {
                    get_volume = true
                }
            }
            "--duration" | "-d" => next_is_duration = true,
            _ if arg.starts_with('-')
                // and not a number (negative numbers)
                && arg
                    .strip_prefix('-')
                    .unwrap()
                    .chars()
                    .next()
                    .map_or(true, |c| !c.is_numeric() && c != '_' && c != '.') =>
            {
                arg_invalid_exit(format!("Unrecognised argument: {arg}."))
            }
            _ if volume.is_some() && get_volume => arg_invalid_exit("Only one argument is valid."),
            _ if volume.is_some() && path.is_some() => {
                arg_invalid_exit("Only two arguments are valid.")
            }
            _ if volume.is_some() => path = Some(arg),
            _ => volume = Some(arg),
        }
    }
    if next_is_duration {
        arg_invalid_exit("--duration takes a value");
    }

    let path = (if get_volume { &volume } else { &path })
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(socket_path);
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

    if get_volume {
        s.write_all(b"get-volume").unwrap();
        s.flush().unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        if buf.is_empty() {
            eprintln!("Failed to query volume");
            process::exit(1);
        } else {
            std::io::stdout().write_all(&buf).unwrap();
            std::io::stdout().write_all(b"\n").unwrap();
        }
    } else {
        s.write_all(v.as_bytes()).unwrap();
        if let Some(duration) = duration {
            s.write_all(b" ").unwrap();
            s.write_all(duration.as_bytes()).unwrap();
        }
    };
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
