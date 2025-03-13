// Copyright (C) 2024, Asymptotic Inc.
//      Author: Sanchayan Maity <sanchayan@asymptotic.io>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use bytes::Bytes;
use gst::{glib, prelude::*, subclass::prelude::*};
use gst_base::subclass::prelude::*;
use iroh::NodeId;
use std::str::FromStr;
use std::sync::LazyLock;
use std::sync::Mutex;

use crate::common::GlobalState;
use crate::common::SendFlowReply;
use crate::gst_err;
use crate::util::Canceller;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "irohrtpsink",
        gst::DebugColorFlags::empty(),
        Some("Iroh RTP Sink"),
    )
});

struct Started {
    data_sender: async_channel::Sender<Bytes>,
    error_receiver: async_channel::Receiver<anyhow::Error>,
}

#[derive(Default)]
enum State {
    #[default]
    Stopped,
    Started(Started),
}

#[derive(Debug)]
struct Settings {
    peer: String,
    flow_id: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            peer: Default::default(),
            flow_id: 0,
        }
    }
}

pub struct IrohRtpSink {
    settings: Mutex<Settings>,
    state: Mutex<State>,
    canceller: Mutex<Canceller>,
}

impl Default for IrohRtpSink {
    fn default() -> Self {
        Self {
            settings: Mutex::new(Settings::default()),
            state: Mutex::new(State::default()),
            canceller: Mutex::new(Canceller::default()),
        }
    }
}

impl GstObjectImpl for IrohRtpSink {}

impl ElementImpl for IrohRtpSink {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Iroh RTP sink",
                "Source/Network/QUIC",
                "Send data over the network via Iroh",
                "Frando <franz@n0.computer>",
            )
        });
        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let caps = gst::Caps::builder("application/x-rtp").build();
            let sink_pad_template = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &caps,
            )
            .unwrap();

            vec![sink_pad_template]
        });

        PAD_TEMPLATES.as_ref()
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        self.parent_change_state(transition)
    }
}

impl ObjectImpl for IrohRtpSink {
    fn constructed(&self) {
        self.parent_constructed();
    }

    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                glib::ParamSpecString::builder("peer")
                    .nick("Peer node id")
                    .blurb("Node id of peer to connect with")
                    .readwrite()
                    .build(),
                glib::ParamSpecUInt::builder("flow-id")
                    .nick("Flow id")
                    .blurb("Flow id to receive data on")
                    .readwrite()
                    .build(),
            ]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        let mut settings = self.settings.lock().unwrap();

        match pspec.name() {
            "peer" => {
                let value = value.get::<String>().expect("type checked upstream");
                settings.peer = value;
            }
            "flow-id" => {
                settings.flow_id = value.get().expect("type checked upstream");
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let settings = self.settings.lock().unwrap();

        match pspec.name() {
            "peer" => settings.peer.clone().to_value(),
            "flow-id" => settings.flow_id.to_value(),
            _ => unimplemented!(),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for IrohRtpSink {
    const NAME: &'static str = "IrohRtpSink";
    type Type = super::IrohRtpSink;
    type ParentType = gst_base::BaseSink;
}

impl BaseSinkImpl for IrohRtpSink {
    fn start(&self) -> Result<(), gst::ErrorMessage> {
        let settings = self.settings.lock().unwrap();
        let node_id = NodeId::from_str(&settings.peer).map_err(|err| {
            gst::error_msg!(
                gst::ResourceError::Failed,
                ["missing or invalid peer node id: {}", err]
            )
        })?;
        let flow_id = settings.flow_id;
        drop(settings);

        let mut state = self.state.lock().unwrap();
        if let State::Started { .. } = *state {
            unreachable!("IrohRtpSrc already started");
        }
        let SendFlowReply {
            data_sender,
            error_receiver,
        } = GlobalState::get()
            .map_err(gst_err!())?
            .send_flow(node_id, flow_id)
            .map_err(gst_err!())?;
        *state = State::Started(Started {
            data_sender,
            error_receiver,
        });
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        gst::info!(CAT, imp = self, "Stopping");

        let mut state = self.state.lock().unwrap();
        *state = State::Stopped;
        gst::info!(CAT, imp = self, "Stopped");
        Ok(())
    }

    fn render(&self, buffer: &gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
        let state = self.state.lock().unwrap();
        let sender = match *state {
            State::Started(ref started) => {
                match started.error_receiver.try_recv() {
                    Ok(err) => {
                        gst::element_imp_error!(
                            self,
                            gst::CoreError::Failed,
                            ["Send flow failed: {:?}", err]
                        );
                        return Err(gst::FlowError::Error);
                    }
                    Err(async_channel::TryRecvError::Empty) => {}
                    Err(async_channel::TryRecvError::Closed) => {}
                }
                started.data_sender.clone()
            }
            State::Stopped => {
                gst::element_imp_error!(self, gst::CoreError::Failed, ["Not started yet"]);
                return Err(gst::FlowError::Error);
            }
        };
        drop(state);

        gst::trace!(CAT, imp = self, "Rendering {:?}", buffer);

        let map = buffer.map_readable().map_err(|_| {
            gst::element_imp_error!(self, gst::CoreError::Failed, ["Failed to map buffer"]);
            gst::FlowError::Error
        })?;

        let buf = Bytes::from(map.to_vec());
        if let Err(_err) = sender.send_blocking(buf) {
            gst::element_imp_error!(
                self,
                gst::CoreError::Failed,
                ["Failed to send: connection lost"]
            );
            return Err(gst::FlowError::Error);
        }
        Ok(gst::FlowSuccess::Ok)
    }

    // fn query(&self, query: &mut gst::QueryRef) -> bool {
    //     match query.view_mut() {
    //         gst::QueryViewMut::Custom(q) => self.sink_query(q),
    //         _ => BaseSinkImplExt::parent_query(self, query),
    //     }
    // }

    fn unlock(&self) -> Result<(), gst::ErrorMessage> {
        let mut canceller = self.canceller.lock().unwrap();
        canceller.abort();
        Ok(())
    }

    fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
        let mut canceller = self.canceller.lock().unwrap();
        if matches!(&*canceller, Canceller::Cancelled) {
            *canceller = Canceller::None;
        }
        Ok(())
    }
}
