use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{fs, process, thread};

use clap::Arg;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::proplist::Proplist;
use libpulse_binding::volume::{ChannelVolumes, Volume};

#[derive(Debug, Clone, Copy)]
enum ChangeVolume {
    Increase(f64),
    Absolute(f64),
}
impl ChangeVolume {
    fn collapse(self, absolute_volume: f64) -> f64 {
        match self {
            ChangeVolume::Increase(i) => absolute_volume + i,
            ChangeVolume::Absolute(a) => a,
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Change {
        volume: ChangeVolume,
        duration: Option<f64>,
    },
    GetVolume(mpsc::SyncSender<Option<f64>>),
}

fn command() -> clap::Command<'static> {
    let cmd: clap::Command<'static> = clap::command!();
    cmd.arg(
        Arg::new("duration")
            .long("duration")
            .short('d')
            .default_value("150")
            .help("Duration of volume change in milliseconds.")
            .value_parser(clap::value_parser!(u64)),
    )
    .arg(
        Arg::new("interval")
            .long("interval")
            .short('i')
            .default_value("10")
            .help("Interval between volume changes in milliseconds.")
            .value_parser(clap::value_parser!(u64)),
    )
    .arg(
        Arg::new("path")
            .long("path")
            .short('p')
            .help(
                "Path to bind the command receiving UNIX socket to. \
                Defaults to `/run/user/<uid>/pasvd`.",
            )
            .takes_value(true),
    )
    .arg(
        Arg::new("no-clamp")
            .long("no-clamp")
            .help("Don't clamp volume range to 100%."),
    )
    .arg(
        Arg::new("verbose")
            .long("verbose")
            .help("Print more information of the Pulseaudio operations."),
    )
    .arg(Arg::new("print-timings").long("print-timings").help(
        "Print timing information. Useful \
        for performance debugging regarding slow Pulseaudio callbacks.",
    ))
}

fn main() {
    let cmd = command();
    let matches = cmd.get_matches();
    let duration = Duration::from_millis(*matches.get_one("duration").unwrap());
    let interval = Duration::from_millis(*matches.get_one("interval").unwrap());
    let path = matches
        .get_one::<&str>("path")
        .map_or_else(socket_path, std::path::PathBuf::from);
    let clamp = !matches.contains_id("no-clamp");
    let verbose = matches.contains_id("verbose");
    let print_timings = matches.contains_id("print-timings");

    let mut ml = libpulse_binding::mainloop::threaded::Mainloop::new()
        .expect("failed to create a libpulse Mainloop");

    let mut props = Proplist::new().unwrap();
    props
        .set_str(
            libpulse_binding::proplist::properties::APPLICATION_NAME,
            "pa-smooth-volume",
        )
        .unwrap();

    let mut ctx = Context::new_with_proplist(&ml, "pa-smooth-volume", &props)
        .expect("failed to create a libpulse Context");
    let (tx, rx) = mpsc::channel();

    ctx.set_state_callback(Some(Box::new(move || {
        tx.send(()).unwrap();
    })));

    ctx.connect(None, FlagSet::NOFLAGS, None)
        .expect("failed to connect to PA");

    ml.start().unwrap();

    loop {
        // wait for connection
        rx.recv().unwrap();
        println!("State change: {:?}", ctx.get_state());
        if let State::Ready = ctx.get_state() {
            break;
        }
    }
    println!("Connected");

    let mut volume = None;
    let mut initial_volume = None;
    let mut step = None;
    let mut iterations = 0_u32;

    let mut sink = get_default_sink(&ctx);
    println!("Got sink.");
    let mut channels = sink.as_ref().and_then(|sink| get_channels(sink, &ctx));
    let mut sink_last_changed = Instant::now();

    let (change_volume, rx_change_volume) = mpsc::channel();

    {
        thread::spawn(move || {
            let _ = fs::remove_file(&path);
            let listener =
                UnixListener::bind(&path).expect("failed to listen for commands from the user");
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = String::new();
                if let Err(err) = stream.read_to_string(&mut buf) {
                    eprintln!("Failed to read target volume from socket: {err}");
                    continue;
                };
                let mut trimmed = buf.trim();
                if trimmed == "get-volume" {
                    let (tx, rx) = mpsc::sync_channel(1);
                    change_volume.send(Message::GetVolume(tx)).unwrap();
                    let v = rx.recv().unwrap();
                    if let Some(v) = v {
                        let s = format!("{:.2}%", v * 100.);
                        let _ = stream.write_all(s.as_bytes());
                    }
                    continue;
                }
                let duration: Option<f64> = if let Some((v, duration)) = trimmed.split_once(' ') {
                    trimmed = v.trim();
                    duration.parse().ok()
                } else {
                    None
                };
                let relative = trimmed.starts_with('+') || trimmed.starts_with('-');
                let num = if relative { &trimmed[1..] } else { trimmed };
                let percent = num.ends_with('%');
                let num = num.strip_suffix('%').unwrap_or(num);
                let mut target: f64 = if let Ok(v) = num.parse() {
                    v
                } else {
                    eprintln!("Failed to parse volume command from socket.");
                    continue;
                };
                if percent {
                    target /= 100.;
                }

                let v = if relative {
                    if trimmed.starts_with('+') {
                        ChangeVolume::Increase(target)
                    } else {
                        ChangeVolume::Increase(-target)
                    }
                } else {
                    ChangeVolume::Absolute(target)
                };

                change_volume
                    .send(Message::Change {
                        volume: v,
                        duration,
                    })
                    .unwrap();
            }
            process::exit(0);
        });
    }

    loop {
        let message = if volume.is_none() {
            if verbose {
                println!("Waiting for command.");
            }
            Some(rx_change_volume.recv().unwrap())
        } else {
            rx_change_volume.try_recv().ok()
        };
        let start = Instant::now();
        match message {
            Some(Message::Change {
                volume: change,
                duration: user_duration,
            }) => {
                if verbose {
                    println!("Change volume!");
                }
                if sink.is_none() || sink_last_changed.elapsed() > Duration::from_secs(1) {
                    if verbose {
                        println!("QUERY SINK");
                    }
                    sink = get_default_sink(&ctx);
                    sink_last_changed = Instant::now();
                }
                if let Some(sink) = &sink {
                    if let Some((v, _sink_idx, chs)) = get_volume(sink, &ctx) {
                        let i_volume = vol_to_linear(v.max());
                        let mut target_volume = change
                            .collapse(if let Some(v) = volume { v } else { i_volume })
                            .max(0.);
                        if clamp {
                            target_volume = target_volume.min(1.);
                        }
                        volume = Some(target_volume);
                        initial_volume = Some(i_volume);
                        let used_duration = match user_duration {
                            Some(d) if (0.0..=1e9).contains(&d) => {
                                Duration::from_secs_f64(d * 1e-3)
                            }
                            _ => duration,
                        };
                        if used_duration <= interval {
                            step = Some(target_volume - i_volume)
                        } else {
                            step = Some(
                                (target_volume - i_volume)
                                    / (used_duration.as_millis() / interval.as_millis()) as f64,
                            );
                        }
                        iterations = 0;
                        channels = Some(chs);
                        if verbose {
                            println!(
                                "Initial {i_volume} => {target_volume} by steps {}",
                                step.unwrap()
                            );
                        }
                    } else {
                        eprintln!("The volume of the default sink couldn't be found.");
                        continue;
                    }
                } else {
                    eprintln!("No default sink was found.");
                    continue;
                }
            }
            Some(Message::GetVolume(tx)) => {
                if verbose {
                    println!("Get volume");
                }
                if let Some(sink) = &sink {
                    let v = get_volume(sink, &ctx);
                    tx.send(v.map(|(chw, _, _)| vol_to_linear(chw.avg())))
                        .unwrap();
                } else {
                    tx.send(None).unwrap();
                }
            }
            None => {}
        }
        if let (Some(target), Some(initial_volume), Some(step), Some(sink), Some(channels)) =
            (volume, initial_volume, step, &sink, channels)
        {
            let mut v = initial_volume + step * iterations as f64;
            if step.is_sign_positive() {
                if v >= target {
                    volume = None;
                    v = target;
                }
            } else if v <= target {
                volume = None;
                v = target;
            }

            set_volume(sink, channels, v, &ctx);

            iterations += 1;
        } else {
            volume = None;
        }
        let loop_duration = start.elapsed();
        if print_timings {
            println!("Loop took {loop_duration:?}");
        }
        thread::sleep(interval.saturating_sub(loop_duration));
    }
}

fn get_default_sink(ctx: &Context) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    ctx.introspect().get_server_info(move |info| {
        tx.send(
            info.default_sink_name
                .as_ref()
                .map(|c| c.clone().into_owned()),
        )
        .unwrap();
    });
    rx.recv().unwrap()
}
fn get_volume(sink: &str, ctx: &Context) -> Option<(ChannelVolumes, u32, u8)> {
    let (tx, rx) = mpsc::channel();
    ctx.introspect().get_sink_info_by_name(sink, move |info| {
        if let ListResult::Item(info) = info {
            tx.send(Some((info.volume, info.index, info.volume.len())))
                .unwrap();
        } else {
            tx.send(None).unwrap();
        }
    });
    let mut first = None;
    while let Some(item) = rx.recv().unwrap() {
        first = Some(item);
    }
    first
}
fn get_channels(sink: &str, ctx: &Context) -> Option<u8> {
    get_volume(sink, ctx).map(|(_, _, chs)| chs)
}
fn set_volume(sink: &str, channels: u8, vol: f64, ctx: &Context) {
    let mut volume = ChannelVolumes::default();
    volume.set_len(channels);
    volume.set(channels, vol_from_linear(vol));
    ctx.introspect()
        .set_sink_volume_by_name(sink, &volume, None);
}
fn vol_to_linear(volume: Volume) -> f64 {
    (volume.0 as f64 / Volume::NORMAL.0 as f64 * 1e4).round() / 1e4
}
fn vol_from_linear(volume: f64) -> Volume {
    Volume((volume * Volume::NORMAL.0 as f64) as u32)
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
