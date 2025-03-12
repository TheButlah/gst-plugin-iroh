export GST_PLUGIN_PATH=../target/debug
export IROH_SECRET=91cfc65d95a39a85a87d4deb698fccf0821de7deab79529add1c103070f16195
# node id: 4cba5c51e0fe2c5805bb6de6fd8a0ecf431c46634a3fa5a8c76b59e3105146e2

PEER=cba308bf093e49fd8643a965db43103f85c8249721acb009a8a01396288ef004

gst-launch-1.0 -v\
	irohrtpsrc peer=$PEER !\
	"application/x-rtp, media=(string)video, clock-rate=(int)90000, encoding-name=(string)H264, payload=(int)96" !\
    rtph264depay ! h264parse ! decodebin ! videoconvert !\
    autovideosink sync=false

	# rtpbin !\
	# decodebin !\
	# queue !\
	# videoconvert !\
	# autovideosink


	# rtpvp8depay ! vp8dec !\
