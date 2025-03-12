export GST_PLUGIN_PATH=../target/debug
export IROH_SECRET=89e9ec92fce4cf891e3afea46d4e4e2f3c838d022584372006fa500fb364018a
# node id: cba308bf093e49fd8643a965db43103f85c8249721acb009a8a01396288ef004

PEER=4cba5c51e0fe2c5805bb6de6fd8a0ecf431c46634a3fa5a8c76b59e3105146e2

gst-launch-1.0 -v\
	videotestsrc is-live=true pattern=ball !\
	x264enc tune=zerolatency bitrate=500 speed-preset=superfast !\
    rtph264pay !\
    irohrtpsink peer=$PEER

# gst-launch-1.0 \
# 	videotestsrc is-live=true pattern=ball !\
# 	videoconvert ! \
# 	queue !\
# 	vp8enc deadline=1 ! rtpvp8pay pt=96 ssrc=2 !\
# 	queue !\
# 	application/x-rtp,media=video,encoding-name=VP8,payload=96 !\
#         rtpbin !\
# 	irohrtpsink peer=$PEER
