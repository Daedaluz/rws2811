mod spi;

use clap::Parser;
use nix::sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags};
use nix::sys::signal::{SigSet, Signal};
use nix::sys::signalfd::{SfdFlags, SignalFd};
use nix::sys::time::TimeSpec;
use nix::sys::timerfd::{ClockId, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::os::fd::AsFd;
use std::{fs, thread};
use std::time::Duration;

const UDP_OVERHEAD: usize = 28; // 20 bytes IP + 8 bytes UDP

const EVENT_TIMER: u64 = 0;
const EVENT_SIGNAL: u64 = 1;
const EVENT_SOCKET: u64 = 2;

#[derive(Parser)]
struct Args {
    /// SPI speed in Hz
    #[arg(long = "speed", default_value_t = 2_000_000)]
    speed: u32,

    /// SPI device path
    #[arg(long = "device", default_value = "/dev/spidev0.0")]
    device: String,

    /// Frame rate in FPS
    #[arg(long = "rate", default_value_t = 60)]
    rate: u64,

    /// Size of frame buffer in bytes
    #[arg(long = "size", default_value_t = 150)]
    size: usize,

    /// UDP bind address
    #[arg(long = "listen", default_value = "0.0.0.0:1337")]
    listen: String,
}

fn preflight(args: &Args) {
    // Check that the frame size fits within the MTU of all non-loopback interfaces
    // Only exit if no interface can carry the frame; warn for those that can't
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        let mut too_small = Vec::new();
        let mut fits = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "lo" {
                continue;
            }
            let mtu_path = entry.path().join("mtu");
            let Ok(contents) = fs::read_to_string(&mtu_path) else { continue };
            let Ok(mtu) = contents.trim().parse::<usize>() else { continue };
            let max_payload = mtu.saturating_sub(UDP_OVERHEAD);
            if args.size > max_payload {
                too_small.push((name, mtu, max_payload));
            } else {
                fits.push((name, mtu, max_payload));
            }
        }
        if !too_small.is_empty() {
            for (name, mtu, max_payload) in &too_small {
                eprintln!(
                    "frame size {} exceeds max UDP payload {max_payload} (MTU {mtu} on {name})",
                    args.size
                );
            }
            if fits.is_empty() {
                std::process::exit(1);
            }
            for (name, mtu, max_payload) in &fits {
                eprintln!(
                    "frame size {} fits within max UDP payload {max_payload} (MTU {mtu} on {name})",
                    args.size
                );
            }
        }
    }

    // Check that the SPI bus can keep up with the requested frame rate
    // Time per frame: (size * 8 / speed) + delay_usec
    let bits_per_frame = args.size as f64 * 8.0;
    let transfer_secs = bits_per_frame / args.speed as f64;
    let delay_secs = 500e-6; // delay_usec from SPI config
    let frame_secs = transfer_secs + delay_secs;
    let max_fps = 1.0 / frame_secs;
    if (args.rate as f64) > max_fps {
        eprintln!(
            "requested {} FPS but SPI can only sustain {:.1} FPS \
             ({} bytes @ {} Hz + 500us delay = {:.2}ms per frame)",
            args.rate,
            max_fps,
            args.size,
            args.speed,
            frame_secs * 1000.0,
        );
        std::process::exit(1);
    }
}

fn main() {
    let args = Args::parse();
    preflight(&args);

    eprintln!("rws2811 v{}", env!("CARGO_PKG_VERSION"));
    eprintln!("  device : {}", args.device);
    eprintln!("  speed  : {} Hz", args.speed);
    eprintln!("  rate   : {} fps", args.rate);
    eprintln!("  size   : {} B", args.size);
    eprintln!("  listen : {}", args.listen);

    let device = loop {
        match spi::Device::open(
            &args.device,
            spi::Config {
                mode: 0,
                bits: 8,
                speed: args.speed,
                delay_usec: 500,
                cs_change: false,
                tx_nbits: 0,
                rx_nbits: 0,
                word_delay_usec: 0,
            },
        ) {
            Ok(dev) => break dev,
            Err(e) => {
                eprintln!("open: {e}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    };

    let sock = UdpSocket::bind(&args.listen).unwrap_or_else(|e| {
        eprintln!("listen: {e}");
        std::process::exit(1);
    });
    sock.set_nonblocking(true).unwrap();

    // Block SIGINT/SIGTERM and catch via signalfd
    let mut mask = SigSet::empty();
    mask.add(Signal::SIGINT);
    mask.add(Signal::SIGTERM);
    mask.thread_block().unwrap();
    let sfd = SignalFd::with_flags(&mask, SfdFlags::SFD_NONBLOCK).unwrap();

    // Timerfd for frame rate
    let tfd = TimerFd::new(ClockId::CLOCK_MONOTONIC, TimerFlags::TFD_NONBLOCK).unwrap();
    let interval = Duration::from_secs(1) / args.rate as u32;
    tfd.set(
        Expiration::Interval(TimeSpec::from_duration(interval)),
        TimerSetTimeFlags::empty(),
    )
    .unwrap();

    // Epoll on timerfd + signalfd + socket
    let epoll = Epoll::new(EpollCreateFlags::empty()).unwrap();
    epoll
        .add(&tfd, EpollEvent::new(EpollFlags::EPOLLIN, EVENT_TIMER))
        .unwrap();
    epoll
        .add(&sfd, EpollEvent::new(EpollFlags::EPOLLIN, EVENT_SIGNAL))
        .unwrap();
    epoll
        .add(sock.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, EVENT_SOCKET))
        .unwrap();

    let mut buffer = vec![0u8; args.size];
    let mut recv_buf = vec![0u8; args.size];
    let mut dirty = false;
    let mut events = [EpollEvent::empty(); 3];

    loop {
        let n = epoll.wait(&mut events, None::<u8>).unwrap();
        for event in &events[..n] {
            match event.data() {
                EVENT_SOCKET => {
                    // Drain all pending UDP packets to catch up if we would fall behind in frames
                    // This would indicate that we are not able to keep up with the incoming frame rate, and dropping frames is necessary to catch up
                    let mut drained = 0usize;
                    loop {
                        match sock.recv_from(&mut recv_buf) {
                            Ok((n, _)) => {
                                let len = n.min(buffer.len());
                                buffer[..len].copy_from_slice(&recv_buf[..len]);
                                dirty = true;
                                drained += 1;
                            }
                            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                            Err(e) => {
                                eprintln!("recv: {e}");
                                std::process::exit(1);
                            }
                        }
                    }
                    if drained > 1 {
                        eprintln!("warn: drained {drained} frames in one poll; sender outpacing output rate");
                    }
                }
                EVENT_TIMER => {
                    tfd.wait().unwrap();

                    if !dirty {
                        continue;
                    }
                    dirty = false;

                    if let Err(e) = device.tx(&buffer) {
                        eprintln!("tx error: {e}");
                        std::process::exit(1);
                    }
                }
                EVENT_SIGNAL => {
                    if let Ok(Some(sig)) = sfd.read_signal() {
                        eprintln!("received signal {}, exiting", sig.ssi_signo);
                    }
                    std::process::exit(0);
                }
                _ => {}
            }
        }
    }
}
