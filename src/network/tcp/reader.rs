#![allow(dead_code)]
#![allow(unreachable_code)]
extern crate mio;

use network::tcp::{TcpReaderConn, TcpNetworkCommand, TcpNetworkCMD};
use std::io::Result;
use self::mio::{Poll, Token, Ready, PollOpt, Events};
use self::mio::channel::{Receiver, Sender, channel};
use self::mio::tcp::TcpStream;
use std::sync::Arc;
use std::collections::BTreeMap;

/// Read buffer size 64KB
const READER_READ_BUFFER_SIZE: usize = 65000;
const READER_CHANNEL_TOKEN: Token = Token(1);

pub enum TcpReaderCMD {
    HandleNewConnection,
    CloseConnection,
    SendData,
}

pub struct TcpReaderCommand {
    // base command code
    code: TcpReaderCMD,
    // socket vector for transfering new connection
    socket: Vec<TcpStream>,
    token: Vec<Token>,
    data: Vec<Arc<Vec<u8>>>
}

pub struct TcpReader {
    // connections transferred to this reader for IO operations
    connections: Vec<TcpReaderConn>,
    // map for keeping vector keys based on connections
    // beacuse we are getting events based on connection keys
    connection_keys: BTreeMap<Token, usize>,

    // buffers for making one time allocations per read process
    data_len_buf: Vec<u8>,
    data_chunk: Vec<u8>,

    // base event loop handler
    poll: Poll,

    // chanel sender, receiver for keeping communication with loop
    channel_sender: Sender<TcpReaderCommand>,
    channel_receiver: Receiver<TcpReaderCommand>,

    // channel for sending commands to TcpNetwork main loop
    channel_tcp_net: Sender<TcpNetworkCommand>
}

impl TcpReader {
    /// creating new TcpReader with default values
    pub fn new(tcp_net_chan: Sender<TcpNetworkCommand>) -> TcpReader {
        let (s, r)= channel::<TcpReaderCommand>();
        TcpReader {
            connections: Vec::new(),
            data_len_buf: vec![0; 4],
            data_chunk: vec![0; READER_READ_BUFFER_SIZE],
            poll: Poll::new().unwrap(),
            channel_sender: s,
            channel_receiver: r,
            channel_tcp_net: tcp_net_chan,
            connection_keys: BTreeMap::new()
        }
    }

    /// Clonning channel for sending commands
    pub fn channel(&self) -> Sender<TcpReaderCommand> {
        self.channel_sender.clone()
    }

    /// Private function for handling Reader commands
    #[inline(always)]
    fn notify(&mut self, cmd: &mut TcpReaderCommand) {
        match cmd.code {
            TcpReaderCMD::HandleNewConnection => {
                // Handling new connection with given socket
                // if it exists in Vector of sockets
                while !cmd.socket.is_empty() && !cmd.token.is_empty() {
                    let sock = match cmd.socket.pop() {
                        Some(sock) => sock,
                        None => return
                    };

                    let token = match cmd.token.pop() {
                        Some(t) => t,
                        None => return
                    };

                    self.connections.push(TcpReaderConn::new(sock, token));
                    // keeping index of connection inside map
                    self.connection_keys.insert(token, self.connections.len() - 1);
                }
            }

            TcpReaderCMD::CloseConnection => {
                // Closing connection by given token
                while !cmd.token.is_empty() {
                    let token = match cmd.token.pop() {
                        Some(t) => t,
                        _ => return
                    };

                    self.close_connection(token, false);
                }
            }

            TcpReaderCMD::SendData => {
                // if data is empty just returning
                if cmd.data.len() == 0 {
                    return;
                }

                // Closing connection by given token
                while !cmd.token.is_empty() {
                    let token = match cmd.token.pop() {
                        Some(t) => t,
                        _ => return
                    };

                    // if we have this connection
                    // adding sent data to our queue for writing
                    // and making connection writable
                    if !self.connection_keys.contains_key(&token) {
                        continue;
                    }
                    
                    let i = self.connection_keys[&token];
                    self.connections[i].write_queue.append(&mut cmd.data);
                    self.make_writable(&self.connections[i]);
                }
            }
        }
    }

    /// running TcpReader loop
    /// this will exit when loop is no longer running
    pub fn run(&mut self) -> Result<()> {
        // registering receiver for poll loop
        match self.poll.register(&self.channel_receiver, READER_CHANNEL_TOKEN, Ready::readable(), PollOpt::edge()) {
            Ok(_) => {},
            Err(e) => return Err(e)
        }

        let mut events: Events = Events::with_capacity(1000);

        loop {
            // using unwrap here because if it is failing anyway process should be closed
            let event_count = self.poll.poll(&mut events, None).unwrap();
            if event_count == 0 {
                continue
            }

            for event in events.into_iter() {
                let token = event.token();
                if token == READER_CHANNEL_TOKEN {
                    match self.channel_receiver.try_recv() {
                        Ok(cmd) => {
                            let mut c = cmd;
                            self.notify(&mut c);
                        }
                        Err(_) => {}
                    }
                    continue;
                }

                let kind = event.kind();

                if kind == Ready::error() || kind == Ready::hup() {
                    self.close_connection(token, true);
                    continue;
                }

                if kind == Ready::readable() {
                    self.readable(token);
                    continue;
                }

                if kind == Ready::writable() {
                    self.writable(token);
                    continue;
                }
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn make_writable(&self, conn: &TcpReaderConn) {
        let mut r = Ready::readable();
        r.insert(Ready::writable());
        let _ = self.poll.reregister(
            &conn.socket, conn.token, r,
            PollOpt::edge() | PollOpt::oneshot()
        );
    }

    #[inline(always)]
    fn close_connection(&mut self, token: Token, send_data_event: bool) {
        // if we have this connection
        // just removing it from our list
        // after removing it will be automatically deatached from loop
        if !self.connection_keys.contains_key(&token) {
            return;
        }

        let i = self.connection_keys[&token];

        self.connections.remove(i);
        self.connection_keys.remove(&token);

        // do we need to send event about connection close to
        // connection handler loop or not
        if send_data_event {
            let _ = self.channel_tcp_net.send(TcpNetworkCommand {
                cmd: TcpNetworkCMD::ConnectionClosed,
                token: token,
                data: Vec::new()
            });
        }
    }

    #[inline(always)]
    fn readable(&mut self, token: Token) {
        if !self.connection_keys.contains_key(&token) {
            return;
        }

        let i = self.connection_keys[&token];

        let mut total_data: Vec<Arc<Vec<u8>>> = Vec::new();
        loop {
            let (rd, completed) = match self.connections[i].read_data(&mut self.data_len_buf, &mut self.data_chunk) {
                Ok(r) => r,
                Err(_) => {
                    // if we got error we need to close connection
                    self.close_connection(token, true);
                    return;
                }
            };

            // if we got some comlete data based on our API
            // saving it for transfering to Networking loop
            if rd.len() > 0 {
                total_data.push(Arc::new(rd));
            }

            // if we completed read process, just breaking the loop
            if !completed {
                break;
            }
        }

        let _ = self.channel_tcp_net.send(TcpNetworkCommand {
            cmd: TcpNetworkCMD::HandleNewData,
            token: token,
            data: total_data
        });
    }

    #[inline(always)]
    fn writable(&mut self, token: Token) {

    }
}
