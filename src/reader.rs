use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, thread};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use crate::config::parse_command;
use crate::ecs::state::StateQueryKind;
use crate::errors::{Error, Result};
use crate::events::{Event, EventSender};

#[derive(Deserialize, Serialize)]
struct CommandResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// `CommandReader` is responsible for sending and receiving commands via a Unix socket.
/// It acts as an IPC mechanism for the `paneru` application, allowing external processes
/// or the CLI client to communicate with the running daemon.
pub struct CommandReader {
    events: EventSender,
}

impl CommandReader {
    /// The path to the Unix socket used for inter-process communication.
    const SOCKET_PATH: &str = "/tmp/paneru.socket";

    /// Sends a command and its arguments to the running `paneru` application via a Unix socket.
    /// The arguments are serialized and sent as a byte stream.
    ///
    /// # Arguments
    ///
    /// * `params` - An iterator over command-line arguments, where each `String` is a parameter.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the command is sent successfully, otherwise `Err(Error)` if an I/O error occurs or the connection fails.
    pub fn send_command(params: impl IntoIterator<Item = String>) -> Result<()> {
        let _stream = Self::send_socket_request(params)?;
        Ok(())
    }

    pub fn execute_command(params: impl IntoIterator<Item = String>) -> Result<()> {
        let mut stream = Self::send_socket_request(
            std::iter::once("exec-cmd".to_string()).chain(params),
        )?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;

        let mut output = String::new();
        stream.read_to_string(&mut output)?;
        let output = output.trim();
        if output.is_empty() {
            return Err(Error::Generic("empty exec-cmd response".to_string()));
        }

        let response: CommandResponse = serde_json::from_str(output)?;
        if response.ok {
            Ok(())
        } else {
            Err(Error::Generic(
                response
                    .error
                    .unwrap_or_else(|| "exec-cmd was not accepted".to_string()),
            ))
        }
    }

    pub fn send_query(kind: StateQueryKind) -> Result<String> {
        let args = match kind {
            StateQueryKind::State => ["query", "state", "--json"],
            StateQueryKind::VirtualWorkspaces => ["query", "virtual-workspaces", "--json"],
            StateQueryKind::Active => ["query", "active", "--json"],
        };
        let mut stream = Self::send_socket_request(args.into_iter().map(str::to_string))?;
        let mut output = String::new();
        stream.read_to_string(&mut output)?;
        Ok(output)
    }

    pub fn subscribe_json() -> Result<()> {
        let mut stream =
            Self::send_socket_request(["subscribe", "--json"].into_iter().map(str::to_string))?;
        std::io::copy(&mut stream, &mut std::io::stdout())?;
        Ok(())
    }

    fn send_socket_request(params: impl IntoIterator<Item = String>) -> Result<UnixStream> {
        let output = params
            .into_iter()
            .flat_map(|param| [param.as_bytes(), &[0]].concat())
            .collect::<Vec<_>>();
        let size: u32 = output.len().try_into()?;
        debug!("{:?} {output:?}", size.to_le_bytes());

        let mut stream = UnixStream::connect(CommandReader::SOCKET_PATH)?;
        stream.write_all(&size.to_le_bytes())?;
        stream.write_all(&output)?;
        Ok(stream)
    }

    /// Creates a new `CommandReader` instance.
    ///
    /// # Arguments
    ///
    /// * `events` - An `EventSender` to dispatch received commands as `Event::Command`.
    ///
    /// # Returns
    ///
    /// A new `CommandReader`.
    pub fn new(events: EventSender) -> Self {
        CommandReader { events }
    }

    /// Starts the `CommandReader` in a new thread, listening for incoming commands on a Unix socket.
    /// Any errors encountered in the runner thread are logged.
    pub fn start(mut self) {
        thread::spawn(move || {
            if let Err(err) = self.runner() {
                error!("{err}");
            }
        });
    }

    /// The main runner function for the `CommandReader` thread. It binds to a Unix socket,
    /// listens for incoming connections, reads command size and data, and dispatches them as `Event::Command`.
    /// This loop continues indefinitely until an unrecoverable error occurs.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the runner completes successfully (though it's typically a long-running loop),
    /// otherwise `Err(Error)` if a binding or I/O error occurs.
    fn runner(&mut self) -> Result<()> {
        _ = fs::remove_file(CommandReader::SOCKET_PATH);
        let listener = UnixListener::bind(CommandReader::SOCKET_PATH)?;

        for stream in listener.incoming() {
            let Ok(stream) = stream.inspect_err(|err| error!("reading stream {err}")) else {
                continue;
            };
            if let Err(err) = self.handle_connection(stream) {
                error!("handling stream: {err}");
            }
        }
        Ok(())
    }

    fn handle_connection(&self, mut stream: UnixStream) -> Result<()> {
        let mut buffer = [0u8; 4];

        if !full_read(&mut stream, buffer.len(), &mut buffer) {
            return Ok(());
        }
        let size = u32::from_le_bytes(buffer) as usize;
        let mut buffer = vec![0u8; size];

        if !full_read(&mut stream, buffer.len(), &mut buffer) {
            return Ok(());
        }
        let argv = buffer
            .split(|c| *c == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect::<Vec<_>>();
        let argv_ref = argv.iter().map(String::as_str).collect::<Vec<_>>();

        if let Some(kind) = parse_query_request(&argv_ref) {
            let (tx, rx) = channel();
            _ = self
                .events
                .send(Event::StateQuery {
                    kind,
                    respond_to: tx,
                })
                .inspect_err(|err| {
                    error!("sending state query: {err}");
                });

            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(response) => {
                    _ = stream.write_all(response.as_bytes());
                    _ = stream.write_all(b"\n");
                }
                Err(err) => error!("waiting for state query response: {err}"),
            }
            return Ok(());
        }

        if is_subscribe_request(&argv_ref) {
            match stream.try_clone() {
                Ok(clone) => {
                    if let Err(err) = clone.set_nonblocking(true) {
                        error!("configuring state subscriber as nonblocking: {err}");
                        return Ok(());
                    }
                    _ = self
                        .events
                        .send(Event::StateSubscribe {
                            stream: Arc::new(Mutex::new(clone)),
                        })
                        .inspect_err(|err| {
                            error!("registering state subscriber: {err}");
                        });
                }
                Err(err) => error!("cloning subscriber stream: {err}"),
            }
            return Ok(());
        }

        if argv_ref.first() == Some(&"exec-cmd") {
            let response = match parse_command(&argv_ref[1..])
                .and_then(|command| self.events.send(Event::Command { command }))
            {
                Ok(()) => CommandResponse {
                    ok: true,
                    error: None,
                },
                Err(err) => CommandResponse {
                    ok: false,
                    error: Some(err.to_string()),
                },
            };
            let response = serde_json::to_string(&response)?;
            stream.write_all(response.as_bytes())?;
            stream.write_all(b"\n")?;
            return Ok(());
        }

        if let Ok(command) = parse_command(&argv_ref).inspect_err(|err| error!("parsing command: {err}")) {
            _ = self
                .events
                .send(Event::Command { command })
                .inspect_err(|err| {
                    error!("sending command: {err}");
                });
        }
        Ok(())
    }
}

fn parse_query_request(argv: &[&str]) -> Option<StateQueryKind> {
    match argv {
        ["query", "state", "--json"] | ["query", "state"] => Some(StateQueryKind::State),
        ["query", "virtual-workspaces", "--json"] | ["query", "virtual-workspaces"] => {
            Some(StateQueryKind::VirtualWorkspaces)
        }
        ["query", "active", "--json"] | ["query", "active"] => Some(StateQueryKind::Active),
        _ => None,
    }
}

fn is_subscribe_request(argv: &[&str]) -> bool {
    matches!(argv, ["subscribe", "--json"] | ["subscribe"])
}

fn full_read(stream: &mut UnixStream, expected: usize, buffer: &mut [u8]) -> bool {
    if let Ok(count) = stream.read(buffer).inspect_err(|err| {
        error!("{err}");
    }) && count == expected
    {
        true
    } else {
        error!("short read, expected {expected}.");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, Direction, Operation};
    use std::sync::mpsc::TryRecvError;

    fn send_request(stream: &mut UnixStream, params: &[&str]) {
        let output = params
            .iter()
            .flat_map(|param| [param.as_bytes(), &[0]].concat())
            .collect::<Vec<_>>();
        let size: u32 = output.len().try_into().unwrap();
        stream.write_all(&size.to_le_bytes()).unwrap();
        stream.write_all(&output).unwrap();
    }

    #[test]
    fn execute_command_queues_setwidth_and_acknowledges() {
        let (events, receiver) = EventSender::new();
        let reader = CommandReader::new(events);
        let (mut client, server) = UnixStream::pair().unwrap();
        send_request(&mut client, &["exec-cmd", "window", "setwidth", "0.5"]);

        reader.handle_connection(server).unwrap();

        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert_eq!(response, "{\"ok\":true}\n");
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::Command {
                command: Command::Window(Operation::SetWidth(ratio))
            }) if (ratio - 0.5).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn execute_command_rejects_invalid_operation_without_queueing() {
        let (events, receiver) = EventSender::new();
        let reader = CommandReader::new(events);
        let (mut client, server) = UnixStream::pair().unwrap();
        send_request(&mut client, &["exec-cmd", "window", "invalid"]);

        reader.handle_connection(server).unwrap();

        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        let response: CommandResponse = serde_json::from_str(response.trim()).unwrap();
        assert!(!response.ok);
        assert!(response.error.is_some());
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn legacy_command_queues_without_response() {
        let (events, receiver) = EventSender::new();
        let reader = CommandReader::new(events);
        let (mut client, server) = UnixStream::pair().unwrap();
        send_request(&mut client, &["window", "focus", "east"]);

        reader.handle_connection(server).unwrap();

        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.is_empty());
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::Command {
                command: Command::Window(Operation::Focus(Direction::East))
            })
        ));
    }
}
