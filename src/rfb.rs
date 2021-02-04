use std::io::{Result, Error, ErrorKind};

use async_stream::try_stream;
use bytes::{BytesMut, Buf};
use futures_core::stream::Stream;
use tokio::io::AsyncReadExt;
use tokio::net::tcp::ReadHalf;

trait SighFactoryExt {
    fn peek_u16(&self, offset: usize) -> Option<u16>;
    fn peek_u32(&self, offset: usize) -> Option<u32>;
}

impl SighFactoryExt for BytesMut {
    fn peek_u16(&self, offset: usize) -> Option<u16> {
        if self.len() < offset + 2 {
            None
        } else {
            let b0 = self[offset] as u16;
            let b1 = self[offset + 1] as u16;
            Some(b0 << 8 | b1)
        }
    }

    fn peek_u32(&self, offset: usize) -> Option<u32> {
        if self.len() < offset + 2 {
            None
        } else {
            let b0 = self[offset] as u32;
            let b1 = self[offset + 1] as u32;
            let b2 = self[offset + 2] as u32;
            let b3 = self[offset + 3] as u32;
            Some(b0 << 24 | b1 << 16 | b2 << 8 | b3)
        }
    }
}

#[derive(Debug)]
pub enum Security {
    None,
}

#[derive(Debug)]
pub enum Access {
    Exclusive,
    Shared,
}

#[derive(Debug)]
pub struct UpdateRequest {
    pub incremental: bool,
    pub xpos: usize,
    pub ypos: usize,
    pub width: usize,
    pub height: usize,
}

#[derive(Debug)]
pub enum Frame {
    ProtocolVersion(String),
    SecuritySelection(Security),
    ClientInit(Access),
    SetPixelFormat,
    SetEncodings(Vec<i32>),
    KeyEvent(u8, u32),
    PointerEvent(u8, u16, u16),
    ClientCutText,
    FramebufferUpdateRequest(UpdateRequest),
    EOF,
}

enum State {
    Version,
    SecuritySelection,
    ClientInit,
    Message,
}

struct Rfb {
    buf: BytesMut,
    eof: bool,
    failed: bool,
    state: State,
}

fn fail_<T>(msg: &str) -> Result<T> {
    Err(Error::new(ErrorKind::Other, msg.to_string()))
}

impl Rfb {
    fn new() -> Self {
        Rfb {
            buf: BytesMut::with_capacity(4096),
            eof: false,
            failed: false,
            state: State::Version,
        }
    }

    fn fail<T>(&mut self, msg: &str) -> Result<T> {
        if self.failed {
            return fail_("earlier failure");
        }
        self.failed = true;
        return fail_(msg);
    }

    fn parse(&mut self) -> Result<Option<Frame>> {
        if self.failed {
            return self.fail("");
        }

        /*
         * To avoid a check in the state switch below, we require at least one
         * byte (typically the message ID) in the front of the buffer for all
         * states:
         */
        if self.buf.is_empty() {
            if self.eof {
                return Ok(Some(Frame::EOF));
            }
            return Ok(None);
        }

        match self.state {
            State::Version => {
                /*
                 * Wait for a complete version handshake.
                 */
                if !self.buf.contains(&('\n' as u8)) {
                    if self.buf.len() > 100 {
                        /*
                         * This handshake is too long.
                         */
                        return self.fail("handshake too long");
                    }

                    return Ok(None);
                }

                let mut s = String::new();
                loop {
                    let c = self.buf.get_u8();
                    if c >= 128 {
                        return self.fail("invalid handshake byte");
                    }
                    if c == '\n' as u8 {
                        break;
                    }
                    s.push(c as char);
                }

                self.state = State::SecuritySelection;
                return Ok(Some(Frame::ProtocolVersion(s)));
            }
            State::SecuritySelection => {
                let sec = self.buf.get_u8();
                if sec != 1 {
                    return self.fail(&format!("invalid security {}", sec));
                }

                self.state = State::ClientInit;
                return Ok(Some(Frame::SecuritySelection(Security::None)));
            }
            State::ClientInit => {
                let acc = if self.buf.get_u8() == 0 {
                    Access::Exclusive
                } else {
                    Access::Shared
                };

                self.state = State::Message;
                return Ok(Some(Frame::ClientInit(acc)));
            }
            State::Message => {
                match self.buf[0] {
                    0 => {
                        if self.buf.len() < 1 + 3 + 16 {
                            return Ok(None);
                        }

                        /*
                         * XXX
                         */
                        self.buf.advance(1 + 3 + 16);
                        return Ok(Some(Frame::SetPixelFormat));
                    }
                    2 => {
                        let nenc = if let Some(nenc) = self.buf.peek_u16(2) {
                            let nenc = nenc as usize;
                            if self.buf.len() < 4 + nenc * 4 {
                                return Ok(None);
                            } else {
                                nenc
                            }
                        } else {
                            return Ok(None);
                        };

                        self.buf.advance(4);
                        let mut encs = Vec::new();
                        for _ in 0..nenc {
                            encs.push(self.buf.get_i32());
                        }

                        return Ok(Some(Frame::SetEncodings(encs)));
                    }
                    3 => {
                        if self.buf.len() < 10 {
                            return Ok(None);
                        }

                        self.buf.advance(1);
                        let ur = UpdateRequest {
                            incremental: self.buf.get_u8() != 0,
                            xpos: self.buf.get_u16() as usize,
                            ypos: self.buf.get_u16() as usize,
                            width: self.buf.get_u16() as usize,
                            height: self.buf.get_u16() as usize,
                        };

                        return Ok(Some(Frame::FramebufferUpdateRequest(ur)));
                    }
                    4 => {
                        if self.buf.len() < 1 + 1 + 2 + 4 {
                            return Ok(None);
                        }

                        self.buf.advance(1);
                        let downflag = self.buf.get_u8();
                        self.buf.advance(2);
                        let key = self.buf.get_u32();

                        return Ok(Some(Frame::KeyEvent(downflag, key)));
                    }
                    5 => {
                        if self.buf.len() < 1 + 1 + 2 + 2 {
                            return Ok(None);
                        }

                        self.buf.advance(1);
                        let button_mask = self.buf.get_u8();
                        let xpos = self.buf.get_u16();
                        let ypos = self.buf.get_u16();

                        return Ok(Some(Frame::PointerEvent(button_mask,
                            xpos, ypos)));
                    }
                    6 => {
                        let nchar = if let Some(v) = self.buf.peek_u32(1 + 3) {
                            let nchar = v as usize;
                            if self.buf.len() < 1 + 3 + 4 + nchar {
                                return Ok(None);
                            } else {
                                nchar
                            }
                        } else {
                            return Ok(None);
                        };

                        self.buf.advance(1 + 3 + 4);
                        self.buf.advance(nchar); /* XXX */

                        return Ok(Some(Frame::ClientCutText));
                    }
                    n => {
                        return self.fail(&format!("invalid message {}", n));
                    }
                }
            }
        }
    }

    async fn ingest(&mut self, r: &mut ReadHalf<'_>) -> Result<()> {
        if self.eof {
            /*
             * XXX
             */
            return Ok(());
        }

        if r.read_buf(&mut self.buf).await? == 0 {
            self.eof = true;
        }

        Ok(())
    }
}

pub fn read_stream<'a>(r: ReadHalf<'a>)
    -> impl Stream<Item = Result<Frame>> + 'a
{
    try_stream! {
        tokio::pin!(r);
        let mut rfb = Rfb::new();

        'outer: loop {
            rfb.ingest(&mut r).await?;

            'parse: loop {
                match rfb.parse()? {
                    Some(Frame::EOF) => break 'outer,
                    Some(f) => yield f,
                    None => break 'parse,
                }
            }
        }
    }
}
