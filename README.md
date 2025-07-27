<p align="center">
    <img src="https://github.com/simgt/glitch/actions/workflows/ci.yml/badge.svg?branch=main" />
</p>

# gst-rerun-tools

A set of plugins for gstreamer to forward data to [Rerun](https://rerun.io/) for real-time
visualization:

- A tracer to visualize the pipelines' graph structures
- A sink element to forward video frames

## Quick Start

```bash
cargo run --example one-recording
```

To use the tracer on a pipeline:

```bash
export GST_PLUGIN_PATH=$PWD/target/debug/
export GST_TRACERS="reruntracing(app-id=my_pipeline)"
gst-launch-1.0 videotestsrc ! identity ! rerunsink app-id=my_pipeline
```

With the command above, rerun will display two records. You can either
merge them in the viewer, or set `recording-id` to a fixed value on both
the tracer and sink, or spawn a single `rerun::RecordingStream`  as
done in the example.


## Development

### Upgrading GStreamer

The CI uses the tarballs in the repo to install gstreamer. If the version has changed,
use the Dockerfile to build them:

```bash
docker buildx build \
    --platform linux/arm64,linux/amd64 \
    --target=artifact \
    --output type=local,dest=$(pwd) .
```
