// Copyright (C) 2024, Asymptotic Inc.
//      Author: Sanchayan Maity <sanchayan@asymptotic.io>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use async_channel::RecvError;
use bytes::Bytes;
use gst::{glib, prelude::*, subclass::prelude::*};
use gst_base::prelude::*;
use gst_base::subclass::base_src::CreateSuccess;
use gst_base::subclass::prelude::*;
use iroh::NodeId;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};

use crate::common::GlobalState;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "irohrtpsrc",
        gst::DebugColorFlags::empty(),
        Some("Iroh RTP Source"),
    )
});

struct Started {
    receiver: async_channel::Receiver<Bytes>,
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
    caps: gst::Caps,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            peer: Default::default(),
            flow_id: 0,
            caps: gst::Caps::new_any(),
        }
    }
}

pub struct IrohRtpSrc {
    settings: Mutex<Settings>,
    state: Mutex<State>,
    // canceller: Mutex<utils::Canceller>,
}

impl Default for IrohRtpSrc {
    fn default() -> Self {
        Self {
            settings: Mutex::new(Settings::default()),
            state: Mutex::new(State::default()),
            // canceller: Mutex::new(utils::Canceller::default()),
        }
    }
}

impl GstObjectImpl for IrohRtpSrc {}

impl ElementImpl for IrohRtpSrc {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Iroh RTP source",
                "Source/Network/QUIC",
                "Receive data over the network via Iroh",
                "Frando <franz@n0.computer>",
            )
        });
        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let caps = gst::Caps::builder("application/x-rtp").build();
            let src_pad_template = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &caps,
            )
            .unwrap();

            vec![src_pad_template]
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

impl ObjectImpl for IrohRtpSrc {
    fn constructed(&self) {
        self.parent_constructed();
        self.obj().set_format(gst::Format::Time);
        self.obj().set_do_timestamp(true);
        self.obj().set_live(true);
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
                glib::ParamSpecBoxed::builder::<gst::Caps>("caps")
                    .nick("caps")
                    .blurb("The caps of the source pad")
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
            "caps" => {
                settings.caps = value
                    .get::<Option<gst::Caps>>()
                    .expect("type checked upstream")
                    .unwrap_or_else(gst::Caps::new_any);

                let srcpad = self.obj().static_pad("src").expect("source pad expected");
                srcpad.mark_reconfigure();
            }
            name => {
                println!("{name} is unimplemented")
            }
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let settings = self.settings.lock().unwrap();

        match pspec.name() {
            "peer" => settings.peer.clone().to_value(),
            "flow-id" => settings.flow_id.to_value(),
            "caps" => settings.caps.to_value(),
            _ => unimplemented!(),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for IrohRtpSrc {
    const NAME: &'static str = "GstIrohRtpSrc";
    type Type = super::IrohRtpSrc;
    type ParentType = gst_base::PushSrc;
}

impl BaseSrcImpl for IrohRtpSrc {
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
        let receiver = GlobalState::get()
            .receive_flow(node_id, flow_id)
            .map_err(|err| {
                gst::error_msg!(
                    gst::ResourceError::Failed,
                    ["failed to init recv eflow: {}", err]
                )
            })?;
        *state = State::Started(Started { receiver });
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        gst::info!(CAT, imp = self, "Stopping");

        let mut state = self.state.lock().unwrap();
        *state = State::Stopped;
        gst::info!(CAT, imp = self, "Stopped");

        Ok(())
    }

    // fn unlock(&self) -> Result<(), gst::ErrorMessage> {
    //     let mut canceller = self.canceller.lock().unwrap();
    //     canceller.abort();
    //     Ok(())
    // }

    // fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
    //     let mut canceller = self.canceller.lock().unwrap();
    //     if matches!(&*canceller, Canceller::Cancelled) {
    //         *canceller = Canceller::None;
    //     }
    //     Ok(())
    // }

    fn caps(&self, filter: Option<&gst::Caps>) -> Option<gst::Caps> {
        let settings = self.settings.lock().unwrap();
        let mut tmp_caps = settings.caps.clone();
        gst::debug!(CAT, imp = self, "Advertising our own caps: {:?}", &tmp_caps);

        if let Some(filter_caps) = filter {
            gst::debug!(
                CAT,
                imp = self,
                "Intersecting with filter caps: {:?}",
                &filter_caps
            );

            tmp_caps = filter_caps.intersect_with_mode(&tmp_caps, gst::CapsIntersectMode::First);
        };

        gst::debug!(CAT, imp = self, "Returning caps: {:?}", &tmp_caps);
        Some(tmp_caps)
    }

    fn is_seekable(&self) -> bool {
        false
    }
}

impl PushSrcImpl for IrohRtpSrc {
    fn create(
        &self,
        _buffer: Option<&mut gst::BufferRef>,
    ) -> Result<CreateSuccess, gst::FlowError> {
        let state = self.state.lock().unwrap();
        let receiver = match *state {
            State::Started(ref started) => started.receiver.clone(),
            State::Stopped => {
                gst::error!(
                    CAT,
                    imp = self,
                    "Could not get buffer from source pad: not started"
                );
                return Err(gst::FlowError::Error);
            }
        };
        let bytes = match receiver.recv_blocking() {
            Ok(bytes) => bytes,
            Err(RecvError) => {
                gst::error!(
                    CAT,
                    imp = self,
                    "Could not get buffer from source pad: connection lost"
                );
                return Err(gst::FlowError::Error);
            }
        };
        gst::trace!(CAT, imp = self, "Pushing buffer of {} bytes", bytes.len());

        let buffer = gst::Buffer::from_slice(bytes);
        Ok(CreateSuccess::NewBuffer(buffer.to_owned()))
    }
}
