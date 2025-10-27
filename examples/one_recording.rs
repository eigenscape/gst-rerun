use anyhow::{anyhow, Result};
use gst::prelude::*;
use std::time::Duration;

fn main() -> Result<()> {
    // Tracer needs to be set and registered before initiliazing gstreamer
    // Note: Not using spawn-viewer=true here, we'll set the recorder externally
    std::env::set_var(
        "GST_TRACERS",
        "reruntracing(debounce=200,spawn-viewer=false)",
    );
    gst_reruntracer::plugin_register_static()?;
    gst::init()?;
    gst_rerunsink::plugin_register_static()?;

    let rec = rerun::RecordingStreamBuilder::new("gst-rerun-demo").spawn()?;

    // Get reference to the RerunTracer that was automatically created and set the recorder
    let tracers = gst::active_tracers();
    for tracer in tracers {
        if let Some(rerun_tracer) = tracer.downcast_ref::<gst_reruntracer::RerunTracer>() {
            println!("Found RerunTracer: {}", tracer.name());
            rerun_tracer.set_recorder(rec.clone());
        }
    }

    // Rerun doesn't support bframes, tell the encoder to not emit any
    let pipeline_str = r#"
        videotestsrc pattern=ball num-buffers=100
            ! video/x-raw
            ! tee name=raw
            ! queue
            ! rerunsink entity-path=testsrc/raw

            raw.
            ! queue
            ! x264enc speed-preset=ultrafast tune=zerolatency b-adapt=false
            ! tee name=enc
            ! queue
            ! rerunsink entity-path=testsrc/encoded

            raw.
            ! queue
            ! videoconvert
            ! coloreffects preset=xpro
            ! videoconvert
            ! x264enc speed-preset=ultrafast tune=zerolatency b-adapt=false
            ! rerunsink entity-path=testsrc/filtered/encoded
    "#;
    // FIXME add a decodebin above: enc. ! queue ! h264parse ! decodebin ! fakesink
    let pipeline = gst::parse::launch(pipeline_str)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("Failed to downcast pipeline"))?;

    // Iterate through all rerunsink elements and set the shared recorder
    // Alternatively we could have set the same recording-id property on
    // all rerunsinks.
    for element in pipeline
        .iterate_all_by_element_factory_name("rerunsink")
        .into_iter()
        .flatten()
    {
        if let Some(sink) = element.downcast_ref::<gst_rerunsink::RerunSink>() {
            println!("Setting recorder on sink: {}", element.name());
            sink.set_recorder(rec.clone());
        }
    }

    pipeline.set_state(gst::State::Playing)?;

    // Wait for EOS or error
    println!("Running pipeline until EOS...");
    let bus = pipeline.bus().expect("Pipeline should have a bus");

    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;

        match msg.view() {
            MessageView::Eos(..) => {
                println!("Received EOS");
                break;
            }
            MessageView::Error(err) => {
                eprintln!(
                    "Error from {:?}: {} ({:?})",
                    err.src().map(|s| s.path_string()),
                    err.error(),
                    err.debug()
                );
                break;
            }
            _ => (),
        }
    }

    println!("Waiting a 5s...");
    std::thread::sleep(Duration::from_secs(5));

    pipeline.set_state(gst::State::Null)?;
    println!("Done");

    Ok(())
}
