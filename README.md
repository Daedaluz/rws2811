# rws2811

WS2811 LED strip controller that receives frames over UDP and drives them out over SPI. Single-threaded, uses `epoll` + `timerfd` + `signalfd` — no threads, no busy-wait.

## How it works

1. A UDP sender pushes raw RGB byte frames to the configured address/port.
2. `rws2811` buffers the latest received frame and outputs it to the SPI device at the configured frame rate.
3. If multiple UDP packets arrive between timer ticks, all but the last are dropped (the socket is drained on every readable event so the sender is never stalled).

## Usage

```
rws2811 [OPTIONS]

Options:
  --device <PATH>    SPI device              [default: /dev/spidev0.0]
  --speed  <HZ>      SPI clock speed in Hz   [default: 2000000]
  --rate   <FPS>     Output frame rate        [default: 60]
  --size   <BYTES>   Frame buffer size        [default: 150]
  --listen <ADDR>    UDP bind address         [default: 0.0.0.0:1337]
```

**Frame buffer size** should equal `num_leds × bytes_per_led` (e.g. 50 RGB LEDs → 150 bytes).

**SPI device** is passed as the systemd instance name when using the packaged service (see below).

## Pre-flight checks

On startup the binary validates:

- **MTU** — warns if the frame size exceeds the UDP payload capacity of any network interface (`MTU - 28`). Exits only if *no* interface can carry a full frame.
- **SPI throughput** — exits if the requested FPS is not achievable at the given SPI speed and frame size.

## Sending frames

Send raw bytes to the UDP port. Example with `socat`:

```sh
# Send a single frame of 150 bytes (50 RGB LEDs, all red)
python3 -c "import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.sendto(b'\xff\x00\x00'*50, ('192.168.1.x', 1337))"
```

## Raspberry Pi package

A ready-made `.deb` is built for Raspberry Pi (arm64) and Raspberry Pi Zero (armhf). Installation uses debconf to configure the service interactively:

```sh
sudo apt install ./rws2811-rpi_*.deb
```

During install you are asked:

| Question | Default |
|---|---|
| Enable SPI in `/boot/firmware/config.txt`? | yes |
| Frame buffer size (bytes) | 150 |

All other options (SPI speed, device, frame rate, listen address) use defaults and can be changed afterwards in `/etc/default/rws2811`.

If SPI is newly enabled, a reboot is required before the service starts.

### Systemd service

The service is a template unit keyed on the SPI device node:

```sh
# Enable and start on /dev/spidev0.0
systemctl enable --now rws2811@spidev0.0

# Enable and start on /dev/spidev0.1
systemctl enable --now rws2811@spidev0.1
```

Runtime options are read from `/etc/default/rws2811`:

```sh
RWS2811_OPTS="--size 150 --speed 2000000 --rate 60 --listen 0.0.0.0:1337"
```

Edit this file and restart the service to apply changes:

```sh
systemctl restart rws2811@spidev0.0
```

## Building

### Local (requires Rust)

```sh
cargo build --release
```

### Debian package — Raspberry Pi (arm64, aarch64)

```sh
docker build -f Dockerfile.rpi -o out .
```

### Debian package — Raspberry Pi Zero (armhf, ARMv6)

```sh
docker build -f Dockerfile.rpi-zero -o out .
```

Both commands write the `.deb` to `./out/`. Requires Docker with `buildx` and QEMU binfmt support (`docker run --rm --privileged multiarch/qemu-user-static --reset -p yes`).

## License

MIT
