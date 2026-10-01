# Codec2

Add this to your `Cargo.toml`:

```toml
[dependencies]
codec2 = "0.3"
```

## Usage
Currently 3200, 2400, 1600, 1400, 1300 and 1200 bitrates encoding and decoding are implemented.

Create a Codec2 object e.g. `Codec2::new(Codec2Mode::MODE_3200)` then repeatedly obtain raw 8khz
16-bit audio samples and call `encode` to encode blocks of `samples_per_frame()` samples into
`bits_per_frame()` compressed bits, in order. On the receiving end, create a Codec2 object and
repeatedly call `decode` to decompress each chunk of received bytes into the next samples.

Complete example below. This example uses zerocopy to interpret the `&[i16]` slices as `&[u8]` for
I/O.

Cargo.toml:

```toml
[dependencies]
zerocopy = "0.3.0"
codec2 = "0"
```

main.rs:

```rust
use codec2::*;
use std::env::args;
use std::io::prelude::*;
use zerocopy::AsBytes;

fn main() -> std::io::Result<()> {
    if args().len() != 4 || (args().nth(1).unwrap() != "enc" && args().nth(1).unwrap() != "dec") {
        eprintln!("Usage: {} (enc|dec) inputfile outputfilename", args().nth(0).unwrap());
        eprintln!("Files should be raw 16-bit signed 8khz audio");
        return Ok(());
    }
    let mut fin = std::fs::File::open(args().nth(2).unwrap())?;
    let mut fout = std::fs::File::create(args().nth(3).unwrap())?;
    let mut c2 = Codec2::new(Codec2Mode::MODE_3200);
    let mut samps = vec![0; c2.samples_per_frame()]; //i16 I/O buffer
    let mut packed = vec![0; (c2.bits_per_frame() + 7) / 8]; //u8 I/O buffer for encoded bits
    if args().nth(1).unwrap() == "enc" {
        while fin.read_exact(samps.as_bytes_mut()).is_ok() {
            c2.encode(&mut packed, &samps[..]);
            fout.write_all(&packed)?;
        }
    } else {
        while fin.read_exact(&mut packed).is_ok() {
            c2.decode(&mut samps[..], &packed);
            fout.write_all(samps.as_bytes())?;
        }
    }
    Ok(())
}
```
