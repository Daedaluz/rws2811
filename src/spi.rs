use nix::libc;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const SPI_IOC_MAGIC: u8 = b'k';

#[repr(C)]
struct SpiIocTransfer {
    tx_buf: u64,
    rx_buf: u64,
    len: u32,
    speed_hz: u32,
    delay_usecs: u16,
    bits_per_word: u8,
    cs_change: u8,
    tx_nbits: u8,
    rx_nbits: u8,
    word_delay_usecs: u8,
    pad: u8,
}

nix::ioctl_write_ptr!(spi_wr_max_speed_hz, SPI_IOC_MAGIC, 4, u32);
nix::ioctl_write_ptr!(spi_wr_bits_per_word, SPI_IOC_MAGIC, 3, u8);
nix::ioctl_write_ptr!(spi_wr_mode32, SPI_IOC_MAGIC, 5, u32);
nix::ioctl_write_ptr!(spi_ioc_message, SPI_IOC_MAGIC, 0, SpiIocTransfer);

pub type Mode = u32;

pub struct Config {
    pub mode: Mode,
    pub bits: u8,
    pub speed: u32,
    pub delay_usec: u16,
    pub cs_change: bool,
    pub tx_nbits: u8,
    pub rx_nbits: u8,
    pub word_delay_usec: u8,
}

pub struct Device {
    fd: OwnedFd,
    cfg: Config,
}

impl Device {
    pub fn open(path: &str, cfg: Config) -> io::Result<Self> {
        let c_path = CString::new(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let raw_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR) };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        unsafe {
            spi_wr_max_speed_hz(fd.as_raw_fd(), &cfg.speed)
                .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
            spi_wr_bits_per_word(fd.as_raw_fd(), &cfg.bits)
                .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
            spi_wr_mode32(fd.as_raw_fd(), &cfg.mode)
                .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
        }

        Ok(Device { fd, cfg })
    }

    pub fn tx(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        let mut read = vec![0u8; data.len()];

        let xfer = SpiIocTransfer {
            tx_buf: data.as_ptr() as u64,
            rx_buf: read.as_mut_ptr() as u64,
            len: data.len() as u32,
            speed_hz: self.cfg.speed,
            delay_usecs: self.cfg.delay_usec,
            bits_per_word: self.cfg.bits,
            cs_change: u8::from(self.cfg.cs_change),
            tx_nbits: self.cfg.tx_nbits,
            rx_nbits: self.cfg.rx_nbits,
            word_delay_usecs: self.cfg.word_delay_usec,
            pad: 0,
        };

        unsafe {
            spi_ioc_message(self.fd.as_raw_fd(), &xfer)
                .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
        }

        Ok(read)
    }
}
