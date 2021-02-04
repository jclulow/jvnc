use anyhow::{bail, Result};
use tokio::net::{TcpListener, TcpStream};
use futures::StreamExt;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

mod framebuffer;
mod rfb;
use rfb::{Frame, Security, Access};

fn when() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        as u64
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

fn spawn_draw(fb: &Arc<framebuffer::Framebuffer>) -> Result<()> {
    let fb = Arc::clone(fb);
    std::thread::Builder::new()
        .name("draw".to_string())
        .spawn(move || {
            let mut colour = 0u8;
            let mut colourup = true;
            let mut lastdraw = when();

            loop {
                /*
                 * Put breathing blue everywhere:
                 */
                for y in 0..fb.height() {
                    for x in 0..fb.width() {
                        fb.put(x, y, 0, 0, colour);
                    }
                }

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

                sleep_ms(1000 / 30);
            }
        })?;
    Ok(())
}

async fn process_socket2(
    fb: &Arc<framebuffer::Framebuffer>,
    mut sock: TcpStream,
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

    /*
     * Process incoming messages:
     */
    while let Some(f) = rfb.next().await.transpose()? {
        match f {
            Frame::FramebufferUpdateRequest(ur) => {
                /*
                 * Fashion some pixel data for the client...
                 */
                w.write_u8(0).await?; /* type: FramebufferUpdate */
                w.write_u8(0).await?; /* padding */

                w.write_u16(1).await?; /* nrects */

                w.write_u16(0).await?; /* xpos */
                w.write_u16(0).await?; /* ypos */
                w.write_u16(fb.width() as u16).await?; /* width */
                w.write_u16(fb.height() as u16).await?; /* height */
                w.write_i32(0).await?; /* encoding: Raw */

                w.write_all(&fb.copy_all()).await?;
            }
            Frame::KeyEvent(downflag, key) if downflag == 1 && key == 113 => {
                println!("q is for quit!");
                return Ok(());
            }
            f => {
                println!("f: {:?}", f);
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:5915").await?;

    /*
     * Spawn the simulated framebuffer:
     */
    let fb = Arc::new(framebuffer::Framebuffer::new(512, 384));
    spawn_draw(&fb)?;

    let mut c = 0;
    loop {
        let (socket, addr) = listener.accept().await?;
        c += 1;
        println!("[{}] accept: {:?}", c, addr);

        let fb = Arc::clone(&fb);
        tokio::spawn(async move {
            let res = process_socket2(&fb, socket).await;
            println!("[{}] connection done: {:?}", c, res);
            println!();
        });
    }
}
