use anyhow::{bail, Result};
use tokio::net::{TcpListener, TcpStream};
use futures::StreamExt;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use std::time::Duration;
use tokio::time::{Instant, sleep_until};
use std::sync::atomic::{AtomicU32, Ordering};

mod framebuffer;
mod rfb;
use rfb::{Frame, Security, UpdateRequest};

fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

fn spawn_draw(
    cc: &Arc<AtomicU32>,
    fb: &Arc<framebuffer::Framebuffer>
) -> Result<()> {
    let fb = Arc::clone(fb);
    let cc = Arc::clone(cc);
    std::thread::Builder::new()
        .name("draw".to_string())
        .spawn(move || {
            let mut colour = 0u8;
            let mut colourup = true;

            /*
             * Make a tartan of alternating colours with squares of this size:
             */
            let pitch = 16;

            loop {
                /*
                 * Put breathing blue everywhere:
                 */
                for y in 0..fb.height() {
                    let mut c = (y % pitch < pitch / 2) as usize * (pitch / 2);
                    for x in 0..fb.width() {
                        if c % pitch < (pitch / 2) {
                            fb.put(x, y, 0, 0, 0);
                        } else {
                            match cc.load(Ordering::Relaxed) {
                                0 => fb.put(x, y, 0, 0, 0),
                                1 => fb.put(x, y, colour, colour, colour),
                                2 => fb.put(x, y, colour, 0, 0),
                                3 => fb.put(x, y, 0, colour, 0),
                                4 => fb.put(x, y, 0, 0, colour),
                                _ => (),
                            }
                        }
                        c += 1;
                    }
                }

                for _ in 0..8 {
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

                sleep_ms(50);
            }
        })?;
    Ok(())
}

async fn process_socket(
    fb: &Arc<framebuffer::Framebuffer>,
    mut sock: TcpStream,
    cc: &Arc<AtomicU32>,
) -> Result<()> {
    let (r, mut w) = sock.split();
    let rfb = rfb::read_stream(r);
    tokio::pin!(rfb);

    /*
     * Send the RFB ProtocolVersion Handshake.
     */
    let hs = b"RFB 003.008\n";
    w.write_all(hs).await?;

    /*
     * Wait for the client to return a handshake:
     */
    match rfb.next().await.transpose()? {
        Some(Frame::ProtocolVersion(ver)) => {
            if &ver != "RFB 003.008" {
                bail!("invalid handshake: {:?}", ver);
            }
        }
        Some(f) => {
            bail!("unexpected frame: {:?}", f);
        }
        None => {
            println!("stream done early?");
            return Ok(());
        }
    }

    /*
     * Security Handshake:
     */
    w.write_u8(1).await?; /* 1 type */
    w.write_u8(1).await?; /* type None */

    /*
     * Wait for client to choose:
     */
    match rfb.next().await.transpose()? {
        Some(Frame::SecuritySelection(Security::None)) => {
            println!("  security: none");
        }
        Some(f) => {
            bail!("unexpected frame: {:?}", f);
        }
        None => {
            println!("stream done early?");
            return Ok(());
        }
    }

    /*
     * SecurityResult Handshake:
     */
    w.write_u32(0).await?; /* ok */

    /*
     * Wait for client init:
     */
    let _acc = match rfb.next().await.transpose()? {
        Some(Frame::ClientInit(acc)) => {
            println!("  access: {:?}", acc);
            acc
        }
        Some(f) => {
            bail!("unexpected frame: {:?}", f);
        }
        None => {
            println!("stream done early?");
            return Ok(());
        }
    };

    /*
     * ServerInit:
     */
    w.write_u16(fb.width() as u16).await?; /* width, pixels */
    w.write_u16(fb.height() as u16).await?; /* height, pixels */

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

    let mut draw: Option<UpdateRequest> = None;
    let mut drawtime = Instant::now();
    let fps = 12;

    loop {
        tokio::select! {
            _ = sleep_until(drawtime), if draw.is_some() => {
                let ur = draw.take().unwrap();

                /*
                 * Fashion some pixel data for the client...
                 */
                w.write_u8(0).await?; /* type: FramebufferUpdate */
                w.write_u8(0).await?; /* padding */

                w.write_u16(1).await?; /* nrects */

                w.write_u16(ur.xpos as u16).await?; /* xpos */
                w.write_u16(ur.ypos as u16).await?; /* ypos */
                w.write_u16(ur.width as u16).await?; /* width */
                w.write_u16(ur.height as u16).await?; /* height */
                w.write_i32(0).await?; /* encoding: Raw */

                let mut v = Vec::new();
                for y in ur.ypos..(ur.ypos + ur.height) {
                    for x in ur.xpos..(ur.xpos + ur.width) {
                        let (r, g, b) = fb.get(x, y);
                        v.push(b);
                        v.push(g);
                        v.push(r);
                        v.push(0);
                    }
                }
                w.write_all(&v).await?;

                /*
                 * Schedule the next draw cycle at the expected time
                 * based on the target maximum frame rate:
                 */
                drawtime = Instant::now()
                    .checked_add(Duration::from_millis(1000 / fps))
                    .unwrap();
            }
            f = rfb.next() => {
                let f = match f {
                    Some(f) => f?,
                    None => return Ok(()),
                };

                match f {
                    Frame::FramebufferUpdateRequest(mut ur) => {
                        /*
                         * Make sure the update request is not out of bounds for
                         * the actual framebuffer we have:
                         */
                        if ur.xpos >= fb.width() {
                            ur.xpos = fb.width() - 1;
                        }
                        if ur.ypos >= fb.height() {
                            ur.ypos = fb.height() - 1;
                        }
                        if ur.width > fb.width() {
                            ur.width = fb.width();
                        }
                        if ur.height > fb.height() {
                            ur.height = fb.height();
                        }

                        /*
                         * Schedule a redraw at the next appropriate moment:
                         */
                        draw = Some(ur);
                    }
                    Frame::KeyEvent(down, key) if down == 1 && key == 113 => {
                        println!("q is for quit!");
                        return Ok(());
                    }
                    Frame::KeyEvent(down, key) if down == 1 && key == 122 => {
                        println!("z is for black!");
                        cc.store(0, Ordering::Relaxed);
                    }
                    Frame::KeyEvent(down, key) if down == 1 && key == 119 => {
                        println!("w is for white!");
                        cc.store(1, Ordering::Relaxed);
                    }
                    Frame::KeyEvent(down, key) if down == 1 && key == 114 => {
                        println!("r is for red!");
                        cc.store(2, Ordering::Relaxed);
                    }
                    Frame::KeyEvent(down, key) if down == 1 && key == 103 => {
                        println!("g is for green!");
                        cc.store(3, Ordering::Relaxed);
                    }
                    Frame::KeyEvent(down, key) if down == 1 && key == 98 => {
                        println!("b is for blue!");
                        cc.store(4, Ordering::Relaxed);
                    }
                    f => {
                        println!("f: {:?}", f);
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:5915").await?;

    /*
     * Colour coordination:
     */
    let cc = Arc::new(AtomicU32::new(4));

    /*
     * Spawn the simulated framebuffer:
     */
    let fb = Arc::new(framebuffer::Framebuffer::new(512, 384));
    spawn_draw(&cc, &fb)?;

    let mut c = 0;
    loop {
        let (socket, addr) = listener.accept().await?;
        c += 1;
        println!("[{}] accept: {:?}", c, addr);

        let fb = Arc::clone(&fb);
        let cc = Arc::clone(&cc);
        tokio::spawn(async move {
            let res = process_socket(&fb, socket, &cc).await;
            println!("[{}] connection done: {:?}", c, res);
            println!();
        });
    }
}
