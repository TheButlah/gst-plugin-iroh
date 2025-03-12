use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::LazyLock,
};

use anyhow::Result;
use async_channel::{Receiver, Sender};
use bytes::{Bytes, BytesMut};
use iroh::{
    endpoint::{Connection, Incoming},
    Endpoint, NodeId, SecretKey,
};
use iroh_roq::{rtp::packet::Packet, Session, ALPN};
use tokio::task::JoinSet;
use webrtc_util::{
    marshal::{Marshal, MarshalSize},
    Unmarshal,
};

pub struct GlobalState {
    tx: Sender<Command>,
}

enum Command {
    ReceiveFlow {
        remote_node_id: NodeId,
        flow_id: u32,
        reply: Sender<Result<Receiver<Bytes>>>,
        // sender: Sender<Result<Bytes>>,
    },
    SendFlow {
        remote_node_id: NodeId,
        flow_id: u32,
        reply: Sender<Result<Sender<Bytes>>>,
        // receiver: Receiver<Result<Bytes>>,
    },
}

impl Command {
    fn node_id(&self) -> NodeId {
        match self {
            Self::ReceiveFlow { remote_node_id, .. } => *remote_node_id,
            Self::SendFlow { remote_node_id, .. } => *remote_node_id,
        }
    }

    // fn flow_id(&self) -> u32 {
    //     match self {
    //         Self::ReceiveFlow { flow_id, .. } => *flow_id,
    //         Self::SendFlow { flow_id, .. } => *flow_id,
    //     }
    // }
}

static GLOBAL_STATE: LazyLock<GlobalState> = LazyLock::new(|| GlobalState::init());

impl GlobalState {
    pub fn init() -> Self {
        let (cmd_tx, cmd_rx) = async_channel::bounded(16);
        std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime
                .block_on(async move { main_loop(cmd_rx).await })
                .unwrap();
        });
        Self { tx: cmd_tx }
    }
    pub fn get() -> &'static Self {
        &*GLOBAL_STATE
    }
    pub fn send_flow(&self, remote_node_id: NodeId, flow_id: u32) -> Result<Sender<Bytes>> {
        let (reply, reply_rx) = async_channel::bounded(1);
        let cmd = Command::SendFlow {
            remote_node_id,
            flow_id,
            reply,
        };
        self.tx.send_blocking(cmd)?;
        let res = reply_rx.recv_blocking()?;
        res
    }

    pub fn receive_flow(&self, remote_node_id: NodeId, flow_id: u32) -> Result<Receiver<Bytes>> {
        let (reply, reply_rx) = async_channel::bounded(1);
        let cmd = Command::ReceiveFlow {
            remote_node_id,
            flow_id,
            reply,
        };
        self.tx.send_blocking(cmd)?;
        let res = reply_rx.recv_blocking()?;
        res
    }
}

struct State {
    endpoint: Endpoint,
    incoming: HashMap<NodeId, Connection>,
    sessions: HashMap<NodeId, Session>,
    pending_commands: HashMap<NodeId, Vec<Command>>,
    connecting: HashSet<NodeId>,
    connecting_tasks: JoinSet<(NodeId, Result<Connection>)>,
}

async fn main_loop(cmd_rx: Receiver<Command>) -> Result<()> {
    let secret_key = match std::env::var("IROH_SECRET") {
        Ok(secret) => {
            SecretKey::from_str(&secret).expect("failed to parse secret key from IROH_SECRET")
        }
        Err(_) => SecretKey::generate(&mut rand::rngs::OsRng),
    };
    let endpoint = Endpoint::builder()
        .secret_key(secret_key)
        .discovery_n0()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    println!("IROH NODE ID: {}", endpoint.node_id());
    let mut state = State {
        endpoint: endpoint.clone(),
        incoming: Default::default(),
        sessions: Default::default(),
        pending_commands: Default::default(),
        connecting_tasks: Default::default(),
        connecting: Default::default(),
    };

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Ok(cmd) => state.on_command(cmd).await,
                    Err(_err) => break
                }
            }
            incoming = endpoint.accept() => {
                match incoming {
                    None => break,
                    Some(incoming) => state.on_incoming(incoming).await
                }
            }
            Some(res) = state.connecting_tasks.join_next(), if !state.connecting_tasks.is_empty() => {
                let (node_id, conn) = res.expect("connect task panicked");
                state.connecting.remove(&node_id);
                if let Ok(conn) = conn {
                    state.handle(node_id, conn).await;
                }
            }
        }
    }
    Ok(())
}
impl State {
    async fn on_command(&mut self, command: Command) {
        let node_id = command.node_id();
        if let Some(conn) = self.incoming.remove(&node_id) {
            self.handle(node_id, conn).await;
        } else if !self.sessions.contains_key(&node_id) && !self.connecting.contains(&node_id) {
            let endpoint = self.endpoint.clone();
            self.connecting.insert(node_id);
            self.connecting_tasks
                .spawn(async move { (node_id, endpoint.connect(node_id, ALPN).await) });
        }
        self.handle_command_inner(command).await;
    }

    async fn handle_command_inner(&mut self, command: Command) {
        let node_id = command.node_id();
        if let Some(session) = self.sessions.get(&node_id) {
            match command {
                Command::ReceiveFlow { flow_id, reply, .. } => {
                    let flow = session.new_receive_flow(flow_id.into()).await;
                    let Ok(mut flow) = flow else {
                        return;
                    };
                    let (sender, receiver) = async_channel::bounded(8);

                    tokio::task::spawn(async move {
                        while let Ok(packet) = flow.read_rtp().await {
                            let mut buf = BytesMut::new();
                            let marshal_size = packet.marshal_size();
                            buf.resize(marshal_size, 0);
                            if let Err(_err) = packet.marshal_to(&mut buf[..]) {
                                break;
                            }
                            if let Err(_) = sender.send(buf.freeze()).await {
                                break;
                            }
                            // let packet = iroh_roq::rtp::packet::
                        }
                    });
                    let _ = reply.send(Ok(receiver)).await;
                }
                Command::SendFlow { flow_id, reply, .. } => {
                    let flow = session.new_send_flow(flow_id.into()).await;
                    let Ok(flow) = flow else {
                        return;
                    };
                    let (sender, receiver) = async_channel::bounded(8);
                    tokio::task::spawn(async move {
                        while let Ok(mut bytes) = receiver.recv().await {
                            let Ok(packet) = Packet::unmarshal(&mut bytes) else {
                                continue;
                            };
                            if let Err(_err) = flow.send_rtp(&packet) {
                                break;
                            }
                        }
                    });
                    let _ = reply.send(Ok(sender)).await;
                }
            }
        } else {
            self.pending_commands
                .entry(node_id)
                .or_default()
                .push(command);
        }
    }

    async fn on_incoming(&mut self, incoming: Incoming) {
        let Ok(mut connecting) = incoming.accept() else {
            return;
        };
        let Ok(alpn) = connecting.alpn().await else {
            return;
        };
        if alpn != ALPN {
            return;
        }
        let Ok(conn) = connecting.await else {
            return;
        };
        let Ok(node_id) = conn.remote_node_id() else {
            return;
        };
        if self.pending_commands.contains_key(&node_id) {
            self.handle(node_id, conn).await;
        } else {
            self.incoming.insert(node_id, conn);
        }
    }

    async fn handle(&mut self, node_id: NodeId, conn: Connection) {
        let session = iroh_roq::Session::new(conn);
        self.sessions.insert(node_id, session);
        if let Some(cmds) = self.pending_commands.remove(&node_id) {
            for cmd in cmds {
                self.handle_command_inner(cmd).await;
            }
        }
    }
}
