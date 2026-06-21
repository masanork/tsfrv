use std::io;
use tokio::io::AsyncWriteExt;
use worker::{Error, Result, Socket};

const IAC: u8 = 255;
const DONT: u8 = 254;
const WONT: u8 = 252;

pub struct TelnetProcessor {
    strip_bom: bool,
    pending: Vec<u8>,
}

impl TelnetProcessor {
    pub fn new() -> Self {
        Self {
            strip_bom: true,
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

                if index + 2 >= self.pending.len() {
                    break;
                }

                let command = self.pending[index + 1];
                let option = self.pending[index + 2];
                respond_to_negotiation(socket, command, option).await?;
                index += 3;
                continue;
            }

            output.push(self.pending[index]);
            index += 1;
        }

        self.pending.drain(..index);
        sanitize_output(output, &mut self.strip_bom);
        Ok(())
    }
}

async fn respond_to_negotiation(socket: &mut Socket, command: u8, option: u8) -> Result<()> {
    let response = match command {
        251 | 253 => Some([IAC, if command == 251 { DONT } else { WONT }, option]),
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

fn sanitize_output(output: &mut Vec<u8>, strip_bom: &mut bool) {
    if *strip_bom && output.starts_with(&[0xEF, 0xBB, 0xBF]) {
        output.drain(..3);
        *strip_bom = false;
    } else if !output.is_empty() {
        *strip_bom = false;
    }

    let cleaned = strip_ansi(output);
    output.clear();
    output.extend_from_slice(&cleaned);
}

fn strip_ansi(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;

    while index < input.len() {
        if input[index] == 0x1b {
            if index + 1 < input.len() && input[index + 1] == b'[' {
                index += 2;
                while index < input.len() && !input[index].is_ascii_alphabetic() {
                    index += 1;
                }
                if index < input.len() {
                    index += 1;
                }
                continue;
            }

            if index + 1 < input.len() && input[index + 1] == b']' {
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
        let mut output = b"\xEF\xBB\xBF\x1B[0mhello".to_vec();
        sanitize_output(&mut output, &mut true);
        assert_eq!(output, b"hello");
    }

    #[test]
    fn strips_csi_color_sequence() {
        let mut output = b"\x1B[31mred\x1B[0m".to_vec();
        sanitize_output(&mut output, &mut true);
        assert_eq!(output, b"red");
    }
}