# Send two streams over iroh:
# - Default microphone encoded with Opus
# - Default camera encoded with VP8

export GST_PLUGIN_PATH=../target/debug

# We export a secret key that the Iroh endpoint will use. This gives our
# endpoint a persistent identity. The node id corresponding to the secret key
# is printed when starting thepipeline.
# node id: cba308bf093e49fd8643a965db43103f85c8249721acb009a8a01396288ef004
export IROH_SECRET=89e9ec92fce4cf891e3afea46d4e4e2f3c838d022584372006fa500fb364018a
PEER=4cba5c51e0fe2c5805bb6de6fd8a0ecf431c46634a3fa5a8c76b59e3105146e2

gst-launch-1.0 \
    autoaudiosrc !\
        queue ! audioconvert ! audioresample !\
        opusenc bitrate=64000 ! rtpopuspay !\
        irohrtpsink peer=$PEER flow-id=0 \
    \
    v4l2src ! \
        queue ! videoconvert !\
        vp8enc deadline=1 ! rtpvp8pay mtu=1000 ! \
        irohrtpsink peer=$PEER flow-id=1  \
