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
use std::thread;
use std::time::Duration;

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

fn main() {
    let args = Args::parse();

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
                    loop {
                        match sock.recv_from(&mut recv_buf) {
                            Ok((n, _)) => {
                                let len = n.min(buffer.len());
                                buffer[..len].copy_from_slice(&recv_buf[..len]);
                                dirty = true;
                            }
                            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                            Err(e) => {
                                eprintln!("recv: {e}");
                                std::process::exit(1);
                            }
                        }
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
