use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::LazyLock,
};

use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use bytes::{Bytes, BytesMut};
use iroh::{
    endpoint::{Connection, Incoming},
    Endpoint, NodeId, SecretKey,
};
use iroh_roq::{rtp::packet::Packet, Session, ALPN};
use tokio::task::JoinSet;
use tracing::{debug, error_span, info, warn, Instrument};
use webrtc_util::{
    marshal::{Marshal, MarshalSize},
    Unmarshal,
};

use crate::util::RUNTIME;

pub(crate) const DEFAULT_TIMEOUT: u32 = 15;

pub struct Started {
    cmd_tx: Sender<Command>,
}

pub enum GlobalState {
    Started(Started),
    Failed(anyhow::Error),
}

enum Command {
    Flow(FlowCommand),
    Shutdown { reply: Sender<()> },
}

#[derive(derive_more::Debug)]
enum FlowCommand {
    #[debug("ReceiveFlow({}, {})", node_id.fmt_short(), flow_id)]
    ReceiveFlow {
        node_id: NodeId,
        flow_id: u32,
        reply: Sender<Result<Receiver<Result<Bytes>>>>,
    },
    #[debug("SendFlow({}, {})", node_id.fmt_short(), flow_id)]
    SendFlow {
        node_id: NodeId,
        flow_id: u32,
        reply: Sender<Result<SendFlowReply>>,
    },
}

pub struct SendFlowReply {
    pub data_sender: Sender<Bytes>,
    pub error_receiver: Receiver<anyhow::Error>,
}

impl FlowCommand {
    fn node_id(&self) -> NodeId {
        match self {
            Self::ReceiveFlow { node_id, .. } => *node_id,
            Self::SendFlow { node_id, .. } => *node_id,
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
        let (init_tx, init_rx) = async_channel::bounded(1);
        if let Err(err) = tracing_subscriber::fmt::try_init() {
            println!("failed to init tracing subscriber: {err}");
        }
        std::thread::spawn(|| {
            // let runtime = match tokio::runtime::Builder::new_multi_thread()
            //     .enable_all()
            //     .build()
            // {
            //     Ok(runtime) => runtime,
            //     Err(err) => {
            //         let res = Err(err).context("failed to start tokio runtime");
            //         init_tx
            //             .send_blocking(res)
            //             .expect("global state init receiver dropped unexpectedly");
            //         return;
            //     }
            // };
            RUNTIME.block_on(async move {
                match State::new(cmd_rx).await {
                    Ok(state) => {
                        init_tx.send(Ok(())).await.unwrap();
                        state.run().await
                    }
                    Err(err) => {
                        let res = Err(err).context("failed to bind iroh endpoint");
                        init_tx
                            .send(res)
                            .await
                            .expect("global state init receiver dropped unexpectedly");
                    }
                }
            })
        });
        let init_res = init_rx
            .recv_blocking()
            .expect("global state thread panicked unexpectedly");
        match init_res {
            Ok(()) => GlobalState::Started(Started { cmd_tx }),
            Err(err) => GlobalState::Failed(err),
        }
    }

    pub fn get() -> Result<&'static Started, &'static anyhow::Error> {
        match &*GLOBAL_STATE {
            GlobalState::Started(started) => Ok(started),
            GlobalState::Failed(error) => Err(error),
        }
    }
}

impl Started {
    pub fn send_flow(&self, remote_node_id: NodeId, flow_id: u32) -> Result<SendFlowReply> {
        let (reply, reply_rx) = async_channel::bounded(1);
        let cmd = Command::Flow(FlowCommand::SendFlow {
            node_id: remote_node_id,
            flow_id,
            reply,
        });
        self.cmd_tx.send_blocking(cmd)?;
        let res = reply_rx.recv_blocking()?;
        res
    }

    pub fn receive_flow(
        &self,
        remote_node_id: NodeId,
        flow_id: u32,
    ) -> Result<Receiver<Result<Bytes>>> {
        let (reply, reply_rx) = async_channel::bounded(1);
        let cmd = Command::Flow(FlowCommand::ReceiveFlow {
            node_id: remote_node_id,
            flow_id,
            reply,
        });
        self.cmd_tx.send_blocking(cmd)?;
        let res = reply_rx.recv_blocking()?;
        res
    }

    pub fn shutdown(&self) -> Result<()> {
        let (reply, reply_rx) = async_channel::bounded(1);
        let cmd = Command::Shutdown { reply };
        self.cmd_tx.send_blocking(cmd)?;
        reply_rx.recv_blocking()?;
        Ok(())
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        if let Err(err) = self.shutdown() {
            println!("failure at endpoint shutdown: {err}");
        } else {
            println!("endpoint shutdown");
        }
    }
}

struct State {
    endpoint: Endpoint,
    pending_commands: HashMap<NodeId, Vec<FlowCommand>>,
    connecting_tasks: JoinSet<(NodeId, Result<Connection>)>,

    connecting: HashSet<NodeId>,
    incoming: HashMap<NodeId, Connection>,
    sessions: HashMap<NodeId, Session>,
    // peers: HashMap<NodeId, PeerState>,
    cmd_rx: Receiver<Command>,
}

// enum PeerState {
//     Connecting,
//     Incoming(Connection),
//     Active(Session),
//     // Failed,
// }

impl State {
    async fn new(cmd_rx: Receiver<Command>) -> Result<Self> {
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
        tracing::info!("endpoint bound: {}", endpoint.node_id());
        Ok(State {
            endpoint: endpoint.clone(),
            pending_commands: Default::default(),
            connecting_tasks: Default::default(),
            connecting: Default::default(),
            incoming: Default::default(),
            sessions: Default::default(),
            // peers: Default::default(),
            cmd_rx,
        })
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Ok(cmd) => {
                            match cmd {
                                Command::Flow(flow_command) => {
                                    self.on_command(flow_command).await;
                                }
                                Command::Shutdown { reply } => {
                                    self.endpoint.close().await;
                                    reply.send(()).await.ok();
                                    break;
                                }
                            }
                        }
                        Err(_err) => break
                    }
                }
                incoming = self.endpoint.accept() => {
                    match incoming {
                        None => break,
                        Some(incoming) => self.on_incoming(incoming).await
                    }
                }
                Some(res) = self.connecting_tasks.join_next(), if !self.connecting_tasks.is_empty() => {
                    let (node_id, conn) = res.expect("connect task panicked");
                    self.on_connected(node_id, conn).await;
                }
            }
        }
    }

    async fn on_command(&mut self, command: FlowCommand) {
        let node_id = command.node_id();

        if let Some(session) = self.sessions.get(&node_id) {
            debug!(?command, "on_command: handle");
            handle_flow_command(session.clone(), command).await
        } else {
            debug!(?command, "on_command: put on hold");
            self.pending_commands
                .entry(node_id)
                .or_default()
                .push(command);

            if let Some(conn) = self.incoming.remove(&node_id) {
                self.activate_conn(node_id, conn).await;
            } else if !self.connecting.contains(&node_id) {
                info!(remote=%node_id.fmt_short(), "start connecting");
                let endpoint = self.endpoint.clone();
                self.connecting.insert(node_id);
                self.connecting_tasks.spawn(async move {
                    let fut = async {
                        let conn = endpoint.connect(node_id, ALPN).await?;
                        let mut stream = conn.accept_uni().await?;
                        let buf = stream.read_to_end(2).await?;
                        if &buf != b"hi" {
                            anyhow::bail!("unexpected initialization bytes");
                        }
                        anyhow::Ok(conn)
                    };
                    (node_id, fut.await)
                });
            }
        }
    }

    async fn on_connected(&mut self, node_id: NodeId, conn: Result<Connection>) {
        self.connecting.remove(&node_id);
        let conn = match conn {
            Ok(conn) => {
                info!(remote=%node_id.fmt_short(), "connected");
                conn
            }
            Err(err) => {
                info!(remote=%node_id.fmt_short(), ?err, "connecting failed");
                return;
            }
        };
        if self.sessions.contains_key(&node_id) {
            info!(remote=%node_id.fmt_short(), "abort duplicate connection: already active");
            conn.close(1u32.into(), b"already-connected");
        } else {
            self.activate_conn(node_id, conn).await;
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
        debug!("on_incoming node_id {}", node_id.fmt_short());

        if self.connecting.contains(&node_id) && node_id < self.endpoint.node_id() {
            debug!(remote=%node_id.fmt_short(), "incoming connection: decline, prefer our dial");
            conn.close(2u32.into(), b"prefer-mine");
            return;
        } else {
            let fut = async {
                let mut stream = conn.open_uni().await?;
                stream.write_all(b"hi").await?;
                anyhow::Ok(())
            };
            if let Err(err) = fut.await {
                warn!("failed to initialize incoming connection: {err}");
                conn.close(3u32.into(), b"init-failure");
                return;
            }
            debug!(remote=%node_id.fmt_short(), "incoming connection: accept");
        }

        if self.sessions.contains_key(&node_id) {
            info!(
                remote = %node_id.fmt_short(),
                "incoming connection: session already exists, abort"
            );
            conn.close(1u32.into(), b"already-connected");
        } else if self.pending_commands.contains_key(&node_id) {
            info!(
                remote = %node_id.fmt_short(),
                "incoming connection: pending commands, activate"
            );
            self.activate_conn(node_id, conn).await;
        } else {
            info!(
                remote = &node_id.fmt_short(),
                "incoming connection: no pending commands, put on hold"
            );
            self.incoming.insert(node_id, conn);
        }
    }

    async fn activate_conn(&mut self, node_id: NodeId, conn: Connection) {
        info!(remote=%node_id.fmt_short(), "activate connection");
        let session = iroh_roq::Session::new(conn);
        self.sessions.insert(node_id, session.clone());
        if let Some(cmds) = self.pending_commands.remove(&node_id) {
            for cmd in cmds {
                handle_flow_command(session.clone(), cmd).await;
            }
        }
    }
}

async fn handle_flow_command(session: Session, command: FlowCommand) {
    info!("handle flow command: {command:?}");
    match command {
        FlowCommand::ReceiveFlow {
            flow_id,
            reply,
            node_id,
        } => {
            let (sender, receiver) = async_channel::bounded(16);
            tokio::task::spawn(
                async move {
                    if let Err(err) = receive_flow_task(session, flow_id, &sender).await {
                        warn!(?err, "receive flow failed");
                        if let Err(_err) = sender.send(Err(err)).await {
                            warn!("failed to forward receive flow error to gst");
                        }
                    }
                }
                .instrument(error_span!("recv_flow", remote=%node_id.fmt_short(), %flow_id)),
            );
            let _ = reply.send(Ok(receiver)).await;
        }
        FlowCommand::SendFlow {
            flow_id,
            reply,
            node_id,
        } => {
            let (data_sender, data_receiver) = async_channel::bounded(16);
            let (error_sender, error_receiver) = async_channel::bounded(1);
            let res = SendFlowReply {
                data_sender,
                error_receiver,
            };
            tokio::task::spawn(
                async move {
                    if let Err(err) = send_flow_task(session, flow_id, data_receiver).await {
                        warn!(?err, "send flow failed");
                        if let Err(_err) = error_sender.send(err).await {
                            warn!("failed to forward send flow error to gst");
                        }
                    }
                }
                .instrument(error_span!("send_flow", remote=%node_id.fmt_short(), %flow_id)),
            );
            let _ = reply.send(Ok(res)).await;
        }
    }
}

async fn receive_flow_task(
    session: Session,
    flow_id: u32,
    sender: &Sender<Result<Bytes>>,
) -> Result<()> {
    let mut flow = session
        .new_receive_flow(flow_id.into())
        .await
        .context("failed to start receive flow")?;
    info!("receive flow initialized");
    loop {
        let packet = flow.read_rtp().await?;
        let mut buf = BytesMut::new();
        let marshal_size = packet.marshal_size();
        buf.resize(marshal_size, 0);
        packet
            .marshal_to(&mut buf[..])
            .context("failed to remarshal packet")?;
        if let Err(_) = sender.send(Ok(buf.freeze())).await {
            info!("break receive flow: channel receiver dropped");
            break Ok(());
        }
    }
}

async fn send_flow_task(session: Session, flow_id: u32, receiver: Receiver<Bytes>) -> Result<()> {
    let flow = session
        .new_send_flow(flow_id.into())
        .await
        .context("failed to start send flow")?;
    info!("send flow initialized");
    while let Ok(mut bytes) = receiver.recv().await {
        let packet = match Packet::unmarshal(&mut bytes) {
            Ok(packet) => packet,
            Err(err) => {
                warn!("failed to unmarshal packet for sending: {err}");
                continue;
            }
        };
        flow.send_rtp(&packet)?;
    }
    Ok(())
}
