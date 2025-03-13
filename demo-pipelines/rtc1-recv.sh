# Receive two streams from a remote via Iroh:
# - one with Opus audio
# - one with VP8 video
# and render them both onto the auto sinks.

export GST_PLUGIN_PATH=../target/debug

# We export a secret key that the Iroh endpoint will use. This gives our
# endpoint a persistent identity. The node id corresponding to the secret key
# is printed when starting thepipeline.
# node id: 4cba5c51e0fe2c5805bb6de6fd8a0ecf431c46634a3fa5a8c76b59e3105146e2
export IROH_SECRET=91cfc65d95a39a85a87d4deb698fccf0821de7deab79529add1c103070f16195
PEER=cba308bf093e49fd8643a965db43103f85c8249721acb009a8a01396288ef004

# cargo run --example launch -- \
gst-launch-1.0 \
    irohrtpsrc peer=$PEER flow-id=0 !\
        "application/x-rtp, media=audio, clock-rate=48000, encoding-name=OPUS, payload=96" !\
        decodebin3 !\
        autoaudiosink \
    \
    irohrtpsrc peer=$PEER flow-id=1 !\
        "application/x-rtp, media=video, clock-rate=90000, encoding-name=VP8" !\
        decodebin3 !\
        autovideosink sync=false \
