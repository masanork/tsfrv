use std::io;
use tokio::io::AsyncWriteExt;
use worker::{Error, Result, Socket};

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const CHARSET: u8 = 42;

pub struct TelnetProcessor {
    strip_bom: bool,
    ansi_pending: bool,
    pending: Vec<u8>,
}

impl TelnetProcessor {
    pub fn new() -> Self {
        Self {
            strip_bom: true,
            ansi_pending: false,
            pending: Vec::new(),
        }
    }

    pub async fn process_chunk(
        &mut self,
        socket: &mut Socket,
        chunk: &[u8],
        output: &mut Vec<u8>,
    ) -> Result<()> {
        self.pending.extend_from_slice(chunk);
        self.drain_pending(socket, output).await
    }

    async fn drain_pending(&mut self, socket: &mut Socket, output: &mut Vec<u8>) -> Result<()> {
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index] == IAC {
                if index + 1 >= self.pending.len() {
                    break;
                }

                if self.pending[index + 1] == IAC {
                    output.push(IAC);
                    index += 2;
                    continue;
                }

                if self.pending[index + 1] == SB {
                    if let Some(end) = find_subnegotiation_end(&self.pending[index..]) {
                        let frame = &self.pending[index..index + end];
                        handle_subnegotiation(socket, frame).await?;
                        index += end;
                        continue;
                    }
                    break;
                }

                if index + 2 >= self.pending.len() {
                    break;
                }

                let command = self.pending[index + 1];
                let option = self.pending[index + 2];
                respond_to_command(socket, command, option).await?;
                index += 3;
                continue;
            }

            output.push(self.pending[index]);
            index += 1;
        }

        self.pending.drain(..index);
        sanitize_output(self, output);
        Ok(())
    }
}

fn find_subnegotiation_end(frame: &[u8]) -> Option<usize> {
    if frame.len() < 2 || frame[0] != IAC || frame[1] != SB {
        return None;
    }

    for idx in 2..frame.len().saturating_sub(1) {
        if frame[idx] == IAC && frame[idx + 1] == SE {
            return Some(idx + 2);
        }
    }

    None
}

async fn handle_subnegotiation(socket: &mut Socket, frame: &[u8]) -> Result<()> {
    if frame.len() < 4 {
        return Ok(());
    }

    let option = frame[2];
    if option != CHARSET || frame.len() < 5 {
        return Ok(());
    }

    // CHARSET SEND/REQUEST -> reply CHARSET IS UTF-8
    if frame[3] == 1 || frame[3] == 3 {
        let response = [
            IAC, SB, CHARSET, 2, 1, b'U', b'T', b'F', b'-', b'8', IAC, SE,
        ];
        socket
            .write_all(&response)
            .await
            .map_err(|error| Error::RustError(error.to_string()))?;
    }

    Ok(())
}

async fn respond_to_command(socket: &mut Socket, command: u8, option: u8) -> Result<()> {
    let response = match (command, option) {
        (DO, CHARSET) => Some([IAC, WILL, CHARSET]),
        (WILL, CHARSET) => Some([IAC, DO, CHARSET]),
        (WILL, _) => Some([IAC, DONT, option]),
        (DO, _) => Some([IAC, WONT, option]),
        _ => None,
    };

    if let Some(bytes) = response {
        socket
            .write_all(&bytes)
            .await
            .map_err(|error| Error::RustError(error.to_string()))?;
    }

    Ok(())
}

fn sanitize_output(processor: &mut TelnetProcessor, output: &mut Vec<u8>) {
    if processor.strip_bom && output.starts_with(&[0xEF, 0xBB, 0xBF]) {
        output.drain(..3);
        processor.strip_bom = false;
    } else if !output.is_empty() {
        processor.strip_bom = false;
    }

    let cleaned = strip_ansi(output, &mut processor.ansi_pending);
    let normalized = normalize_crlf(&cleaned);
    output.clear();
    output.extend_from_slice(&normalized);
}

fn normalize_crlf(input: &[u8]) -> Vec<u8> {
    input.iter().copied().filter(|byte| *byte != b'\r').collect()
}

fn strip_ansi(input: &[u8], pending: &mut bool) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;

    if *pending {
        if !input.is_empty() && input[0] == b'[' {
            index = 1;
            while index < input.len() && !input[index].is_ascii_alphabetic() {
                index += 1;
            }
            if index < input.len() {
                index += 1;
            }
            *pending = false;
        } else {
            output.push(0x1b);
            *pending = false;
        }
    }

    while index < input.len() {
        if input[index] == 0x1b {
            if index + 1 >= input.len() {
                *pending = true;
                break;
            }

            if input[index + 1] == b'[' {
                index += 2;
                while index < input.len() && !input[index].is_ascii_alphabetic() {
                    index += 1;
                }
                if index < input.len() {
                    index += 1;
                }
                continue;
            }

            if input[index + 1] == b']' {
                index += 2;
                while index < input.len() {
                    if input[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if input[index] == 0x1b {
                        break;
                    }
                    index += 1;
                }
                continue;
            }

            output.push(input[index]);
            index += 1;
            continue;
        }

        output.push(input[index]);
        index += 1;
    }

    output
}

pub fn io_error(error: io::Error) -> Error {
    Error::RustError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bom_and_ansi_reset() {
        let mut processor = TelnetProcessor::new();
        let mut output = b"\xEF\xBB\xBF\x1B[0mhello".to_vec();
        sanitize_output(&mut processor, &mut output);
        assert_eq!(output, b"hello");
    }

    #[test]
    fn strips_csi_color_sequence() {
        let mut processor = TelnetProcessor::new();
        let mut output = b"\x1B[31mred\x1B[0m".to_vec();
        sanitize_output(&mut processor, &mut output);
        assert_eq!(output, b"red");
    }

    #[test]
    fn strips_ansi_across_chunk_boundary() {
        let mut pending = false;
        let first = strip_ansi(b"\x1B", &mut pending);
        assert!(pending);
        assert!(first.is_empty());

        let second = strip_ansi(b"[0mhi", &mut pending);
        assert!(!pending);
        assert_eq!(second, b"hi");
    }

    #[test]
    fn finds_subnegotiation_frame() {
        let frame = [IAC, SB, CHARSET, 1, IAC, SE];
        assert_eq!(find_subnegotiation_end(&frame), Some(6));
    }

    #[test]
    fn normalizes_crlf_line_endings() {
        assert_eq!(normalize_crlf(b"a\r\nb\r\nc"), b"a\nb\nc");
        assert_eq!(normalize_crlf(b"\n\r\n"), b"\n\n");
        assert_eq!(normalize_crlf(b"lone\r"), b"lone");
    }
}