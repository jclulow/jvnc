use anyhow::{bail, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{
    AsyncWriteExt,
    AsyncBufRead,
    AsyncReadExt,
    AsyncRead,
    BufReader,
};

async fn read_handshake<R: AsyncRead + Unpin>(r: &mut R) -> Result<String> {
    let mut hs = String::new();
    loop {
        let b = r.read_u8().await?;
        if b < 128 {
            hs.push(b as char);
        } else {
            bail!("invalid byte {:?}", b);
        }

        if let Some(last) = hs.chars().last() {
            if last == '\n' {
                return Ok(hs.trim_end_matches('\n').to_string());
            }
        }
    }
}

fn when() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        as u64
}

async fn process_socket(mut sock: TcpStream) -> Result<()> {
    let (r, mut w) = sock.split();
    let mut br = BufReader::new(r);

    /*
     * Send the RFB ProtocolVersion Handshake.
     */
    let hs = b"RFB 003.008\n";
    w.write_all(hs).await?;

    let chs = read_handshake(&mut br).await?;
    if chs != "RFB 003.008" {
        bail!("invalid handshake: {:?}", chs);
    }
    println!("handshake -> {}", chs);

    /*
     * Security Handshake:
     */
    w.write_u8(1).await?; /* 1 type */
    w.write_u8(1).await?; /* type None */

    /*
     * Wait for security selection:
     */
    let b = br.read_u8().await?;
    if b != 1 {
        bail!("invalid security selection: {}", b);
    }

    /*
     * SecurityResult Handshake:
     */
    w.write_u32(0).await?; /* ok */

    let b = br.read_u8().await?;
    if b == 0 {
        println!("ClientInit: exclusive access");
    } else {
        println!("ClientInit: shared access");
    }

    let mut extsize = false;
    let mut gw = 1024;
    let mut gh = 768;
    let mut colour = 0u8;
    let mut colourup = true;

    let mut screenbuf = Vec::with_capacity(4 * gh as usize * gw as usize);
    let mut lastdraw = when();

    /*
     * ServerInit:
     */
    w.write_u16(gw).await?; /* width, pixels */
    w.write_u16(gh).await?; /* height, pixels */

    /* PIXEL_FORMAT */
    w.write_u8(32).await?; /* bpp */
    w.write_u8(24).await?; /* depth */
    w.write_u8(0).await?; /* big endian */
    w.write_u8(1).await?; /* true colour */
    w.write_u16(255).await?; /* red max */
    w.write_u16(255).await?; /* green max */
    w.write_u16(255).await?; /* blue max */
    w.write_u8(16).await?; /* red shift */
    w.write_u8(8).await?; /* green shift */
    w.write_u8(0).await?; /* blue shift */
    w.write_u8(0).await?; /* padding ... */
    w.write_u8(0).await?;
    w.write_u8(0).await?; /* ... padding */

    w.write_u32(4).await?; /* name length */
    let buf = b"jvnc";
    w.write_all(buf).await?;

    loop {
        /*
         * Read the message type:
         */
        let b = br.read_u8().await?;

        match b {
            0 => {
                /*
                 * SetPixelFormat
                 */
                for _ in 0..3 {
                    /*
                     * 3 x Padding:
                     */
                    br.read_u8().await?;
                }

                let mut pf = [0u8; 16];
                br.read_exact(&mut pf).await?;
                println!("pixel format: {:?}", pf);
                /*
                 * XXX do something with this, rather than assume:
                 *  depth 24 (32bpp) little-endian rgb888
                 */
            }
            2 => {
                /*
                 * SetEncodings
                 */
                for _ in 0..1 {
                    /*
                     * 1 x Padding:
                     */
                    br.read_u8().await?;
                }

                let nenc = br.read_u16().await?;
                for i in 0..nenc {
                    let enc = br.read_i32().await?;

                    println!("encoding {:>2}: {:>08X}", i, enc);

                    if enc == -308 {
                        extsize = true;
                    }
                }
                /*
                 * XXX assume Raw(0) is present for now...
                 */
            }
            3 => {
                /*
                 * FramebufferUpdateRequest
                 */
                let increm = br.read_u8().await? != 0;
                let xpos = br.read_u16().await?;
                let ypos = br.read_u16().await?;
                let width = br.read_u16().await?;
                let height = br.read_u16().await?;

                /*
                println!("update (increm? {}) @ {}x{}, {}w {}h",
                    increm, xpos, ypos, width, height);
                */

                /*
                 * Fashion some pixel data for the client...
                 */
                w.write_u8(0).await?; /* type: FramebufferUpdate */
                w.write_u8(0).await?; /* padding */

                if extsize && !increm {
                    /*
                     * Send the ExtendedDesktopSize rectangle.
                     */
                    w.write_u16(1).await?; /* nrects */

                    w.write_u16(0).await?; /* xpos */
                    w.write_u16(0).await?; /* ypos */
                    w.write_u16(gw).await?; /* width */
                    w.write_u16(gh).await?; /* height */
                    w.write_i32(-308).await?; /* ExtendedDesktopSize? */

                    w.write_u8(1).await?; /* nscreens */
                    w.write_u8(0).await?; /* padding */
                    w.write_u8(0).await?; /* padding */
                    w.write_u8(0).await?; /* padding */

                    w.write_u32(0).await?; /* id */
                    w.write_u16(0).await?; /* xoffset */
                    w.write_u16(0).await?; /* yoffset */
                    w.write_u16(gw).await?; /* width */
                    w.write_u16(gh).await?; /* height */
                    w.write_u32(0).await?; /* flags (unused) */
                } else {
                    w.write_u16(1).await?; /* nrects */

                    w.write_u16(0).await?; /* xpos */
                    w.write_u16(0).await?; /* ypos */
                    w.write_u16(gw).await?; /* width */
                    w.write_u16(gh).await?; /* height */
                    w.write_i32(0).await?; /* encoding: Raw */

                    screenbuf.clear();
                    for y in 0..gh {
                        let mut c = (y % 2 == 0) as usize;
                        for x in 0..gw {
                            if c % 2 == 0 {
                                screenbuf.push(colour);
                                screenbuf.push(0);
                                screenbuf.push(0);
                                screenbuf.push(0);
                            } else {
                                screenbuf.push(0);
                                screenbuf.push(0);
                                screenbuf.push(0);
                                screenbuf.push(0);
                            }
                            c += 10;
                        }
                    }

                    w.write_all(&screenbuf).await?;

                    let now = when();
                    let delta = now - lastdraw;
                    if delta > 4 {
                        for _ in 0..(delta / 4) {
                            if colourup {
                                colour += 1;
                                if colour > 240 {
                                    colourup = false;
                                }
                            } else {
                                colour -= 1;
                                if colour < 10 {
                                    colourup = true;
                                }
                            }
                        }
                        lastdraw = now;
                    }
                }
            }
            4 => {
                /*
                 * KeyEvent
                 */
                let downflag = br.read_u8().await?;
                for _ in 0..2 {
                    /*
                     * 2 x Padding:
                     */
                    br.read_u8().await?;
                }
                let key = br.read_u32().await?;
            }
            5 => {
                /*
                 * PointerEvent
                 */
                let button_mask = br.read_u8().await?;
                let xpos = br.read_u16().await?;
                let ypos = br.read_u16().await?;
            }
            6 => {
                /*
                 * ClientCutText
                 */
                for _ in 0..3 {
                    /*
                     * 2 x Padding:
                     */
                    br.read_u8().await?;
                }
                let len = br.read_u32().await?;
                for _ in 0..len {
                    /*
                     * Discard the Latin-1 cut buffer text:
                     */
                    br.read_u8().await?;
                }
            }
            251 => {
                /*
                 * SetDesktopSize
                 */
                br.read_u8().await?; /* padding */
                let nw = br.read_u16().await?;
                let nh = br.read_u16().await?;

                let nscreens = br.read_u8().await?;
                br.read_u8().await?; /* padding */
                for i in 0..nscreens {
                    let mut pf = [0u8; 16];
                    br.read_exact(&mut pf).await?;
                    println!("screen[{}]: {:?}", i, pf);
                }

                if nw >= 10000 || nh >= 10000 {
                    bail!("illegal resolution: {}x{}", nw, nh);
                }

                println!("CLIENT RESIZE: {}x{}", nw, nh);
                gw = nw;
                gh = nh;
                screenbuf = Vec::with_capacity(4 * gh as usize * gw as usize);

                /*
                 * Send the ExtendedDesktopSize rectangle.
                 */
                w.write_u8(0).await?; /* type: FramebufferUpdate */
                w.write_u8(0).await?; /* padding */

                w.write_u16(1).await?; /* nrects */

                w.write_u16(1).await?; /* xpos */
                w.write_u16(0).await?; /* ypos */
                w.write_u16(gw).await?; /* width */
                w.write_u16(gh).await?; /* height */
                w.write_i32(-308).await?; /* ExtendedDesktopSize? */

                w.write_u8(1).await?; /* nscreens */
                w.write_u8(0).await?; /* padding */
                w.write_u8(0).await?; /* padding */
                w.write_u8(0).await?; /* padding */

                w.write_u32(0).await?; /* id */
                w.write_u16(0).await?; /* xoffset */
                w.write_u16(0).await?; /* yoffset */
                w.write_u16(gw).await?; /* width */
                w.write_u16(gh).await?; /* height */
                w.write_u32(0).await?; /* flags (unused) */
            }
            n => {
                bail!("unknown type {}", b);
            }
        }
    }

    println!("shutting down");
    w.shutdown().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:5915").await?;

    loop {
        let (socket, addr) = listener.accept().await?;
        println!("accept: {:?}", addr);
        process_socket(socket).await?;
        println!("ok");
    }
}
